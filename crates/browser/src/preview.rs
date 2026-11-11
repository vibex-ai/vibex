//! A one-directory local file preview server for the embedded browser.
//!
//! A local HTML file cannot be loaded with `file://` — that scheme would let the
//! page read any file the runtime can, which is exactly the capability the
//! browser's scheme allowlist exists to withhold. Serving the same document as
//! a `data:` URL keeps that boundary, but a data URL has no usable origin of its
//! own: relative `<link>`, `<script>` and `<img>` references resolve to
//! nothing, so a real page arrives unstyled and scriptless.
//!
//! This server is the middle path, and only for previews a **human** opened.
//! It binds an ephemeral loopback port, and every URL it hands out carries a
//! capability only the runtime knows:
//!
//! * it serves exactly one directory: the previewed file's own directory;
//! * dotfiles and dot-directories are never served, so `.env`, `.git/` and
//!   `.ssh/` stay out of reach even though they sit beside the page;
//! * every request is canonicalized and must stay inside that directory, so a
//!   symbolic link cannot smuggle a request out of it;
//! * it answers `GET`/`HEAD` only, never lists a directory, and sends
//!   `Cache-Control: no-store` so a reload sees the file as it is on disk;
//! * it lives only as long as a tab shows one of its documents, and the caller
//!   drops it when the last such tab closes.
//!
//! The page therefore learns `http://127.0.0.1:<port>/<token>/<relative path>`:
//! an origin it can resolve assets against, and no absolute filesystem path at
//! all. Agent-initiated previews do **not** come through here — they keep the
//! per-file human approval and the `data:` URL, because one approved file must
//! not turn into a directory a page can read.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;

use crate::error::{BrowserError, BrowserResult};

/// Largest single file the preview server answers with.
pub const PREVIEW_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The document a directory request opens, the rule a static web server uses.
pub const PREVIEW_INDEX_FILE: &str = "index.html";

/// Prefix of the per-server capability token in the URL path.
pub const PREVIEW_TOKEN_PREFIX: &str = "lpv";

#[derive(Clone)]
struct PreviewState {
    root: Arc<PathBuf>,
    token: Arc<str>,
}

/// A running loopback server for one previewed directory.
///
/// [`LocalPreviewServer::bind`] is synchronous on purpose: it only reserves a
/// socket, so a caller knows the URL in the same turn it decides to open the
/// tab. [`LocalPreviewServer::serve`] is the part that needs a Tokio runtime,
/// and it is meant to be driven from a spawned task whose handle the caller
/// keeps — dropping that task is what stops the server.
pub struct LocalPreviewServer {
    address: SocketAddr,
    token: String,
    root: PathBuf,
    /// Taken exactly once, by [`LocalPreviewServer::serve`].
    listener: Mutex<Option<std::net::TcpListener>>,
}

impl std::fmt::Debug for LocalPreviewServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalPreviewServer")
            .field("address", &self.address)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl LocalPreviewServer {
    /// Reserves an ephemeral loopback port for `root`.
    ///
    /// `root` must already be a directory; the caller canonicalizes the
    /// previewed file, so its parent is the canonical directory the server is
    /// confined to.
    pub fn bind(root: PathBuf) -> BrowserResult<Self> {
        if !root.is_dir() {
            return Err(BrowserError::validation(
                "browser_preview_root_invalid",
                "a local preview needs an existing directory",
            ));
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
            BrowserError::process(
                "browser_preview_bind_failed",
                "the local preview server could not bind a loopback port",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            BrowserError::process(
                "browser_preview_bind_failed",
                "the local preview listener could not be made non-blocking",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let address = listener.local_addr().map_err(|error| {
            BrowserError::process(
                "browser_preview_bind_failed",
                "the local preview server could not read its address",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        // A v4 UUID is 122 random bits, minted per server: the port alone is not
        // a capability, so a page that guesses the port still cannot name a
        // document.
        let token = format!("{PREVIEW_TOKEN_PREFIX}_{}", uuid::Uuid::new_v4().simple());
        Ok(Self {
            address,
            token,
            root,
            listener: Mutex::new(Some(listener)),
        })
    }

    /// The directory this server is confined to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The address every URL of this server starts with.
    pub fn origin(&self) -> String {
        format!("http://{}/{}", self.address, self.token)
    }

    /// The URL that serves `file`, which must live inside this server's root.
    ///
    /// The relative path is percent-encoded segment by segment, so a file name
    /// with a space, `#` or non-ASCII character reaches the server as written.
    pub fn url_for(&self, file: &Path) -> Option<String> {
        let relative = file.strip_prefix(&self.root).ok()?;
        let mut url = url::Url::parse(&format!("http://{}/", self.address)).ok()?;
        {
            let mut segments = url.path_segments_mut().ok()?;
            segments.push(&self.token);
            for component in relative.components() {
                let Component::Normal(segment) = component else {
                    return None;
                };
                segments.push(&segment.to_string_lossy());
            }
        }
        Some(url.to_string())
    }

    /// Serves this directory until the calling task is dropped.
    ///
    /// Must run inside a Tokio runtime; it converts the reserved socket and
    /// hands it to axum. The future never returns on its own.
    pub async fn serve(&self) -> BrowserResult<()> {
        let listener = self.listener.lock().ok().and_then(|mut slot| slot.take());
        let Some(listener) = listener else {
            return Err(BrowserError::process(
                "browser_preview_already_serving",
                "the local preview server is already serving",
            ));
        };
        let listener = TcpListener::from_std(listener).map_err(|error| {
            BrowserError::process(
                "browser_preview_bind_failed",
                "the local preview listener could not join the runtime",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let state = PreviewState {
            root: Arc::new(self.root.clone()),
            token: Arc::from(self.token.as_str()),
        };
        let router = Router::new()
            .route("/{*path}", get(serve_document))
            .with_state(state);
        axum::serve(listener, router).await.map_err(|error| {
            BrowserError::process(
                "browser_preview_serve_failed",
                "the local preview server stopped",
            )
            .with_diagnostic("error", error.to_string())
        })
    }
}

/// Answers one preview request.
///
/// Every refusal looks the same from outside — a bare `404` — so a prober
/// cannot tell a wrong token from a missing file, and a traversal attempt
/// reveals nothing about the directory.
async fn serve_document(
    State(state): State<PreviewState>,
    UrlPath(path): UrlPath<String>,
) -> Response {
    let Some((token, relative)) = path.split_once('/') else {
        // A bare token names no document; `/` names nothing at all.
        return not_found();
    };
    if !constant_time_eq(token.as_bytes(), state.token.as_bytes()) {
        return not_found();
    }
    let Some(requested) = normalized_request(relative) else {
        return not_found();
    };
    let content_type = content_type_for(&requested);
    let root = Arc::clone(&state.root);
    let resolved = tokio::task::spawn_blocking(move || resolve_file(&root, &requested))
        .await
        .ok()
        .flatten();
    let Some(file) = resolved else {
        return not_found();
    };
    let Ok(metadata) = std::fs::metadata(&file) else {
        return not_found();
    };
    if metadata.len() > PREVIEW_MAX_FILE_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "preview file is too large").into_response();
    }
    let read = tokio::task::spawn_blocking(move || std::fs::read(&file))
        .await
        .ok()
        .and_then(Result::ok);
    let Some(bytes) = read else {
        return not_found();
    };
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// Normalizes a request path, refusing anything this server must not resolve.
///
/// Returns the relative path inside the root, with an empty or directory-shaped
/// request turned into the index document.
fn normalized_request(path: &str) -> Option<String> {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.ends_with('/') {
        return Some(format!("{path}{PREVIEW_INDEX_FILE}"));
    }
    let mut normalized = String::with_capacity(path.len());
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        // `..` would leave the root, and a leading dot names a file the preview
        // deliberately does not serve: `.env`, `.git/config`, `.ssh/`.
        if component == ".." || component.starts_with('.') || component.contains('\\') {
            return None;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    (!normalized.is_empty()).then_some(normalized)
}

/// Canonicalizes `relative` under `root` and confirms it stayed there.
fn resolve_file(root: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = root.join(relative);
    let resolved = candidate.canonicalize().ok()?;
    // The canonical path is what decides: a symbolic link inside the directory
    // that points outside it resolves outside and is refused.
    if !resolved.starts_with(root) {
        return None;
    }
    if resolved.is_dir() {
        let index = resolved.join(PREVIEW_INDEX_FILE);
        return index.is_file().then_some(index);
    }
    resolved.is_file().then_some(resolved)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// The content type a preview URL is served with.
///
/// The list is deliberately closed: a preview needs documents, styles, scripts,
/// images, fonts and media, and anything else is an opaque download rather than
/// something the browser may guess at.
fn content_type_for(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("txt" | "md" | "csv") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    /// Starts a server and keeps its task alive for the test.
    fn spawn_server(
        root: PathBuf,
    ) -> (
        Arc<LocalPreviewServer>,
        tokio::task::JoinHandle<BrowserResult<()>>,
    ) {
        let server = Arc::new(LocalPreviewServer::bind(root).expect("the server binds"));
        let serving = Arc::clone(&server);
        let task = tokio::spawn(async move { serving.serve().await });
        (server, task)
    }

    async fn body_of(url: &str) -> (StatusCode, String) {
        let response = reqwest::get(url).await.expect("the preview server answers");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        (status, body)
    }

    #[tokio::test]
    async fn a_preview_url_serves_the_file_and_its_assets() {
        let dir = temp_dir();
        std::fs::write(
            dir.path().join("index.html"),
            "<link rel=stylesheet href=site.css><h1>preview</h1>",
        )
        .unwrap();
        std::fs::write(dir.path().join("site.css"), "h1 { color: red }").unwrap();
        let (server, _task) = spawn_server(dir.path().to_path_buf());
        let url = server
            .url_for(&dir.path().join("index.html"))
            .expect("the file is inside the root");

        let (status, body) = body_of(&url).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("preview"));
        // The stylesheet is reachable through the same capability URL, which is
        // the whole reason a server exists rather than a `data:` URL.
        let (status, body) = body_of(&url.replace("index.html", "site.css")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "h1 { color: red }");
    }

    #[tokio::test]
    async fn a_directory_request_opens_its_index_document() {
        let dir = temp_dir();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs").join("index.html"), "index").unwrap();
        let (server, _task) = spawn_server(dir.path().to_path_buf());
        let (status, body) = body_of(&format!("{}/docs/", server.origin())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "index");
    }

    #[tokio::test]
    async fn a_file_name_with_spaces_and_unicode_is_served() {
        let dir = temp_dir();
        let name = "a b 测试.html";
        std::fs::write(dir.path().join(name), "unicode").unwrap();
        let (server, _task) = spawn_server(dir.path().to_path_buf());
        let url = server
            .url_for(&dir.path().join(name))
            .expect("the file is inside the root");
        assert!(url.contains("%20"), "spaces are encoded: {url}");
        let (status, body) = body_of(&url).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "unicode");
    }

    #[tokio::test]
    async fn the_server_refuses_traversal_dotfiles_and_wrong_tokens() {
        let dir = temp_dir();
        std::fs::write(dir.path().join("index.html"), "safe").unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
        let (server, _task) = spawn_server(dir.path().to_path_buf());

        let origin = server.origin();
        for path in [
            "%2e%2e/secret.txt",
            ".env",
            ".git/config",
            "missing.html",
            // A bare token names no document.
        ] {
            let (status, body) = body_of(&format!("{origin}/{path}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path} must not be served");
            assert!(!body.contains("SECRET"), "{path} leaked a dotfile");
        }
        let (status, _) = body_of(&origin).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = body_of(&format!("http://{}/", server.address)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "the root names nothing");
        let refused = format!("http://{}/lpv_wrong/index.html", server.address);
        let (status, _) = body_of(&refused).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a wrong token names nothing");
    }

    #[tokio::test]
    async fn a_symlink_that_leaves_the_root_is_refused() {
        #[cfg(unix)]
        {
            let dir = temp_dir();
            std::fs::write(dir.path().join("index.html"), "safe").unwrap();
            let outside = temp_dir();
            std::fs::write(outside.path().join("secret.txt"), "outside").unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                dir.path().join("link.txt"),
            )
            .unwrap();
            let (server, _task) = spawn_server(dir.path().to_path_buf());
            let (status, body) = body_of(&format!("{}/link.txt", server.origin())).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(!body.contains("outside"));
        }
    }

    #[tokio::test]
    async fn a_file_over_the_size_limit_is_refused() {
        let dir = temp_dir();
        std::fs::write(dir.path().join("index.html"), "ok").unwrap();
        let big = dir.path().join("big.bin");
        std::fs::File::create(&big)
            .and_then(|file| file.set_len(PREVIEW_MAX_FILE_BYTES + 1))
            .unwrap();
        let (server, _task) = spawn_server(dir.path().to_path_buf());
        let (status, _) = body_of(&format!("{}/big.bin", server.origin())).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn preview_requests_never_escape_the_root() {
        assert_eq!(
            normalized_request("docs/index.html"),
            Some("docs/index.html".to_string())
        );
        assert_eq!(normalized_request(""), Some("index.html".to_string()));
        assert_eq!(
            normalized_request("docs/"),
            Some("docs/index.html".to_string())
        );
        assert_eq!(normalized_request("../secret"), None);
        assert_eq!(normalized_request("a/../../secret"), None);
        assert_eq!(normalized_request(".env"), None);
        assert_eq!(normalized_request("docs/.git/config"), None);
        assert_eq!(normalized_request("C:\\Windows\\win.ini"), None);
    }

    #[test]
    fn preview_content_types_cover_documents_and_their_assets() {
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type_for("site.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_for("logo.svg"), "image/svg+xml");
        assert_eq!(content_type_for("font.woff2"), "font/woff2");
        assert_eq!(content_type_for("archive.bin"), "application/octet-stream");
    }

    #[test]
    fn a_token_only_url_names_no_document() {
        // `url_for` always appends a document; the token alone is not a page.
        assert_eq!(normalized_request(""), Some("index.html".to_string()));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}

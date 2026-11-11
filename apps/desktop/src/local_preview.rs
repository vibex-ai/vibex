//! Local pages the embedded browser renders.
//!
//! Double-clicking `index.html` should show the page, not its source. The panel
//! cannot load `file://` — the browser's scheme allowlist refuses it, and
//! rightly so — and a `data:` document has no origin, so the page's own
//! `<link>`, `<script>` and `<img>` references would resolve to nothing.
//!
//! [`LocalPreviewServer`] answers both: an ephemeral loopback port, one
//! directory, and a per-server token in the path. This module is the
//! workbench's side of that arrangement — it keeps one server per previewed
//! directory, hands out URLs, and stops a server once no tab renders anything
//! from it.
//!
//! Servers live in the desktop process, so this path is taken only when the
//! file and the browser are both on this machine. A paired remote runtime keeps
//! its browser on the other host, which a `127.0.0.1` URL here cannot reach;
//! such a workspace keeps opening pages in the editor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vibex_browser::preview::LocalPreviewServer;

/// Extensions the embedded browser renders as a document.
///
/// Deliberately closed, and deliberately only documents. Markdown, source,
/// images, PDFs and office files each have a surface of their own in the panel,
/// and that surface is the better answer. Media is the other near miss: Chrome
/// would play it, but a player seeks with range requests, and this server
/// answers whole files — so a video would arrive either unseekable or not at
/// all. Those keep their current external-only presentation.
pub const BROWSER_RENDERABLE_EXTENSIONS: &[&str] = &["html", "htm", "xhtml", "shtml"];

/// Whether a file is something the embedded browser renders itself.
pub fn renders_in_browser(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| BROWSER_RENDERABLE_EXTENSIONS.contains(&extension.as_str()))
}

/// The loopback preview servers this workbench currently owns.
#[derive(Default)]
pub struct LocalPreviews {
    /// Keyed by the canonical directory a server is confined to, so two pages
    /// in one directory share a server and each one's assets resolve.
    servers: BTreeMap<PathBuf, ServingPreview>,
}

impl std::fmt::Debug for LocalPreviews {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalPreviews")
            .field("directories", &self.servers.keys().collect::<Vec<_>>())
            .finish()
    }
}

struct ServingPreview {
    server: Arc<LocalPreviewServer>,
    /// Aborted on drop. Dropping a Tokio `JoinHandle` only detaches the task, so
    /// the abort is what actually stops a server whose last tab closed.
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ServingPreview {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LocalPreviews {
    /// How many servers are running. Tests use this to prove a closed tab does
    /// not leave a port behind.
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// The canonical directory a page is served from, when it can be read.
    ///
    /// The caller uses this both to keep the right server alive and to report
    /// a file that has gone missing.
    pub fn directory_of(file: &Path) -> Option<PathBuf> {
        file.canonicalize()
            .ok()
            .and_then(|canonical| canonical.parent().map(Path::to_path_buf))
    }

    /// Starts a server for `file`'s directory if there is none, and returns the
    /// capability URL that serves `file`.
    ///
    /// `file` may be relative; it is canonicalized first, so the server's root
    /// is the real directory the page lives in. The returned URL carries no
    /// filesystem path at all — only the token and the file's path *inside* the
    /// served directory.
    pub fn url_for(
        &mut self,
        file: &Path,
        runtime: &tokio::runtime::Handle,
    ) -> Result<String, String> {
        let canonical = file
            .canonicalize()
            .map_err(|error| format!("{}: {error}", file.display()))?;
        let root = Self::directory_of(&canonical)
            .ok_or_else(|| "the page is not inside a directory".to_string())?;
        if !self.servers.contains_key(&root) {
            let server = Arc::new(
                LocalPreviewServer::bind(root.clone())
                    .map_err(|error| format!("{}: {}", error.code, error.message))?,
            );
            let serving = Arc::clone(&server);
            let task = runtime.spawn(async move {
                // Only returns when the task is aborted or the socket fails;
                // either way there is nobody left to tell.
                let _ = serving.serve().await;
            });
            self.servers
                .insert(root.clone(), ServingPreview { server, task });
        }
        let server = self
            .servers
            .get(&root)
            .expect("the server was just ensured");
        server.server.url_for(&canonical).ok_or_else(|| {
            "the page could not be addressed inside the directory it lives in".to_string()
        })
    }

    /// Stops every server whose directory has no page left on screen.
    pub fn retain(&mut self, live_directories: &BTreeSet<PathBuf>) {
        self.servers
            .retain(|directory, _| live_directories.contains(directory));
    }

    /// Stops every server, for a workspace that is leaving the window.
    pub fn clear(&mut self) {
        self.servers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_documents_the_browser_can_render_go_to_the_browser() {
        assert!(renders_in_browser("index.html"));
        assert!(renders_in_browser("docs/Page.HTM"));
        assert!(renders_in_browser("site/page.xhtml"));
        // Files with a better native surface, and one Chrome would download.
        for path in [
            "src/lib.rs",
            "README.md",
            "logo.svg",
            "paper.pdf",
            "notes.txt",
            "data.json",
        ] {
            assert!(!renders_in_browser(path), "{path} is not a page");
        }
    }

    #[test]
    fn a_page_is_served_with_its_sibling_assets() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(
            directory.path().join("index.html"),
            "<link rel=stylesheet href=site.css><h1>hello</h1>",
        )
        .unwrap();
        std::fs::write(directory.path().join("site.css"), "h1 { color: red }").unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let mut previews = LocalPreviews::default();
        let page = directory.path().join("index.html");
        let url = previews
            .url_for(&page, runtime.handle())
            .expect("the page is served");
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        assert!(url.contains("/lpv_"), "{url}");
        assert!(
            !url.contains(directory.path().to_str().unwrap()),
            "the URL must not carry a filesystem path: {url}"
        );

        let (status, body) = runtime.block_on(async {
            let response = reqwest::get(&url).await.expect("the server answers");
            let status = response.status();
            (status, response.text().await.unwrap_or_default())
        });
        assert_eq!(status, reqwest::StatusCode::OK);
        assert!(body.contains("hello"));

        let stylesheet = url.replace("index.html", "site.css");
        let (status, body) = runtime.block_on(async {
            let response = reqwest::get(&stylesheet).await.expect("the server answers");
            let status = response.status();
            (status, response.text().await.unwrap_or_default())
        });
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body, "h1 { color: red }");

        // One server serves both pages of a directory.
        std::fs::write(directory.path().join("other.html"), "other").unwrap();
        previews
            .url_for(&directory.path().join("other.html"), runtime.handle())
            .expect("the sibling page is served");
        assert_eq!(previews.len(), 1);

        // The server stops with the last page that used it. The task is aborted
        // rather than dropped, so the socket closes on the runtime's next turn
        // instead of at this instant; the assertion gives it that turn.
        previews.retain(&BTreeSet::new());
        assert!(previews.is_empty());
        let stopped = runtime.block_on(async {
            for _ in 0..100 {
                match reqwest::get(&url).await {
                    Ok(response) if response.status().is_success() => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    _ => return true,
                }
            }
            false
        });
        assert!(stopped, "a stopped server must not keep serving");
    }

    #[test]
    fn a_missing_page_reports_the_path_it_could_not_read() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let mut previews = LocalPreviews::default();
        let error = previews
            .url_for(&directory.path().join("missing.html"), runtime.handle())
            .expect_err("a missing page cannot be served");
        assert!(error.contains("missing.html"), "{error}");
        assert!(previews.is_empty());
    }
}

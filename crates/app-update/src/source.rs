use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt as _;
use quick_xml::{Reader, XmlVersion, events::Event};
use reqwest::{Client, StatusCode, header};
use semver::Version;
use tokio::io::AsyncWriteExt as _;
use url::Url;

use crate::{AppUpdateError, AppUpdateResult, UpdateArtifact, UpdateChannel};

const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 4 * 1024;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
/// Wall-clock budget for one discovery document: the release feed, the manifest,
/// or its signature. These are small, so a total deadline is the right shape.
///
/// The artifact download deliberately has no total deadline. A package is tens
/// of megabytes and legitimately takes minutes on a slow link; capping it by
/// wall clock cancels a healthy transfer halfway.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for establishing a connection to GitHub.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for one body read that makes no progress. It resets on every read, so
/// it detects a stalled transfer without capping a progressing one.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const OFFICIAL_RELEASES_FEED: &str = "https://github.com/vibex-ai/vibex/releases.atom";
const RELEASE_NOTES_ASSET: &str = "vibex-release-notes.md";

pub type UpdateDownloadProgress = Arc<dyn Fn(u64, u64) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedManifest {
    pub tag: String,
    pub manifest: Vec<u8>,
    pub signature_base64: String,
}

#[async_trait]
pub trait UpdateSource: Send + Sync {
    async fn latest_signed_manifest(
        &self,
        channel: UpdateChannel,
    ) -> AppUpdateResult<Option<SignedManifest>>;

    /// Informational release notes published beside `tag`.
    ///
    /// `Ok(None)` means the release publishes no notes. Callers must treat any
    /// failure as "notes unavailable" and keep the update itself usable.
    async fn release_notes(&self, tag: &str) -> AppUpdateResult<Option<String>>;

    /// Streams `artifact` into `destination`, resuming from any usable bytes
    /// already present.
    ///
    /// Implementations should send a ranged request when `destination` holds a
    /// partial download, and either complete the file or fail without claiming
    /// the bytes on disk are complete. Callers verify the assembled file
    /// independently, so a resumed file is trusted only after that check.
    async fn download_artifact(
        &self,
        artifact: &UpdateArtifact,
        destination: &Path,
        progress: UpdateDownloadProgress,
    ) -> AppUpdateResult<()>;
}

#[derive(Default)]
struct FeedCache {
    etag: Option<String>,
    last_modified: Option<String>,
    bytes: Option<Vec<u8>>,
}

pub struct GitHubReleaseSource {
    client: Client,
    feed_url: Url,
    cache: tokio::sync::Mutex<FeedCache>,
}

impl GitHubReleaseSource {
    pub fn official() -> AppUpdateResult<Self> {
        Self::new(Url::parse(OFFICIAL_RELEASES_FEED).expect("official update feed URL is valid"))
    }

    pub fn new(feed_url: Url) -> AppUpdateResult<Self> {
        if feed_url.scheme() != "https"
            || feed_url.host_str() != Some("github.com")
            || feed_url.username() != ""
            || feed_url.password().is_some()
            || feed_url.query().is_some()
            || feed_url.fragment().is_some()
        {
            return Err(AppUpdateError::new(
                "app_update_feed_url_invalid",
                "the update feed URL is invalid",
            ));
        }
        let client = update_http_client()?;
        Ok(Self {
            client,
            feed_url,
            cache: tokio::sync::Mutex::new(FeedCache::default()),
        })
    }

    async fn feed_bytes(&self) -> AppUpdateResult<Vec<u8>> {
        let (etag, last_modified) = {
            let cache = self.cache.lock().await;
            (cache.etag.clone(), cache.last_modified.clone())
        };
        let mut request = self.client.get(self.feed_url.clone());
        if let Some(etag) = etag {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header(header::IF_MODIFIED_SINCE, last_modified);
        }
        let response = request.send().await.map_err(network_error)?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return self.cache.lock().await.bytes.clone().ok_or_else(|| {
                AppUpdateError::retryable(
                    "app_update_feed_cache_missing",
                    "the update feed cache is unavailable",
                )
            });
        }
        let response = require_success(response, "app_update_feed_request_failed")?;
        let response_etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let response_last_modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = bounded_response(response, MAX_FEED_BYTES, "app_update_feed_too_large").await?;
        let mut cache = self.cache.lock().await;
        cache.etag = response_etag;
        cache.last_modified = response_last_modified;
        cache.bytes = Some(bytes.clone());
        Ok(bytes)
    }

    async fn release_asset(&self, tag: &str, name: &str, limit: usize) -> AppUpdateResult<Vec<u8>> {
        validate_release_tag(tag)?;
        let url = Url::parse(&format!(
            "https://github.com/vibex-ai/vibex/releases/download/{tag}/{name}"
        ))
        .expect("release asset URL is valid");
        let response = self.client.get(url).send().await.map_err(network_error)?;
        let response = require_success(response, "app_update_manifest_request_failed")?;
        bounded_response(response, limit, "app_update_manifest_too_large").await
    }
}

#[async_trait]
impl UpdateSource for GitHubReleaseSource {
    async fn latest_signed_manifest(
        &self,
        channel: UpdateChannel,
    ) -> AppUpdateResult<Option<SignedManifest>> {
        let feed = under_deadline(DISCOVERY_TIMEOUT, self.feed_bytes()).await?;
        let Some((tag, _)) = latest_channel_release(&feed, channel)? else {
            return Ok(None);
        };
        let manifest = under_deadline(
            DISCOVERY_TIMEOUT,
            self.release_asset(&tag, "vibex-update.json", MAX_MANIFEST_BYTES),
        )
        .await?;
        let signature = under_deadline(
            DISCOVERY_TIMEOUT,
            self.release_asset(&tag, "vibex-update.json.sig", MAX_SIGNATURE_BYTES),
        )
        .await?;
        let signature_base64 = String::from_utf8(signature).map_err(|_| {
            AppUpdateError::new(
                "app_update_signature_invalid",
                "the update manifest signature is invalid",
            )
        })?;
        Ok(Some(SignedManifest {
            tag,
            manifest,
            signature_base64,
        }))
    }

    async fn release_notes(&self, tag: &str) -> AppUpdateResult<Option<String>> {
        validate_release_tag(tag)?;
        let url = Url::parse(&format!(
            "https://github.com/vibex-ai/vibex/releases/download/{tag}/{RELEASE_NOTES_ASSET}"
        ))
        .expect("release notes URL is valid");
        let response = self.client.get(url).send().await.map_err(network_error)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = require_success(response, "app_update_release_notes_request_failed")?;
        let bytes = bounded_response(
            response,
            crate::MAX_RELEASE_NOTES_BYTES,
            "app_update_release_notes_too_large",
        )
        .await?;
        String::from_utf8(bytes).map(Some).map_err(|_| {
            AppUpdateError::new(
                "app_update_release_notes_invalid",
                "the release notes are not valid UTF-8",
            )
        })
    }

    async fn download_artifact(
        &self,
        artifact: &UpdateArtifact,
        destination: &Path,
        progress: UpdateDownloadProgress,
    ) -> AppUpdateResult<()> {
        // A failed attempt may have left bytes behind. Resume from them when the
        // server can serve a range; the signed size and hash checks at the end
        // still decide whether the assembled file is trustworthy.
        let mut resumed = resumable_offset(destination, artifact.size).await;
        let mut request = self
            .client
            .get(artifact.url.clone())
            .header(header::ACCEPT_ENCODING, "identity");
        if resumed > 0 {
            request = request.header(header::RANGE, format!("bytes={resumed}-"));
        }
        let mut response = request.send().await.map_err(network_error)?;
        if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            // The server cannot serve the bytes already on disk. Refetch the
            // whole artifact instead of reporting a mismatch for a good resume.
            resumed = 0;
            response = self
                .client
                .get(artifact.url.clone())
                .header(header::ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(network_error)?;
        }
        let response = require_success(response, "app_update_download_failed")?;
        let mut already_downloaded = resumed;
        if response.status() == StatusCode::PARTIAL_CONTENT {
            if content_range_start(response.headers()) != Some(resumed) {
                return Err(AppUpdateError::new(
                    "app_update_download_range_mismatch",
                    "the update server resumed the download at the wrong position",
                ));
            }
        } else if response.status() == StatusCode::OK {
            // The server ignored the range request: start the artifact over
            // rather than appending a full copy to the bytes already stored.
            already_downloaded = 0;
        }
        if response
            .content_length()
            .is_some_and(|length| length != artifact.size - already_downloaded)
        {
            return Err(AppUpdateError::new(
                "app_update_download_size_mismatch",
                "the update download size does not match the signed manifest",
            ));
        }
        if already_downloaded == 0 {
            let _ = tokio::fs::remove_file(destination).await;
        }
        let mut file = if already_downloaded > 0 {
            tokio::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(destination)
                .await
        } else {
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .await
        }
        .map_err(|_| {
            AppUpdateError::new(
                "app_update_download_file_create_failed",
                "the update download file could not be created",
            )
        })?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(network_error)?;
            let next = already_downloaded
                .checked_add(chunk.len() as u64)
                .filter(|total| *total <= artifact.size)
                .ok_or_else(|| {
                    AppUpdateError::new(
                        "app_update_download_size_mismatch",
                        "the update download exceeded the signed size",
                    )
                })?;
            file.write_all(&chunk).await.map_err(|_| {
                AppUpdateError::new(
                    "app_update_download_write_failed",
                    "the update download could not be written",
                )
            })?;
            already_downloaded = next;
            progress(already_downloaded, artifact.size);
        }
        if already_downloaded != artifact.size {
            return Err(AppUpdateError::retryable(
                "app_update_download_incomplete",
                "the update download ended before it was complete",
            ));
        }
        file.sync_all().await.map_err(|_| {
            AppUpdateError::new(
                "app_update_download_sync_failed",
                "the update download could not be committed to disk",
            )
        })?;
        Ok(())
    }
}

/// Builds the HTTPS client every update request shares.
///
/// The client carries a connect budget and a per-read stall budget, but no total
/// deadline. A total deadline covers the response body as well, so it would cut
/// off an artifact download that is transferring fine — a 100 MiB package can
/// take longer than any wall-clock limit on a slow link. Discovery documents
/// get their wall-clock bound at the call sites instead.
///
/// Android's platform verifier can reject otherwise valid public chains when a
/// leaf omits an OCSP responder, so the Android build pins the bundled Mozilla
/// WebPKI store — the same decision the remote client makes for its own HTTP
/// traffic.
fn update_http_client() -> AppUpdateResult<Client> {
    let builder = Client::builder()
        .user_agent(format!("Vibex/{} updater", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many update redirects");
            }
            if redirect_host_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("update redirect left the trusted GitHub boundary")
            }
        }));
    #[cfg(target_os = "android")]
    let builder = {
        let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .map(|certificate| {
                reqwest::Certificate::from_der(certificate.as_ref()).map_err(|_| {
                    AppUpdateError::new(
                        "app_update_tls_root_certificate_invalid",
                        "the bundled update TLS root certificate is invalid",
                    )
                })
            })
            .collect::<AppUpdateResult<Vec<_>>>()?;
        builder.tls_certs_only(roots)
    };
    builder.build().map_err(|_| {
        AppUpdateError::new(
            "app_update_http_client_unavailable",
            "the update network client could not be initialized",
        )
    })
}

fn latest_channel_release(
    feed: &[u8],
    channel: UpdateChannel,
) -> AppUpdateResult<Option<(String, Version)>> {
    let mut reader = Reader::from_reader(feed);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_entry = false;
    let mut entry_link = None;
    let mut releases = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) if event.name().as_ref() == b"entry" => {
                in_entry = true;
                entry_link = None;
            }
            Ok(Event::Empty(event)) if in_entry && event.name().as_ref() == b"link" => {
                for attribute in event.attributes().flatten() {
                    if attribute.key.as_ref() == b"href"
                        && let Ok(value) = attribute
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                    {
                        entry_link = Some(value.into_owned());
                    }
                }
            }
            Ok(Event::End(event)) if event.name().as_ref() == b"entry" => {
                if let Some(link) = entry_link.take()
                    && let Some((tag, version)) = release_from_link(&link)
                    && channel.accepts(&version)
                {
                    releases.push((tag, version));
                }
                in_entry = false;
            }
            Ok(Event::Eof) => break,
            Err(_) => {
                return Err(AppUpdateError::new(
                    "app_update_feed_invalid",
                    "the GitHub Releases feed is invalid",
                ));
            }
            _ => {}
        }
        buffer.clear();
    }
    releases.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(releases.pop())
}

/// Reject tags that could escape the release download path they are
/// interpolated into. The feed already constrains tags; this keeps every asset
/// URL builder honest on its own.
fn validate_release_tag(tag: &str) -> AppUpdateResult<()> {
    if tag.is_empty()
        || tag.len() > 120
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(AppUpdateError::new(
            "app_update_release_tag_invalid",
            "the release tag is invalid",
        ));
    }
    Ok(())
}

fn release_from_link(link: &str) -> Option<(String, Version)> {
    let url = Url::parse(link).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let prefix = "/vibex-ai/vibex/releases/tag/";
    let tag = url.path().strip_prefix(prefix)?;
    if tag.is_empty() || tag.contains('/') {
        return None;
    }
    let version = Version::parse(tag.strip_prefix('v')?).ok()?;
    Some((tag.to_string(), version))
}

fn redirect_host_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
                    | "github-releases.githubusercontent.com"
            )
        )
}

/// Runs one discovery request under a wall-clock deadline.
///
/// The shared client intentionally has no total timeout, so the small feed,
/// manifest, and signature requests are bounded here instead. A stalled server
/// still fails the check promptly rather than holding the operation open.
async fn under_deadline<T>(
    deadline: Duration,
    future: impl std::future::Future<Output = AppUpdateResult<T>>,
) -> AppUpdateResult<T> {
    match tokio::time::timeout(deadline, future).await {
        Ok(result) => result,
        Err(_) => Err(AppUpdateError::retryable(
            "app_update_network_failed",
            "the update service did not answer in time",
        )),
    }
}

/// Bytes already on disk that a ranged request can resume from.
///
/// A missing, empty, or complete file, or one larger than the signed artifact,
/// restarts from zero.
pub(crate) async fn resumable_offset(destination: &Path, size: u64) -> u64 {
    match tokio::fs::metadata(destination).await {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 && metadata.len() < size => {
            metadata.len()
        }
        _ => 0,
    }
}

/// The first byte a `206 Partial Content` response carries, parsed from
/// `Content-Range: bytes <start>-<end>/<total>`.
fn content_range_start(headers: &header::HeaderMap) -> Option<u64> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (unit, range) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    range.split('-').next()?.trim().parse().ok()
}

fn require_success(
    response: reqwest::Response,
    code: &'static str,
) -> AppUpdateResult<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let retryable =
        response.status() == StatusCode::TOO_MANY_REQUESTS || response.status().is_server_error();
    Err(if retryable {
        AppUpdateError::retryable(code, "GitHub could not serve the update request")
    } else {
        AppUpdateError::new(code, "GitHub rejected the update request")
    })
}

async fn bounded_response(
    response: reqwest::Response,
    limit: usize,
    code: &'static str,
) -> AppUpdateResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(AppUpdateError::new(
            code,
            "the update response is too large",
        ));
    }
    let bytes = response.bytes().await.map_err(network_error)?;
    if bytes.len() > limit {
        return Err(AppUpdateError::new(
            code,
            "the update response is too large",
        ));
    }
    Ok(bytes.to_vec())
}

fn network_error(error: reqwest::Error) -> AppUpdateError {
    if error.is_timeout() {
        return AppUpdateError::retryable(
            "app_update_network_failed",
            "the update request timed out",
        );
    }
    let retryable = error.is_connect() || error.is_request() || error.is_body();
    if retryable {
        AppUpdateError::retryable(
            "app_update_network_failed",
            "the update service could not be reached",
        )
    } else {
        AppUpdateError::new(
            "app_update_network_failed",
            "the update request could not be completed",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn release_tags_cannot_escape_the_release_download_path() {
        assert!(validate_release_tag("v0.2.0").is_ok());
        assert!(validate_release_tag("v0.2.0-rc.1").is_ok());
        for tag in ["", "../v0.2.0", "v0.2.0/extra", "v0.2.0?x=1", "v0.2.0#frag"] {
            assert_eq!(
                validate_release_tag(tag).unwrap_err().code,
                "app_update_release_tag_invalid",
                "{tag} should be rejected"
            );
        }
    }

    #[test]
    fn atom_feed_selects_only_the_requested_channel() {
        let feed = br#"<?xml version="1.0" encoding="UTF-8"?>
          <feed xmlns="http://www.w3.org/2005/Atom">
            <entry><link href="https://github.com/vibex-ai/vibex/releases/tag/v0.3.0-preview.2" /></entry>
            <entry><link href="https://github.com/vibex-ai/vibex/releases/tag/v0.2.0" /></entry>
            <entry><link href="https://github.com/vibex-ai/vibex/releases/tag/v0.3.0-rc.1" /></entry>
            <entry><link href="https://example.com/vibex/releases/tag/v9.0.0" /></entry>
          </feed>"#;
        assert_eq!(
            latest_channel_release(feed, UpdateChannel::Stable)
                .unwrap()
                .unwrap()
                .0,
            "v0.2.0"
        );
        assert_eq!(
            latest_channel_release(feed, UpdateChannel::Rc)
                .unwrap()
                .unwrap()
                .0,
            "v0.3.0-rc.1"
        );
        assert_eq!(
            latest_channel_release(feed, UpdateChannel::Preview)
                .unwrap()
                .unwrap()
                .0,
            "v0.3.0-preview.2"
        );
    }

    #[test]
    fn content_range_parsing_reads_the_first_byte() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_RANGE,
            "bytes 2048-4095/8192".parse().unwrap(),
        );
        assert_eq!(content_range_start(&headers), Some(2048));

        assert_eq!(content_range_start(&header::HeaderMap::new()), None);
    }

    #[tokio::test]
    async fn under_deadline_rejects_a_stalled_discovery_request() {
        let outcome = under_deadline(Duration::from_millis(20), async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<(), AppUpdateError>(())
        })
        .await;
        let error = outcome.unwrap_err();
        assert_eq!(error.code, "app_update_network_failed");
        assert!(error.retryable);
    }

    /// Reads one HTTP request's headers from a freshly accepted socket.
    async fn read_request_headers(socket: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt as _;
        let mut buffer = Vec::new();
        let mut byte = [0u8; 1];
        while !buffer.ends_with(b"\r\n\r\n") {
            if socket.read(&mut byte).await.unwrap() == 0 {
                break;
            }
            buffer.push(byte[0]);
        }
        String::from_utf8_lossy(&buffer).to_string()
    }

    fn test_artifact(url: Url, size: u64) -> UpdateArtifact {
        UpdateArtifact {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            package: "deb".to_string(),
            install_mode: crate::InstallMode::SystemInstaller,
            url,
            size,
            sha256: String::new(),
            sha512: None,
        }
    }

    async fn download_from(
        source: &GitHubReleaseSource,
        artifact: &UpdateArtifact,
        destination: &Path,
    ) -> AppUpdateResult<()> {
        source
            .download_artifact(artifact, destination, Arc::new(|_, _| {}))
            .await
    }

    #[tokio::test]
    async fn download_artifact_resumes_from_bytes_already_on_disk() {
        use tokio::io::AsyncWriteExt as _;
        use tokio::net::TcpListener;

        let body: Vec<u8> = (0..48 * 1024u32).map(|index| (index % 256) as u8).collect();
        let total = body.len() as u64;
        let resumed = 20 * 1024u64;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_range = format!("bytes={resumed}-");
        let server_body = body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request_headers(&mut socket).await;
            assert!(
                request.contains(&expected_range),
                "the resume attempt should ask for the missing bytes: {request}"
            );
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {resumed}-{}/{total}\r\nContent-Length: {}\r\n\r\n",
                total - 1,
                total - resumed,
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket
                .write_all(&server_body[resumed as usize..])
                .await
                .unwrap();
        });

        let directory = tempdir().unwrap();
        let destination = directory.path().join("artifact.deb");
        tokio::fs::write(&destination, &body[..resumed as usize])
            .await
            .unwrap();

        let source = GitHubReleaseSource::official().unwrap();
        let artifact = test_artifact(
            Url::parse(&format!("http://{addr}/artifact.deb")).unwrap(),
            total,
        );
        download_from(&source, &artifact, &destination)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), body);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_artifact_starts_over_when_the_server_ignores_the_range() {
        use tokio::io::AsyncWriteExt as _;
        use tokio::net::TcpListener;

        let body: Vec<u8> = (0..16 * 1024u32).map(|index| (index % 256) as u8).collect();
        let total = body.len() as u64;
        let resumed = 4 * 1024u64;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_body = body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request_headers(&mut socket).await;
            assert!(request.contains(&format!("bytes={resumed}-")), "{request}");
            // The server answers with the whole artifact instead of a slice.
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n\r\n");
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&server_body).await.unwrap();
        });

        let directory = tempdir().unwrap();
        let destination = directory.path().join("artifact.deb");
        tokio::fs::write(&destination, &body[..resumed as usize])
            .await
            .unwrap();

        let source = GitHubReleaseSource::official().unwrap();
        let artifact = test_artifact(
            Url::parse(&format!("http://{addr}/artifact.deb")).unwrap(),
            total,
        );
        download_from(&source, &artifact, &destination)
            .await
            .unwrap();
        // The full artifact replaced the stale prefix instead of being appended
        // to it.
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), body);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_artifact_survives_a_slow_but_progressing_body() {
        use tokio::io::AsyncWriteExt as _;
        use tokio::net::TcpListener;

        // The body arrives in small chunks with pauses between them. A total
        // request deadline would cancel a healthy transfer like this; only a
        // stalled read should fail.
        let body: Vec<u8> = (0..64 * 1024u32).map(|index| (index % 256) as u8).collect();
        let chunks = 8usize;
        let chunk = body.len() / chunks;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_body = body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request_headers(&mut socket).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                server_body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            for index in 0..chunks {
                let start = index * chunk;
                let end = if index == chunks - 1 {
                    server_body.len()
                } else {
                    start + chunk
                };
                socket.write_all(&server_body[start..end]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });

        let directory = tempdir().unwrap();
        let destination = directory.path().join("artifact.deb");
        let source = GitHubReleaseSource::official().unwrap();
        let artifact = test_artifact(
            Url::parse(&format!("http://{addr}/artifact.deb")).unwrap(),
            body.len() as u64,
        );
        download_from(&source, &artifact, &destination)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), body);
        server.await.unwrap();
    }
}

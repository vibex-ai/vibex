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
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const OFFICIAL_RELEASES_FEED: &str = "https://github.com/vibex-ai/vibex/releases.atom";

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
        let client = Client::builder()
            .user_agent(format!("Vibex/{} updater", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many update redirects");
                }
                if redirect_host_allowed(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("update redirect left the trusted GitHub boundary")
                }
            }))
            .build()
            .map_err(|_| {
                AppUpdateError::new(
                    "app_update_http_client_unavailable",
                    "the update network client could not be initialized",
                )
            })?;
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
        let feed = self.feed_bytes().await?;
        let Some((tag, _)) = latest_channel_release(&feed, channel)? else {
            return Ok(None);
        };
        let manifest = self
            .release_asset(&tag, "vibex-update.json", MAX_MANIFEST_BYTES)
            .await?;
        let signature = self
            .release_asset(&tag, "vibex-update.json.sig", MAX_SIGNATURE_BYTES)
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

    async fn download_artifact(
        &self,
        artifact: &UpdateArtifact,
        destination: &Path,
        progress: UpdateDownloadProgress,
    ) -> AppUpdateResult<()> {
        let response = self
            .client
            .get(artifact.url.clone())
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(network_error)?;
        let response = require_success(response, "app_update_download_failed")?;
        if response
            .content_length()
            .is_some_and(|length| length != artifact.size)
        {
            return Err(AppUpdateError::new(
                "app_update_download_size_mismatch",
                "the update download size does not match the signed manifest",
            ));
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .await
            .map_err(|_| {
                AppUpdateError::new(
                    "app_update_download_file_create_failed",
                    "the update download file could not be created",
                )
            })?;
        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(network_error)?;
            downloaded = downloaded
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
            progress(downloaded, artifact.size);
        }
        if downloaded != artifact.size {
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
    let retryable =
        error.is_timeout() || error.is_connect() || error.is_request() || error.is_body();
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
}

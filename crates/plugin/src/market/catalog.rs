//! The market catalog: fetch both sources, merge, cache and serve.
//!
//! [`MarketCatalog::load`] is the one entry point the runtime uses. It prefers
//! what it cached, refreshes when stale, and — when nothing can be reached —
//! serves the last catalog with `stale` set and a reason, never an empty list
//! passed off as "there are none".

use std::time::Duration;

use ed25519_dalek::VerifyingKey;
use url::Url;
use vibex_core::{
    PluginMarketDetail, PluginMarketIndexRequest, PluginMarketIndexResponse, PluginMarketVersion,
};

use crate::error::PluginError;

use super::cache::MarketCache;
use super::fetch::MarketHttpClient;
use super::github::GithubTopicSource;
use super::index::{
    MAX_INDEX_BYTES, PluginIndexEntry, PluginIndexFile, PluginRevocations, encode_verifying_key,
    verify_signature,
};
use super::merge::merge_entries;

/// How long a merged listing stands before it is refreshed.
pub const INDEX_TTL: Duration = Duration::from_secs(30 * 60);
/// How long a listing may be served after the index stopped updating.
pub const INDEX_MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);
/// How long topic discovery stands before GitHub is asked again.
pub const GITHUB_TTL: Duration = Duration::from_secs(10 * 60);
/// The largest signature document the market reads.
const MAX_SIGNATURE_BYTES: u64 = 8 * 1024;

/// Everything the catalog needs to run.
#[derive(Debug, Clone)]
pub struct MarketConfig {
    /// The index addresses, tried in order.
    pub index_urls: Vec<Url>,
    /// The root key index signatures are verified against.
    pub root_public_key: Option<VerifyingKey>,
    /// Topic discovery, when enabled.
    pub github: Option<GithubTopicSource>,
    /// The cache directory.
    pub cache: MarketCache,
    /// How long a merged listing stands.
    pub index_ttl: Duration,
    /// How long topic discovery stands.
    pub github_ttl: Duration,
    /// Whether an index may be read without a verified signature.
    ///
    /// Off in a shipped build; development hosts may turn it on.
    pub allow_unverified_index: bool,
}

impl MarketConfig {
    /// A config with no sources, for tests and for a build without a market.
    pub fn empty(cache: MarketCache) -> Self {
        Self {
            index_urls: Vec::new(),
            root_public_key: None,
            github: None,
            cache,
            index_ttl: INDEX_TTL,
            github_ttl: GITHUB_TTL,
            allow_unverified_index: false,
        }
    }
}

/// The merged market catalog.
#[derive(Debug, Clone)]
pub struct MarketCatalog {
    client: MarketHttpClient,
    config: MarketConfig,
}

impl MarketCatalog {
    pub fn new(client: MarketHttpClient, config: MarketConfig) -> Self {
        Self { client, config }
    }

    pub fn config(&self) -> &MarketConfig {
        &self.config
    }

    /// Loads the merged catalog, using and refreshing the cache as needed.
    pub async fn load(&self, request: &PluginMarketIndexRequest) -> PluginMarketIndexResponse {
        let cached = self.config.cache.read_response();
        let fresh = self
            .config
            .cache
            .merged_age()
            .is_some_and(|age| age < self.config.index_ttl);
        if !request.refresh
            && fresh
            && let Some(mut response) = cached.clone()
        {
            response.stale = false;
            response.partial_reason = None;
            return response;
        }

        let mut partial_reason: Option<String> = None;

        let (index_entries, generated_at, revision) = match self.fetch_index().await {
            Ok(index) => {
                let generated_at = Some(index.generated_at.clone());
                let revision = index.revision;
                (index.plugins, generated_at, revision)
            }
            Err(error) => {
                partial_reason = Some(format!("the index could not be read: {error}"));
                (Vec::new(), None, 0)
            }
        };

        let github_entries = self.load_github(request, &mut partial_reason).await;

        let merged = merge_entries(index_entries, github_entries);
        if !merged.is_empty() {
            let response = PluginMarketIndexResponse {
                entries: merged.iter().map(|entry| entry.entry.clone()).collect(),
                generated_at,
                revision,
                stale: false,
                partial_reason,
            };
            let _ = self.config.cache.write_entries(&merged);
            let _ = self.config.cache.write_response(&response);
            return response;
        }

        if let Some(mut cached) = cached {
            cached.stale = true;
            cached.partial_reason = Some(
                partial_reason.unwrap_or_else(|| "the market could not be reached".to_string()),
            );
            return cached;
        }

        PluginMarketIndexResponse {
            entries: Vec::new(),
            generated_at,
            revision,
            stale: true,
            partial_reason: Some(
                partial_reason.unwrap_or_else(|| PluginError::NoCatalog.to_string()),
            ),
        }
    }

    async fn load_github(
        &self,
        request: &PluginMarketIndexRequest,
        partial_reason: &mut Option<String>,
    ) -> Vec<PluginIndexEntry> {
        let Some(github) = &self.config.github else {
            return self.config.cache.read_github_entries().unwrap_or_default();
        };
        if !request.include_github {
            return self.config.cache.read_github_entries().unwrap_or_default();
        }
        let fresh = self
            .config
            .cache
            .github_age()
            .is_some_and(|age| age < self.config.github_ttl);
        if !request.refresh && fresh {
            return self.config.cache.read_github_entries().unwrap_or_default();
        }
        match github.discover().await {
            Ok(entries) => {
                let _ = self.config.cache.write_github_entries(&entries);
                entries
            }
            Err(error) => {
                let reason = format!("topic discovery could not be read: {error}");
                *partial_reason = Some(match partial_reason.take() {
                    Some(existing) => format!("{existing}; {reason}"),
                    None => reason,
                });
                self.config.cache.read_github_entries().unwrap_or_default()
            }
        }
    }

    async fn fetch_index(&self) -> Result<PluginIndexFile, PluginError> {
        let mut last_error = None;
        for url in &self.config.index_urls {
            match self.fetch_index_from(url).await {
                Ok(index) => return Ok(index),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or(PluginError::NoCatalog))
    }

    async fn fetch_index_from(&self, url: &Url) -> Result<PluginIndexFile, PluginError> {
        let raw = self.client.fetch_bytes(url, MAX_INDEX_BYTES).await?;
        let signature_url = signature_url(url)?;
        let signature_bytes = self
            .client
            .fetch_bytes(&signature_url, MAX_SIGNATURE_BYTES)
            .await?;
        let signature = String::from_utf8_lossy(&signature_bytes).trim().to_string();
        match &self.config.root_public_key {
            Some(key) => {
                let public_key = encode_verifying_key(key);
                verify_signature(&raw, &signature, &public_key)?;
            }
            None => {
                if !self.config.allow_unverified_index {
                    return Err(PluginError::IndexSignatureInvalid);
                }
            }
        }
        let index = PluginIndexFile::parse(&raw)?;
        let _ = self.config.cache.write_index(&raw, &signature);
        let _ = self
            .config
            .cache
            .write_publisher_keys(&index.publisher_keys);
        Ok(index)
    }

    /// Builds one plugin's detail page from the cached catalog.
    pub async fn detail(
        &self,
        plugin_id: &str,
        version: Option<&str>,
    ) -> Result<PluginMarketDetail, PluginError> {
        let entries = self.config.cache.read_entries().unwrap_or_default();
        let entry = entries
            .into_iter()
            .find(|entry| entry.entry.id == plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        // A reviewed entry carries its own page text; only a topic-discovered
        // one has to fetch a README, because its repository is all there is.
        let mut readme = entry.readme.clone();
        if readme.is_none()
            && entry.entry.listing_source == vibex_core::PluginListingSource::Github
            && let Some(github) = &self.config.github
            && let Some(repo) = repository_spec(&entry)
        {
            readme = github
                .read_readme(&repo, vibex_core::MAX_PLUGIN_README_BYTES)
                .await
                .ok()
                .flatten();
        }
        let readme_bytes = readme.as_ref().map(|text| text.len() as u64).unwrap_or(0);
        let requested_permissions = requested_permissions(&entry.versions);
        let versions = match version {
            Some(version) => entry
                .versions
                .iter()
                .filter(|candidate| candidate.version == version)
                .cloned()
                .collect(),
            None => entry.versions.clone(),
        };

        Ok(PluginMarketDetail {
            entry: entry.entry.clone(),
            description: entry
                .description
                .clone()
                .or_else(|| entry.entry.summary.clone()),
            readme,
            readme_bytes,
            screenshots: entry.screenshots.clone(),
            homepage: entry.entry.homepage.clone(),
            repository: entry.entry.repository.clone(),
            license: entry.entry.license.clone(),
            versions,
            reviews: entry.reviews.clone(),
            requested_permissions,
        })
    }

    /// The cached revocation list.
    pub fn revocations(&self) -> Option<PluginRevocations> {
        self.config.cache.read_revocations()
    }

    /// The revocation blocking a plugin version, if any.
    pub fn blocked(&self, plugin_id: &str, version: &str) -> Option<super::index::RevokedPlugin> {
        self.revocations()
            .and_then(|revocations| revocations.blocking(plugin_id, version).cloned())
    }

    pub fn client(&self) -> &MarketHttpClient {
        &self.client
    }
}

/// The union of every version's permissions, for the detail page.
fn requested_permissions(versions: &[PluginMarketVersion]) -> Vec<vibex_core::PluginPermission> {
    let mut permissions: Vec<vibex_core::PluginPermission> = Vec::new();
    for version in versions {
        for permission in &version.permissions {
            if !permissions
                .iter()
                .any(|existing| existing.key == permission.key)
            {
                permissions.push(permission.clone());
            }
        }
    }
    permissions.sort_by(|left, right| left.key.cmp(&right.key));
    permissions
}

/// The `owner/repo` a topic-discovered entry came from.
fn repository_spec(entry: &PluginIndexEntry) -> Option<String> {
    for version in &entry.versions {
        if let Some(spec) = version.url.strip_prefix("github:") {
            return Some(spec.to_string());
        }
    }
    entry
        .entry
        .repository
        .as_deref()
        .and_then(|url| url.strip_prefix("https://github.com/"))
        .map(str::to_string)
}

fn signature_url(url: &Url) -> Result<Url, PluginError> {
    let mut signature = url.clone();
    signature.set_path(&format!("{}.sig", url.path()));
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_url_appends_to_the_path() {
        let url = Url::parse("https://plugins.example.com/dist/index.json").unwrap();
        assert_eq!(
            signature_url(&url).unwrap().as_str(),
            "https://plugins.example.com/dist/index.json.sig"
        );
    }

    #[test]
    fn requested_permissions_are_the_union() {
        let permission = |key: &str| vibex_core::PluginPermission::new(key, Default::default());
        let versions = vec![
            PluginMarketVersion {
                version: "1.0.0".to_string(),
                published_at: None,
                size_bytes: None,
                sha256: "a".repeat(64),
                sha512: None,
                url: "https://example.com/a".to_string(),
                changelog: None,
                min_vibex: None,
                min_shell: None,
                permissions: vec![permission("ui.panel"), permission("storage")],
                platforms: Vec::new(),
                yanked: false,
                yanked_reason: None,
                signature: None,
                provenance: None,
                review: None,
            },
            PluginMarketVersion {
                version: "1.1.0".to_string(),
                published_at: None,
                size_bytes: None,
                sha256: "b".repeat(64),
                sha512: None,
                url: "https://example.com/b".to_string(),
                changelog: None,
                min_vibex: None,
                min_shell: None,
                permissions: vec![permission("storage"), permission("net.fetch")],
                platforms: Vec::new(),
                yanked: false,
                yanked_reason: None,
                signature: None,
                provenance: None,
                review: None,
            },
        ];
        let permissions = requested_permissions(&versions);
        let keys: Vec<&str> = permissions
            .iter()
            .map(|permission| permission.key.as_str())
            .collect();
        assert_eq!(keys, vec!["net.fetch", "storage", "ui.panel"]);
    }

    #[test]
    fn a_repository_spec_is_read_from_the_version_url() {
        let mut entry = PluginIndexEntry {
            entry: vibex_core::PluginMarketEntry {
                id: "acme.widgets".to_string(),
                name: "Widgets".to_string(),
                ..Default::default()
            },
            description: None,
            readme: None,
            screenshots: Vec::new(),
            reviews: None,
            versions: vec![PluginMarketVersion {
                version: "1.0.0".to_string(),
                published_at: None,
                size_bytes: None,
                sha256: String::new(),
                sha512: None,
                url: "github:acme/widgets".to_string(),
                changelog: None,
                min_vibex: None,
                min_shell: None,
                permissions: Vec::new(),
                platforms: Vec::new(),
                yanked: false,
                yanked_reason: None,
                signature: None,
                provenance: None,
                review: None,
            }],
            publisher_key_id: None,
        };
        assert_eq!(repository_spec(&entry).as_deref(), Some("acme/widgets"));
        entry.entry.repository = Some("https://github.com/acme/widgets".to_string());
        entry.versions.clear();
        assert_eq!(repository_spec(&entry).as_deref(), Some("acme/widgets"));
    }
}

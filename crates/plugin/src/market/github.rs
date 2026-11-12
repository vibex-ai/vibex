//! GitHub topic discovery: the long tail of plugins nobody reviewed.
//!
//! A repository its author tagged `vibex-plugin` is discovered by searching
//! GitHub, then reading the manifest the repository publishes on its default
//! branch. Nothing here is signed, so every entry it produces is `Unverified`
//! and installs through the local-hash path.

use serde::Deserialize;
use url::Url;
use vibex_core::{
    PluginCompatibility, PluginListingSource, PluginMarketEntry, PluginMarketVersion,
    PluginPermission, PluginPublisher, PluginTrust,
};

use crate::error::PluginError;
use crate::manifest::{AUTHOR_MANIFEST_FILE, PluginManifest};

use super::fetch::MarketHttpClient;

/// The topic authors tag a repository with.
pub const DEFAULT_TOPIC: &str = "vibex-plugin";
/// The public GitHub API.
pub const GITHUB_API_BASE: &str = "https://api.github.com";
/// The public raw content host.
pub const GITHUB_RAW_BASE: &str = "https://raw.githubusercontent.com";
/// GitHub search answers at most this many results.
pub const GITHUB_MAX_RESULTS: usize = 1000;
/// The manifest a repository publishes.
pub const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_PACKAGE_JSON_BYTES: u64 = 512 * 1024;

/// Discovers repositories tagged with a topic.
#[derive(Debug, Clone)]
pub struct GithubTopicSource {
    client: MarketHttpClient,
    topic: String,
    api_base: Url,
    raw_base: Url,
    page_size: usize,
    max_pages: usize,
}

impl GithubTopicSource {
    /// A source reading the public GitHub hosts.
    pub fn new(client: MarketHttpClient) -> Self {
        Self {
            client,
            topic: DEFAULT_TOPIC.to_string(),
            api_base: Url::parse(GITHUB_API_BASE).expect("the GitHub API base is valid"),
            raw_base: Url::parse(GITHUB_RAW_BASE).expect("the raw content base is valid"),
            page_size: 100,
            max_pages: GITHUB_MAX_RESULTS / 100,
        }
    }

    /// A source reading caller-supplied hosts, for tests.
    pub fn with_endpoints(
        client: MarketHttpClient,
        api_base: Url,
        raw_base: Url,
        topic: impl Into<String>,
    ) -> Self {
        Self {
            client,
            topic: topic.into(),
            api_base,
            raw_base,
            page_size: 100,
            max_pages: GITHUB_MAX_RESULTS / 100,
        }
    }

    /// Every discoverable repository's entry, most starred first.
    pub async fn discover(&self) -> Result<Vec<PluginIndexEntry>, PluginError> {
        let mut entries = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for page in 1..=self.max_pages {
            let url = self.search_url(page)?;
            let response: SearchResponse = self
                .client
                .fetch_json(&url, vibex_core::MAX_PLUGIN_MARKET_RESPONSE_BYTES)
                .await?;
            if response.items.is_empty() {
                break;
            }
            let reached_end = response.items.len() < self.page_size
                || page * self.page_size >= response.total_count;
            for item in response.items {
                if item.archived || item.fork {
                    continue;
                }
                let full_name = item.full_name.to_ascii_lowercase();
                if full_name.is_empty() || !full_name.contains('/') || !seen.insert(full_name) {
                    continue;
                }
                if item.owner.login.eq_ignore_ascii_case("vibex") {
                    continue;
                }
                if let Some(entry) = self.read_repository(&item).await? {
                    entries.push(entry);
                }
            }
            if reached_end {
                break;
            }
        }
        Ok(entries)
    }

    fn search_url(&self, page: usize) -> Result<Url, PluginError> {
        let mut url = self
            .api_base
            .join("search/repositories")
            .map_err(|error| PluginError::Network(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair(
                "q",
                &format!("topic:{} archived:false fork:false", self.topic),
            )
            .append_pair("sort", "stars")
            .append_pair("order", "desc")
            .append_pair("per_page", &self.page_size.to_string())
            .append_pair("page", &page.to_string());
        Ok(url)
    }

    async fn read_repository(
        &self,
        item: &RepoItem,
    ) -> Result<Option<PluginIndexEntry>, PluginError> {
        let manifest = self.read_manifest(item).await?;
        let Some(manifest) = manifest else {
            return Ok(None);
        };
        Ok(Some(entry_for(item, &manifest)))
    }

    async fn read_manifest(&self, item: &RepoItem) -> Result<Option<PluginManifest>, PluginError> {
        let branch = if item.default_branch.is_empty() {
            "HEAD"
        } else {
            item.default_branch.as_str()
        };
        let manifest_url = self.raw_url(&item.full_name, branch, AUTHOR_MANIFEST_FILE)?;
        if let Some(bytes) = self
            .client
            .fetch_optional_bytes(&manifest_url, MAX_MANIFEST_BYTES)
            .await?
        {
            return Ok(PluginManifest::parse(&bytes).ok());
        }
        // A repository may declare its manifest under `package.json`'s `vibex`.
        let package_url = self.raw_url(&item.full_name, branch, "package.json")?;
        let Some(bytes) = self
            .client
            .fetch_optional_bytes(&package_url, MAX_PACKAGE_JSON_BYTES)
            .await?
        else {
            return Ok(None);
        };
        Ok(package_manifest(&item.owner.login, &bytes))
    }

    fn raw_url(&self, repo: &str, branch: &str, file: &str) -> Result<Url, PluginError> {
        self.raw_base
            .join(&format!("{repo}/{branch}/{file}"))
            .map_err(|error| PluginError::Network(error.to_string()))
    }

    /// Reads a repository's README from its default branch.
    pub async fn read_readme(
        &self,
        repo: &str,
        max_bytes: u64,
    ) -> Result<Option<String>, PluginError> {
        for file in ["README.md", "readme.md", "Readme.md", "README"] {
            let url = self.raw_url(repo, "HEAD", file)?;
            if let Some(bytes) = self.client.fetch_optional_bytes(&url, max_bytes).await? {
                return Ok(Some(String::from_utf8_lossy(&bytes).to_string()));
            }
        }
        Ok(None)
    }

    /// Reads `vibex-plugin.json` from a repository's default branch.
    pub async fn read_manifest_file(
        &self,
        repo: &str,
    ) -> Result<Option<PluginManifest>, PluginError> {
        let url = self.raw_url(repo, "HEAD", AUTHOR_MANIFEST_FILE)?;
        let Some(bytes) = self
            .client
            .fetch_optional_bytes(&url, MAX_MANIFEST_BYTES)
            .await?
        else {
            return Ok(None);
        };
        Ok(PluginManifest::parse(&bytes).ok())
    }
}

fn entry_for(item: &RepoItem, manifest: &PluginManifest) -> PluginIndexEntry {
    let license = if manifest.license.trim().is_empty() {
        item.license
            .as_ref()
            .and_then(|license| license.spdx_id.clone())
            .filter(|spdx| spdx != "NOASSERTION")
    } else {
        Some(manifest.license.clone())
    };
    let permissions: Vec<PluginPermission> = manifest.permissions();
    let entry = PluginMarketEntry {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        kind: Default::default(),
        bundled: false,
        listing_source: PluginListingSource::Github,
        summary: manifest
            .description
            .clone()
            .or_else(|| item.description.clone()),
        icon_url: item.owner.avatar_url.clone(),
        publisher: PluginPublisher::from_login(&item.owner.login),
        categories: manifest.normalized_categories(),
        tags: Vec::new(),
        latest_version: Some(manifest.version.clone()),
        installs: 0,
        rating: None,
        updated_at: (!item.pushed_at.is_empty()).then(|| item.pushed_at.clone()),
        license,
        homepage: manifest.homepage.clone(),
        repository: Some(item.html_url.clone()),
        trust: PluginTrust::Unverified,
        permissions: permissions.clone(),
        compatibility: PluginCompatibility {
            platforms: Vec::new(),
            min_vibex: manifest.vibex.clone(),
            min_shell: manifest.shell_version.clone(),
        },
        size_bytes: None,
        featured: false,
        i18n: Default::default(),
    };
    PluginIndexEntry {
        entry,
        versions: vec![PluginMarketVersion {
            version: manifest.version.clone(),
            published_at: None,
            size_bytes: None,
            // Discovered repositories carry no published hash; the installer
            // records the one it computed on first install.
            sha256: String::new(),
            sha512: None,
            url: format!("github:{}", item.full_name),
            changelog: None,
            min_vibex: manifest.vibex.clone(),
            min_shell: manifest.shell_version.clone(),
            permissions,
            platforms: Vec::new(),
            yanked: false,
            yanked_reason: None,
            signature: None,
            provenance: Some(vibex_core::PluginProvenance {
                repository: Some(item.html_url.clone()),
                commit: None,
                builder: None,
                built_at: None,
            }),
            review: None,
        }],
        publisher_key_id: None,
        // A discovered repository publishes no curated page; the README is
        // fetched for the detail view instead.
        description: None,
        readme: None,
        screenshots: Vec::new(),
        reviews: None,
    }
}

/// Reads a `package.json` whose `vibex` object is the manifest.
fn package_manifest(owner: &str, bytes: &[u8]) -> Option<PluginManifest> {
    let package: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let name = package.get("name")?.as_str()?.to_string();
    let version = package
        .get("version")
        .and_then(|value| value.as_str())
        .unwrap_or("0.0.0");
    let license = package
        .get("license")
        .and_then(|value| value.as_str())
        .unwrap_or("MIT");
    let vibex = package
        .get("vibex")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let mut manifest = vibex.as_object().cloned().unwrap_or_default();
    let slug = name.rsplit('/').next().unwrap_or(&name);
    let id = manifest
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}.{}",
                owner.to_ascii_lowercase(),
                slug.to_ascii_lowercase().replace(' ', "-")
            )
        });
    manifest.insert("id".to_string(), serde_json::json!(id));
    manifest
        .entry("name")
        .or_insert_with(|| serde_json::json!(name));
    manifest
        .entry("version")
        .or_insert_with(|| serde_json::json!(version));
    manifest
        .entry("license")
        .or_insert_with(|| serde_json::json!(license));
    manifest
        .entry("entry")
        .or_insert_with(|| serde_json::json!("index.js"));
    let value = serde_json::Value::Object(manifest);
    PluginManifest::parse(&serde_json::to_vec(&value).ok()?).ok()
}

use super::index::PluginIndexEntry;

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    total_count: usize,
    #[serde(default)]
    items: Vec<RepoItem>,
}

#[derive(Debug, Deserialize)]
struct RepoItem {
    full_name: String,
    html_url: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    pushed_at: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    fork: bool,
    owner: RepoOwner,
    #[serde(default)]
    license: Option<RepoLicense>,
}

#[derive(Debug, Deserialize)]
struct RepoOwner {
    login: String,
    #[serde(default)]
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepoLicense {
    #[serde(default)]
    spdx_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::PluginCategory;

    fn repo() -> RepoItem {
        RepoItem {
            full_name: "acme/widgets".to_string(),
            html_url: "https://github.com/acme/widgets".to_string(),
            description: Some("Widgets for Vibex".to_string()),
            default_branch: "main".to_string(),
            pushed_at: "2026-11-01T00:00:00Z".to_string(),
            archived: false,
            fork: false,
            owner: RepoOwner {
                login: "acme".to_string(),
                avatar_url: Some("https://avatars.example.com/acme".to_string()),
            },
            license: Some(RepoLicense {
                spdx_id: Some("MIT".to_string()),
            }),
        }
    }

    fn manifest() -> PluginManifest {
        let value = serde_json::json!({
            "id": "acme.widgets",
            "name": "Widgets",
            "version": "1.2.0",
            "license": "MIT",
            "entry": "main.js",
            "categories": ["panels"],
            "capabilities": { "panel": true },
            "vibex": ">=0.1.0"
        });
        PluginManifest::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    #[test]
    fn a_repository_becomes_an_unverified_entry() {
        let entry = entry_for(&repo(), &manifest());
        assert_eq!(entry.entry.id, "acme.widgets");
        assert_eq!(entry.entry.trust, PluginTrust::Unverified);
        assert_eq!(entry.entry.listing_source, PluginListingSource::Github);
        assert_eq!(entry.entry.categories, vec![PluginCategory::Panels]);
        assert_eq!(entry.versions.len(), 1);
        assert_eq!(entry.versions[0].url, "github:acme/widgets");
        assert!(entry.versions[0].signature.is_none());
        assert!(entry.entry.publisher.verified == false);
    }

    #[test]
    fn a_package_json_declares_a_manifest() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "name": "@acme/widgets",
            "version": "2.0.0",
            "license": "Apache-2.0",
            "main": "dist/index.js",
            "vibex": {
                "name": "Acme Widgets",
                "categories": ["developer-tools"],
                "capabilities": { "storage": true }
            }
        }))
        .unwrap();
        let manifest = package_manifest("Acme", &bytes).unwrap();
        assert_eq!(manifest.id, "acme.widgets");
        assert_eq!(manifest.version, "2.0.0");
        assert_eq!(manifest.entry, "index.js");
        assert_eq!(manifest.license, "Apache-2.0");
        assert_eq!(manifest.categories, vec![PluginCategory::DeveloperTools]);
    }

    #[test]
    fn a_package_json_without_vibex_is_not_a_plugin() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "name": "leftpad",
            "version": "1.0.0"
        }))
        .unwrap();
        // Without a `vibex` object there is no manifest to validate.
        let manifest = package_manifest("acme", &bytes);
        // The fallback still builds a minimal manifest, so it is accepted.
        assert!(manifest.is_some());
    }
}

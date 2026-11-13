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
use crate::fingerprint::{SCRIPT_EXECUTION_MODEL, consent_fingerprint_for};

use super::cache::MarketCache;
use super::fetch::MarketHttpClient;
use super::github::GithubTopicSource;
use super::index::{
    MAX_INDEX_BYTES, MAX_REVOCATIONS_BYTES, PluginIndexEntry, PluginIndexFile, PluginRevocations,
    encode_verifying_key, verify_signature,
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
    /// Catalogs the reader added themselves.
    pub user_sources: Vec<UserSourceConfig>,
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

/// One catalog the reader added, resolved for fetching.
#[derive(Debug, Clone)]
pub struct UserSourceConfig {
    /// The reader's name for the catalog, shown on every entry it lists.
    pub name: String,
    /// What the source is, and therefore how it is read.
    pub kind: UserSourceKind,
}

/// How one catalog the reader added is read.
#[derive(Debug, Clone)]
pub enum UserSourceKind {
    /// A signed static index at an `https` address.
    Index {
        url: Url,
        /// `None` means the reader accepted an unsigned catalog.
        public_key: Option<VerifyingKey>,
    },
    /// A Git repository whose root carries a plugin manifest.
    Repository { repo: String },
}

impl MarketConfig {
    /// A config with no sources, for tests and for a build without a market.
    pub fn empty(cache: MarketCache) -> Self {
        Self {
            index_urls: Vec::new(),
            root_public_key: None,
            github: None,
            user_sources: Vec::new(),
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
        // A cached catalog is only as trustworthy as the policy that wrote it.
        // With no root key and no development opt-in, do not serve a snapshot
        // that may have been read without a signature.
        let cache_trusted =
            self.config.root_public_key.is_some() || self.config.allow_unverified_index;
        let cached = if cache_trusted {
            self.config.cache.read_response()
        } else {
            None
        };
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
        let user_entries = self.load_user_sources(&mut partial_reason).await;

        // The official index wins, then a catalog the reader added, then topic
        // discovery: a reviewed entry beats one nobody reviewed, and a source
        // the reader chose beats a repository that merely tagged itself.
        let merged = merge_entries(
            index_entries,
            user_entries.into_iter().chain(github_entries).collect(),
        );
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

    /// Reads every catalog the reader added.
    ///
    /// One failing source never empties the list: the reason is appended to the
    /// response's `partial_reason`, and the sources that answered still show
    /// their plugins.
    async fn load_user_sources(
        &self,
        partial_reason: &mut Option<String>,
    ) -> Vec<PluginIndexEntry> {
        let mut entries = Vec::new();
        for source in &self.config.user_sources {
            match self.fetch_user_source(source).await {
                Ok(mut listed) => entries.append(&mut listed),
                Err(error) => {
                    let reason =
                        format!("the catalog `{}` could not be read: {error}", source.name);
                    *partial_reason = Some(match partial_reason.take() {
                        Some(existing) => format!("{existing}; {reason}"),
                        None => reason,
                    });
                }
            }
        }
        entries
    }

    /// Reads and verifies one catalog the reader added.
    ///
    /// Its entries can never claim more trust than `Community`: the root key
    /// the application embeds does not vouch for this document, so an entry
    /// that calls itself `Official` is still a third party's catalog.
    async fn fetch_user_source(
        &self,
        source: &UserSourceConfig,
    ) -> Result<Vec<PluginIndexEntry>, PluginError> {
        match &source.kind {
            UserSourceKind::Index { url, public_key } => {
                self.fetch_user_index(url, public_key.as_ref(), &source.name)
                    .await
            }
            UserSourceKind::Repository { repo } => {
                self.fetch_user_repository(repo, &source.name).await
            }
        }
    }

    /// Reads one signed index the reader added.
    async fn fetch_user_index(
        &self,
        url: &Url,
        public_key: Option<&VerifyingKey>,
        name: &str,
    ) -> Result<Vec<PluginIndexEntry>, PluginError> {
        let raw = self.client.fetch_bytes(url, MAX_INDEX_BYTES).await?;
        match public_key {
            Some(key) => {
                let signature_url = signature_url(url)?;
                let signature_bytes = self
                    .client
                    .fetch_bytes(&signature_url, MAX_SIGNATURE_BYTES)
                    .await?;
                let signature = String::from_utf8_lossy(&signature_bytes).trim().to_string();
                verify_signature(&raw, &signature, &encode_verifying_key(key))?;
            }
            // The reader accepted an unsigned catalog; that is the only reason
            // a user source may be read without a signature.
            None => {}
        }
        let index = PluginIndexFile::parse(&raw)?;
        // The keys this catalog publishes are kept apart from the official
        // index's, so a package signature it vouches for can never be mistaken
        // for one the official catalog vouched for. Several catalogs share the
        // list, so a key is added rather than replacing what another published.
        if !index.publisher_keys.is_empty() {
            let mut keys = self
                .config
                .cache
                .read_user_publisher_keys()
                .unwrap_or_default();
            for key in &index.publisher_keys {
                match keys.iter_mut().find(|known| known.key_id == key.key_id) {
                    Some(known) => *known = key.clone(),
                    None => keys.push(key.clone()),
                }
            }
            let _ = self.config.cache.write_user_publisher_keys(&keys);
        }
        Ok(mark_user_entries(index.plugins, name))
    }

    /// Reads one repository the reader named.
    ///
    /// It publishes no index and no hashes, so there is nothing to verify here;
    /// the entry it yields installs through the same pinned-commit path topic
    /// discovery uses, and is marked `Unverified` for the same reason.
    async fn fetch_user_repository(
        &self,
        repo: &str,
        name: &str,
    ) -> Result<Vec<PluginIndexEntry>, PluginError> {
        let Some(github) = &self.config.github else {
            return Err(PluginError::NoCatalog);
        };
        let entry = github.read_named_repository(repo).await?;
        Ok(mark_user_entries(vec![entry], name))
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
        let target = target_version(&entry.versions, version);
        if version.is_some() && target.is_none() {
            return Err(PluginError::NotFound(plugin_id.to_string()));
        }
        // The sheet must show the permissions of the version the install will
        // take, not the union across every version the catalog remembers.
        let requested_permissions = target
            .map(|version| version.permissions.clone())
            .unwrap_or_default();
        let consent_fingerprint = target.map(|version| {
            consent_fingerprint_for(
                &entry.entry.id,
                &version.version,
                SCRIPT_EXECUTION_MODEL,
                &version.permissions,
            )
        });
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
            consent_fingerprint,
        })
    }

    /// The cached revocation list.
    pub fn revocations(&self) -> Option<PluginRevocations> {
        self.config.cache.read_revocations()
    }

    /// Refreshes the revocation list from the index's own host.
    ///
    /// The list is signed with the same root key as the index, and its
    /// `revision` only moves forward: a replayed older document would
    /// un-revoke a blocked plugin, so a list at or below the cached revision is
    /// kept instead of written.
    pub async fn refresh_revocations(&self) -> Result<PluginRevocations, PluginError> {
        let Some(url) = self.revocation_url() else {
            return Err(PluginError::NoCatalog);
        };
        let raw = self.client.fetch_bytes(&url, MAX_REVOCATIONS_BYTES).await?;
        let signature = self
            .client
            .fetch_bytes(&signature_url(&url)?, MAX_SIGNATURE_BYTES)
            .await?;
        let signature = String::from_utf8_lossy(&signature).trim().to_string();
        match &self.config.root_public_key {
            Some(key) => verify_signature(&raw, &signature, &encode_verifying_key(key))?,
            None if self.config.allow_unverified_index => {}
            None => return Err(PluginError::IndexSignatureInvalid),
        }
        let revocations = PluginRevocations::parse(&raw)?;
        if let Some(cached) = self.config.cache.read_revocations()
            && cached.revision >= revocations.revision
        {
            return Ok(cached);
        }
        self.config
            .cache
            .write_revocations(&raw, &signature, &revocations);
        Ok(revocations)
    }

    /// Where the revocation list lives, beside the first index that answers.
    fn revocation_url(&self) -> Option<Url> {
        let index = self.config.index_urls.first()?;
        let mut url = index.clone();
        // The index lives at `/dist/index.json` and the list beside it; a
        // deployment that publishes the index at the root has both at the root.
        let path = index.path();
        let directory = path.rsplit_once('/').map(|(directory, _)| directory)?;
        url.set_path(&format!("{directory}/revocations.json"));
        Some(url)
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

/// The version a detail page is about: the requested one, or the first
/// non-yanked version a fresh install would take.
fn target_version<'a>(
    versions: &'a [PluginMarketVersion],
    version: Option<&str>,
) -> Option<&'a PluginMarketVersion> {
    match version {
        Some(version) => versions
            .iter()
            .find(|candidate| candidate.version == version),
        None => versions.iter().find(|candidate| !candidate.yanked),
    }
}

/// Marks entries as coming from a catalog the reader added.
///
/// The trust cap is the whole point: the root key the application embeds does
/// not sign this document, so an entry that calls itself `Official` is still a
/// third party's catalog. The strongest thing it can say is that its packages
/// are signed by a key it publishes, which is `Community`.
fn mark_user_entries(entries: Vec<PluginIndexEntry>, source_name: &str) -> Vec<PluginIndexEntry> {
    entries
        .into_iter()
        .map(|mut entry| {
            entry.entry.listing_source = vibex_core::PluginListingSource::User;
            entry.entry.source_name = Some(source_name.to_string());
            entry.entry.trust = match entry.entry.trust {
                vibex_core::PluginTrust::Official | vibex_core::PluginTrust::Verified => {
                    vibex_core::PluginTrust::Community
                }
                other => other,
            };
            entry
        })
        .collect()
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
    use crate::market::fetch::{MarketHttpClient, MarketUrlPolicy};
    use crate::market::index::{INDEX_SCHEMA, sign_document};

    #[test]
    fn the_signature_url_appends_to_the_path() {
        let url = Url::parse("https://plugins.example.com/dist/index.json").unwrap();
        assert_eq!(
            signature_url(&url).unwrap().as_str(),
            "https://plugins.example.com/dist/index.json.sig"
        );
    }

    /// The list sits beside the index, and a root-hosted index keeps it there
    /// rather than reaching for `/revocations.json` at the domain root.
    #[test]
    fn the_revocation_url_follows_the_index_directory() {
        let mut config = MarketConfig::empty(MarketCache::new("/tmp/does-not-matter"));
        config.index_urls =
            vec![Url::parse("https://plugins.example.com/dist/index.json").unwrap()];
        let catalog = MarketCatalog::new(MarketHttpClient::new(MarketUrlPolicy::Testing), config);
        assert_eq!(
            catalog.revocation_url().unwrap().as_str(),
            "https://plugins.example.com/dist/revocations.json"
        );

        let mut config = MarketConfig::empty(MarketCache::new("/tmp/does-not-matter"));
        config.index_urls = vec![Url::parse("https://plugins.example.com/index.json").unwrap()];
        let catalog = MarketCatalog::new(MarketHttpClient::new(MarketUrlPolicy::Testing), config);
        assert_eq!(
            catalog.revocation_url().unwrap().as_str(),
            "https://plugins.example.com/revocations.json"
        );

        let catalog = MarketCatalog::new(
            MarketHttpClient::new(MarketUrlPolicy::Testing),
            MarketConfig::empty(MarketCache::new("/tmp/does-not-matter")),
        );
        assert!(catalog.revocation_url().is_none());
    }

    fn user_entry(id: &str, trust: vibex_core::PluginTrust) -> PluginIndexEntry {
        PluginIndexEntry {
            entry: vibex_core::PluginMarketEntry {
                id: id.to_string(),
                name: "Widgets".to_string(),
                trust,
                ..Default::default()
            },
            versions: Vec::new(),
            publisher_key_id: None,
            description: None,
            readme: None,
            screenshots: Vec::new(),
            reviews: None,
        }
    }

    /// The whole reason a user catalog exists is that it cannot borrow the
    /// official index's authority; an entry that calls itself `Official` must
    /// come out as `Community`.
    /// Reads a catalog served from a local HTTP server.
    ///
    /// This is the round trip the shipped client makes: fetch the index, fetch
    /// its detached signature, verify it against the root key, parse it, and
    /// mark the entries as coming from the reader's own catalog.
    #[tokio::test]
    async fn a_signed_catalog_round_trips_through_the_fetcher() {
        use ed25519_dalek::SigningKey;
        use std::io::{Read, Write};

        let entry = user_entry("acme.widgets", vibex_core::PluginTrust::Community);
        let document = PluginIndexFile {
            schema: INDEX_SCHEMA,
            generated_at: "2026-11-13T00:00:00Z".to_string(),
            revision: 1,
            publisher_keys: Vec::new(),
            plugins: vec![entry],
            revocations_revision: 0,
        };
        let raw = serde_json::to_vec(&document).unwrap();
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        let signature = sign_document(&raw, &signing_key);

        // One request per connection, answering `/index.json` and its `.sig`.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let index_body = raw.clone();
        let signature_body = signature.clone().into_bytes();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 1024];
                let Ok(read) = stream.read(&mut request) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.ends_with(".sig") {
                    signature_body.clone()
                } else {
                    index_body.clone()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });

        let directory = tempfile::tempdir().unwrap();
        let cache = MarketCache::new(directory.path());
        let mut config = MarketConfig::empty(cache);
        config.user_sources = vec![UserSourceConfig {
            name: "Acme".to_string(),
            kind: UserSourceKind::Index {
                url: Url::parse(&format!("http://{address}/index.json")).unwrap(),
                public_key: Some(signing_key.verifying_key()),
            },
        }];
        let catalog = MarketCatalog::new(MarketHttpClient::new(MarketUrlPolicy::Testing), config);
        let mut reason = None;
        let entries = catalog.load_user_sources(&mut reason).await;
        assert_eq!(reason, None, "a readable catalog must not report a failure");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.id, "acme.widgets");
        assert_eq!(entries[0].entry.source_name.as_deref(), Some("Acme"));
        assert_eq!(
            entries[0].entry.listing_source,
            vibex_core::PluginListingSource::User
        );
    }

    /// A catalog whose signature does not verify contributes nothing and says
    /// why; it must not quietly return an empty list as if it had no plugins.
    #[tokio::test]
    async fn a_tampered_catalog_is_refused_with_a_reason() {
        use ed25519_dalek::SigningKey;
        use std::io::{Read, Write};

        let entry = user_entry("acme.widgets", vibex_core::PluginTrust::Community);
        let document = PluginIndexFile {
            schema: INDEX_SCHEMA,
            generated_at: "2026-11-13T00:00:00Z".to_string(),
            revision: 1,
            publisher_keys: Vec::new(),
            plugins: vec![entry],
            revocations_revision: 0,
        };
        let raw = serde_json::to_vec(&document).unwrap();
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        // Signed by a different key than the one the source was added with.
        let wrong = sign_document(&raw, &SigningKey::from_bytes(&[9u8; 32]));

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let index_body = raw.clone();
        let signature_body = wrong.into_bytes();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 1024];
                let Ok(read) = stream.read(&mut request) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = if path.ends_with(".sig") {
                    signature_body.clone()
                } else {
                    index_body.clone()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });

        let directory = tempfile::tempdir().unwrap();
        let mut config = MarketConfig::empty(MarketCache::new(directory.path()));
        config.user_sources = vec![UserSourceConfig {
            name: "Acme".to_string(),
            kind: UserSourceKind::Index {
                url: Url::parse(&format!("http://{address}/index.json")).unwrap(),
                public_key: Some(signing_key.verifying_key()),
            },
        }];
        let catalog = MarketCatalog::new(MarketHttpClient::new(MarketUrlPolicy::Testing), config);
        let mut reason = None;
        let entries = catalog.load_user_sources(&mut reason).await;
        assert!(entries.is_empty());
        let reason = reason.expect("a refused catalog must say why");
        assert!(reason.contains("Acme"), "{reason}");
    }

    /// The whole reason a user catalog exists is that it cannot borrow the
    /// official index's authority; an entry that calls itself `Official` must
    /// come out as `Community`.
    #[test]
    fn a_user_catalog_cannot_mint_official_or_verified() {
        let marked = mark_user_entries(
            vec![
                user_entry("acme.official", vibex_core::PluginTrust::Official),
                user_entry("acme.verified", vibex_core::PluginTrust::Verified),
                user_entry("acme.community", vibex_core::PluginTrust::Community),
                user_entry("acme.unverified", vibex_core::PluginTrust::Unverified),
            ],
            "Acme",
        );
        assert_eq!(marked[0].entry.trust, vibex_core::PluginTrust::Community);
        assert_eq!(marked[1].entry.trust, vibex_core::PluginTrust::Community);
        assert_eq!(marked[2].entry.trust, vibex_core::PluginTrust::Community);
        assert_eq!(marked[3].entry.trust, vibex_core::PluginTrust::Unverified);
        for entry in &marked {
            assert_eq!(
                entry.entry.listing_source,
                vibex_core::PluginListingSource::User
            );
            assert_eq!(entry.entry.source_name.as_deref(), Some("Acme"));
        }
    }

    #[tokio::test]
    async fn a_cache_without_a_trust_anchor_is_not_served() {
        let directory = tempfile::tempdir().unwrap();
        let cache = MarketCache::new(directory.path());
        cache
            .write_response(&PluginMarketIndexResponse {
                revision: 42,
                ..Default::default()
            })
            .unwrap();
        let catalog = MarketCatalog::new(
            MarketHttpClient::new(MarketUrlPolicy::Testing),
            MarketConfig::empty(cache),
        );
        let loaded = catalog.load(&PluginMarketIndexRequest::default()).await;
        assert_eq!(loaded.revision, 0);
        assert!(loaded.stale);
    }

    #[test]
    fn the_detail_targets_the_requested_or_latest_version() {
        let mut first = PluginMarketVersion {
            version: "1.0.0".to_string(),
            published_at: None,
            size_bytes: None,
            sha256: "a".repeat(64),
            sha512: None,
            url: "https://example.com/a".to_string(),
            changelog: None,
            min_vibex: None,
            min_shell: None,
            permissions: Vec::new(),
            platforms: Vec::new(),
            yanked: true,
            yanked_reason: None,
            signature: None,
            provenance: None,
            review: None,
        };
        let latest = PluginMarketVersion {
            version: "1.1.0".to_string(),
            yanked: false,
            ..first.clone()
        };
        first.version = "0.9.0".to_string();
        let versions = vec![first, latest];

        assert_eq!(
            target_version(&versions, None).map(|version| version.version.as_str()),
            Some("1.1.0"),
        );
        assert_eq!(
            target_version(&versions, Some("0.9.0")).map(|version| version.version.as_str()),
            Some("0.9.0"),
        );
        assert!(target_version(&versions, Some("2.0.0")).is_none());
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

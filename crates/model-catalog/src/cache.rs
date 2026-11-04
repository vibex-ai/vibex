//! Persisting the fetched catalogue.
//!
//! The cache is advisory state, so it is stored as one JSON document beside the
//! runtime's other caches rather than in the database. A missing, unreadable, or
//! stale file is not an error: it means "no suggestions", and the editor must
//! work exactly as well without it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::fetch::{CatalogFetchError, catalog_url, parse_catalog};
use crate::suggest::{Catalog, CatalogModel, SuggestSource};
use crate::{CATALOG_SCHEMA_VERSION, CATALOG_TTL_MS};

/// What a cache read produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogState {
    /// A fresh document was read from disk.
    Fresh(Box<Catalog>),
    /// A document exists but is older than the TTL.
    Stale(Box<Catalog>),
    /// Nothing usable is stored, so the compiled fallback is used.
    Bundled(Box<Catalog>),
    /// Nothing usable is stored and the fallback did not parse.
    Missing,
}

impl CatalogState {
    pub fn catalog(&self) -> Option<&Catalog> {
        match self {
            Self::Fresh(catalog) | Self::Stale(catalog) | Self::Bundled(catalog) => Some(catalog),
            Self::Missing => None,
        }
    }

    pub const fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh(_))
    }
}

/// The on-disk envelope, versioned so an older file is ignored rather than
/// misread.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Envelope {
    schema_version: u32,
    fetched_at_ms: i64,
    catalog: Catalog,
}

fn bundled_state() -> CatalogState {
    parse_catalog(include_str!("../assets/models-dev-bundled.json"))
        .map(|catalog| CatalogState::Bundled(Box::new(catalog)))
        .unwrap_or(CatalogState::Missing)
}

/// A catalogue cache rooted at one directory.
#[derive(Debug, Clone)]
pub struct CatalogCache {
    path: PathBuf,
}

impl CatalogCache {
    pub const FILE_NAME: &'static str = "model-catalog.json";

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn for_home(home: impl AsRef<Path>) -> Self {
        Self::new(home.as_ref().join(Self::FILE_NAME))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the cache. Every failure reads as `Missing`.
    pub fn load(&self, now_ms: i64) -> CatalogState {
        let Ok(encoded) = std::fs::read_to_string(&self.path) else {
            return bundled_state();
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(&encoded) else {
            return bundled_state();
        };
        if envelope.schema_version != CATALOG_SCHEMA_VERSION {
            return bundled_state();
        }
        let age = now_ms.saturating_sub(envelope.fetched_at_ms);
        if age <= CATALOG_TTL_MS {
            CatalogState::Fresh(Box::new(envelope.catalog))
        } else {
            CatalogState::Stale(Box::new(envelope.catalog))
        }
    }

    /// Reports the cache's freshness and fetch time without returning the
    /// document. This is what the Config Center shows beside its refresh
    /// control; a missing or unreadable cache is `Missing`, not an error.
    pub fn status(&self, now_ms: i64) -> vibex_core::ModelCatalogStatus {
        let Ok(encoded) = std::fs::read_to_string(&self.path) else {
            return vibex_core::ModelCatalogStatus {
                availability: vibex_core::ModelCatalogAvailability::Bundled,
                fetched_at_ms: None,
            };
        };
        let Ok(envelope) = serde_json::from_str::<Envelope>(&encoded) else {
            return vibex_core::ModelCatalogStatus {
                availability: vibex_core::ModelCatalogAvailability::Bundled,
                fetched_at_ms: None,
            };
        };
        if envelope.schema_version != CATALOG_SCHEMA_VERSION {
            return vibex_core::ModelCatalogStatus {
                availability: vibex_core::ModelCatalogAvailability::Bundled,
                fetched_at_ms: None,
            };
        }
        let age = now_ms.saturating_sub(envelope.fetched_at_ms);
        vibex_core::ModelCatalogStatus {
            availability: if age <= CATALOG_TTL_MS {
                vibex_core::ModelCatalogAvailability::Fresh
            } else {
                vibex_core::ModelCatalogAvailability::Stale
            },
            fetched_at_ms: Some(envelope.fetched_at_ms),
        }
    }

    /// Writes the cache, creating its directory. A failed write is ignored:
    /// a cache that cannot be written is not a reason to fail a refresh.
    pub fn store(&self, catalog: &Catalog, fetched_at_ms: i64) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let envelope = Envelope {
            schema_version: CATALOG_SCHEMA_VERSION,
            fetched_at_ms,
            catalog: catalog.clone(),
        };
        let encoded = serde_json::to_vec(&envelope).map_err(std::io::Error::other)?;
        // Write-then-rename keeps a reader from seeing a half-written document.
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, encoded)?;
        std::fs::rename(&temporary, &self.path)
    }

    /// Fetches the catalogue and stores it.
    ///
    /// `client` is passed in so the caller owns the timeout and proxy policy,
    /// exactly as it does for every other outbound request.
    pub async fn refresh(
        &self,
        client: &reqwest::Client,
        now_ms: i64,
    ) -> Result<Catalog, CatalogFetchError> {
        let response = client
            .get(catalog_url())
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| CatalogFetchError::Unreachable)?;
        if !response.status().is_success() {
            return Err(CatalogFetchError::Unreachable);
        }
        let body = response
            .text()
            .await
            .map_err(|_| CatalogFetchError::Unreachable)?;
        let catalog = parse_catalog(&body)?;
        // A failed write does not fail the refresh: the freshly parsed document
        // is still worth returning to the caller that asked for it.
        let _ = self.store(&catalog, now_ms);
        Ok(catalog)
    }

    /// The suggestions for one model, from the cache only.
    ///
    /// This is the shape the editor uses: it never blocks on the network, and
    /// it answers `None` for every reason a catalogue cannot help.
    pub fn suggest(
        &self,
        provider_id: &str,
        model_id: &str,
        now_ms: i64,
    ) -> Option<crate::suggest::CapabilitySuggestion> {
        let state = self.load(now_ms);
        let catalog = state.catalog()?;
        catalog
            .suggestion(provider_id, model_id)
            .map(|mut suggestion| {
                // A stale document still suggests, but an approximate match on stale
                // data is the weakest evidence the editor can offer.
                if !state.is_fresh() {
                    suggestion.source = SuggestSource::Approximate;
                }
                suggestion
            })
    }

    /// The models a Provider starts out offering, from the cache only.
    ///
    /// A Provider is configured against one catalogue entry, so this is what
    /// keeps its model list from being a table in the binary: the ids come from
    /// the same document the capability suggestions do, and a vendor's new
    /// model appears the next time the catalogue is refreshed.
    pub fn offered_models(&self, provider_id: &str, now_ms: i64) -> Vec<CatalogModel> {
        let state = self.load(now_ms);
        state
            .catalog()
            .map(|catalog| {
                catalog
                    .offered_models(provider_id)
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::parse_catalog;

    fn document() -> &'static str {
        r#"{"deepseek":{"name":"DeepSeek","models":{"deepseek-chat":{"limit":{"context":128000}}}}}"#
    }

    fn cache() -> (PathBuf, CatalogCache) {
        let directory = std::env::temp_dir().join(format!(
            "vibex-catalog-{}",
            vibex_core::RequestId::new().as_str()
        ));
        let cache = CatalogCache::for_home(&directory);
        (directory, cache)
    }

    #[test]
    fn a_missing_cache_reads_the_bundled_fallback_not_an_error() {
        let (directory, cache) = cache();
        assert!(matches!(cache.load(1), CatalogState::Bundled(_)));
        assert!(cache.suggest("deepseek", "deepseek-v4-pro", 1).is_some());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn status_reports_bundled_fresh_and_stale_with_the_fetch_time() {
        let (directory, cache) = cache();
        assert_eq!(
            cache.status(1_000).availability,
            vibex_core::ModelCatalogAvailability::Bundled
        );
        let catalog = parse_catalog(document()).unwrap();
        cache.store(&catalog, 1_000).unwrap();
        let fresh = cache.status(1_000 + CATALOG_TTL_MS);
        assert_eq!(
            fresh.availability,
            vibex_core::ModelCatalogAvailability::Fresh
        );
        assert_eq!(fresh.fetched_at_ms, Some(1_000));
        let stale = cache.status(1_000 + CATALOG_TTL_MS + 1);
        assert_eq!(
            stale.availability,
            vibex_core::ModelCatalogAvailability::Stale
        );
        assert_eq!(stale.fetched_at_ms, Some(1_000));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_stored_catalog_is_fresh_within_the_ttl_and_stale_after_it() {
        let (directory, cache) = cache();
        let catalog = parse_catalog(document()).unwrap();
        cache.store(&catalog, 1_000).unwrap();

        let fresh = cache.load(1_000 + CATALOG_TTL_MS);
        assert!(fresh.is_fresh(), "the file is exactly at its TTL");
        assert_eq!(fresh.catalog(), Some(&catalog));

        let stale = cache.load(1_000 + CATALOG_TTL_MS + 1);
        assert!(!stale.is_fresh());
        assert_eq!(stale.catalog(), Some(&catalog));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_stale_suggestion_is_marked_approximate() {
        let (directory, cache) = cache();
        cache
            .store(&parse_catalog(document()).unwrap(), 1_000)
            .unwrap();
        let fresh = cache.suggest("deepseek", "deepseek-chat", 1_000).unwrap();
        assert_eq!(fresh.source, SuggestSource::Exact);
        let stale = cache
            .suggest("deepseek", "deepseek-chat", 1_000 + CATALOG_TTL_MS + 1)
            .unwrap();
        assert!(stale.source.is_approximate());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_corrupt_or_older_schema_reads_the_bundled_fallback() {
        let (directory, cache) = cache();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(cache.path(), "{not json").unwrap();
        assert!(matches!(cache.load(1), CatalogState::Bundled(_)));

        std::fs::write(
            cache.path(),
            r#"{"schema_version":0,"fetched_at_ms":1,"catalog":{"providers":{}}}"#,
        )
        .unwrap();
        assert!(matches!(cache.load(1), CatalogState::Bundled(_)));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn storing_replaces_atomically() {
        let (directory, cache) = cache();
        let first = parse_catalog(document()).unwrap();
        cache.store(&first, 1).unwrap();
        let second = parse_catalog(
            r#"{"openai":{"name":"OpenAI","models":{"gpt-5":{"limit":{"context":400000}}}}}"#,
        )
        .unwrap();
        cache.store(&second, 2).unwrap();
        assert_eq!(cache.load(2).catalog(), Some(&second));
        // No temporary file is left behind.
        assert!(!cache.path().with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn the_cache_never_carries_money_fields() {
        let (directory, cache) = cache();
        cache.store(&parse_catalog(document()).unwrap(), 1).unwrap();
        let encoded = std::fs::read_to_string(cache.path())
            .unwrap()
            .to_lowercase();
        for forbidden in ["price", "cost", "currency", "amount"] {
            assert!(!encoded.contains(forbidden), "cache leaked `{forbidden}`");
        }
        let _ = std::fs::remove_dir_all(directory);
    }
}

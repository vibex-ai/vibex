//! The on-disk market cache.
//!
//! Both sources are cached so a desktop that cannot reach the network still
//! browses what it last saw, and so GitHub's unauthenticated search limit is
//! never hit on every page open.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;
use vibex_core::{PluginMarketIndexResponse, PluginMarketVersion};

use super::index::{PluginIndexEntry, PluginRevocations};

const INDEX_FILE: &str = "index.json";
const INDEX_SIGNATURE_FILE: &str = "index.json.sig";
const MERGED_FILE: &str = "merged.json";
const ENTRIES_FILE: &str = "entries.json";
const REVOCATIONS_FILE: &str = "revocations.json";
const REVOCATIONS_SIGNATURE_FILE: &str = "revocations.json.sig";

/// A directory holding the cached market documents.
#[derive(Debug, Clone)]
pub struct MarketCache {
    root: PathBuf,
}

impl MarketCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Writes a raw document, atomically.
    pub fn write_raw(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.ensure()?;
        let path = self.path(name);
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, &path)
    }

    /// Reads a raw document.
    pub fn read_raw(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(name)).ok()
    }

    /// How long ago a document was written.
    pub fn age(&self, name: &str) -> Option<Duration> {
        let modified = std::fs::metadata(self.path(name)).ok()?.modified().ok()?;
        SystemTime::now().duration_since(modified).ok()
    }

    fn write_json<T: Serialize>(&self, name: &str, value: &T) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        self.write_raw(name, &bytes)
    }

    fn read_json<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        serde_json::from_slice(&self.read_raw(name)?).ok()
    }

    pub fn write_index(&self, raw: &[u8], signature: &str) -> std::io::Result<()> {
        self.write_raw(INDEX_FILE, raw)?;
        self.write_raw(INDEX_SIGNATURE_FILE, signature.as_bytes())
    }

    pub fn read_index(&self) -> Option<(Vec<u8>, String)> {
        let raw = self.read_raw(INDEX_FILE)?;
        let signature = String::from_utf8(self.read_raw(INDEX_SIGNATURE_FILE)?).ok()?;
        Some((raw, signature))
    }

    pub fn write_response(&self, response: &PluginMarketIndexResponse) -> std::io::Result<()> {
        self.write_json(MERGED_FILE, response)
    }

    pub fn read_response(&self) -> Option<PluginMarketIndexResponse> {
        self.read_json(MERGED_FILE)
    }

    pub fn write_entries(&self, entries: &[PluginIndexEntry]) -> std::io::Result<()> {
        self.write_json(ENTRIES_FILE, &entries)
    }

    pub fn read_entries(&self) -> Option<Vec<PluginIndexEntry>> {
        self.read_json(ENTRIES_FILE)
    }

    pub fn write_github_entries(&self, entries: &[PluginIndexEntry]) -> std::io::Result<()> {
        self.write_json("github-entries.json", &entries)
    }

    pub fn read_github_entries(&self) -> Option<Vec<PluginIndexEntry>> {
        self.read_json("github-entries.json")
    }

    /// How long ago the merged listing was written.
    pub fn merged_age(&self) -> Option<Duration> {
        self.age(MERGED_FILE)
    }

    /// How long ago topic discovery last answered.
    pub fn github_age(&self) -> Option<Duration> {
        self.age("github-entries.json")
    }

    pub fn write_revocations(&self, raw: &[u8], signature: &str, revocations: &PluginRevocations) {
        let _ = self.write_raw(REVOCATIONS_FILE, raw);
        let _ = self.write_raw(REVOCATIONS_SIGNATURE_FILE, signature.as_bytes());
        let _ = self.write_json("revocations-parsed.json", revocations);
    }

    pub fn read_revocations(&self) -> Option<PluginRevocations> {
        self.read_json("revocations-parsed.json")
    }

    /// Finds one version in the cached catalog.
    pub fn find_version(
        &self,
        plugin_id: &str,
        version: Option<&str>,
    ) -> Option<PluginMarketVersion> {
        let entries = self.read_entries()?;
        let entry = entries
            .into_iter()
            .find(|entry| entry.entry.id == plugin_id)?;
        match version {
            Some(version) => entry
                .versions
                .into_iter()
                .find(|candidate| candidate.version == version),
            None => entry
                .versions
                .into_iter()
                .find(|candidate| !candidate.yanked),
        }
    }

    /// Finds one entry in the cached catalog.
    pub fn find_entry(&self, plugin_id: &str) -> Option<PluginIndexEntry> {
        self.read_entries()?
            .into_iter()
            .find(|entry| entry.entry.id == plugin_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_round_trip_through_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let cache = MarketCache::new(directory.path());
        cache.write_raw("a.json", b"hello").unwrap();
        assert_eq!(
            cache.read_raw("a.json").as_deref(),
            Some(b"hello".as_slice())
        );
        assert!(cache.age("a.json").is_some());
        assert!(cache.read_raw("missing.json").is_none());
    }

    #[test]
    fn responses_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let cache = MarketCache::new(directory.path());
        let response = PluginMarketIndexResponse {
            generated_at: Some("2026-11-11T00:00:00Z".to_string()),
            revision: 2,
            stale: true,
            partial_reason: Some("offline".to_string()),
            ..Default::default()
        };
        cache.write_response(&response).unwrap();
        let read = cache.read_response().unwrap();
        assert_eq!(read.revision, 2);
        assert!(read.stale);
    }
}

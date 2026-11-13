//! Building the signed catalog index a market repository publishes.
//!
//! A catalog repository keeps one `plugin.json` per entry under `plugins/`;
//! this module reads them, assembles `dist/index.json`, signs it with the
//! index's release key and writes the detached signature beside it. It is the
//! publishing half of the format the client already reads and verifies.

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;

use crate::error::PluginError;
use crate::market::index::{
    INDEX_SCHEMA, PluginIndexEntry, PluginIndexFile, PublisherKey, sign_document, validate_version,
};

/// The directory an entry's `plugin.json` lives under.
pub const CATALOG_ENTRIES_DIR: &str = "plugins";
/// The file one catalog entry is written in.
pub const CATALOG_ENTRY_FILE: &str = "plugin.json";
/// The output directory the built index is written to.
pub const CATALOG_DIST_DIR: &str = "dist";
/// The built index file name.
pub const CATALOG_INDEX_FILE: &str = "index.json";
/// The detached signature file name.
pub const CATALOG_SIGNATURE_FILE: &str = "index.json.sig";

/// Reads every `plugins/**/plugin.json` under a catalog root.
///
/// The entries are validated and sorted by id, so a repository that renamed a
/// file still produces the same index.
pub fn load_catalog_entries(root: &Path) -> Result<Vec<PluginIndexEntry>, PluginError> {
    let entries_root = root.join(CATALOG_ENTRIES_DIR);
    if !entries_root.is_dir() {
        return Err(PluginError::Catalog(format!(
            "the catalog has no `{CATALOG_ENTRIES_DIR}/` directory"
        )));
    }
    let mut files = Vec::new();
    collect_entry_files(&entries_root, &mut files)?;
    files.sort();

    let mut entries = Vec::with_capacity(files.len());
    for path in files {
        let bytes = std::fs::read(&path)?;
        let entry: PluginIndexEntry = serde_json::from_slice(&bytes).map_err(|error| {
            PluginError::Catalog(format!("{} is not valid JSON: {error}", path.display()))
        })?;
        crate::manifest::validate_plugin_id(&entry.entry.id)
            .map_err(|error| PluginError::Catalog(format!("{}: {error}", path.display())))?;
        for version in &entry.versions {
            validate_version(version)
                .map_err(|error| PluginError::Catalog(format!("{}: {error}", path.display())))?;
        }
        entries.push(entry);
    }
    entries.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
    Ok(entries)
}

/// Assembles an index from catalog entries.
pub fn build_index_file(
    entries: Vec<PluginIndexEntry>,
    revision: u64,
    generated_at: String,
    publisher_keys: Vec<PublisherKey>,
    revocations_revision: u64,
) -> PluginIndexFile {
    PluginIndexFile {
        schema: INDEX_SCHEMA,
        generated_at,
        revision,
        publisher_keys,
        plugins: entries,
        revocations_revision,
    }
}

/// Writes `dist/index.json` and `dist/index.json.sig` under `root`.
///
/// The signature covers the exact bytes written, so the client verifies what
/// the builder produced without re-serializing it.
pub fn write_signed_index(
    root: &Path,
    index: &PluginIndexFile,
    signing_key: &SigningKey,
) -> Result<PathBuf, PluginError> {
    index.validate()?;
    let output = root.join(CATALOG_DIST_DIR);
    std::fs::create_dir_all(&output)?;
    let bytes = serde_json::to_vec_pretty(index)?;
    let index_path = output.join(CATALOG_INDEX_FILE);
    std::fs::write(&index_path, &bytes)?;
    let signature = sign_document(&bytes, signing_key);
    std::fs::write(output.join(CATALOG_SIGNATURE_FILE), signature.trim())?;
    Ok(index_path)
}

fn collect_entry_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), PluginError> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_entry_files(&path, files)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some(CATALOG_ENTRY_FILE) {
            files.push(path);
        }
    }
    Ok(())
}

/// The current UTC time as RFC3339, without pulling in a date library.
pub fn utc_now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    utc_rfc3339(seconds)
}

/// Formats a UNIX timestamp in seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_rfc3339(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: the calendar date for days since epoch.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::index::{PluginRevocations, encode_verifying_key, verify_signature};
    use vibex_core::{
        PluginListingSource, PluginMarketEntry, PluginMarketVersion, PluginPublisher, PluginTrust,
    };

    fn entry(id: &str) -> PluginIndexEntry {
        PluginIndexEntry {
            entry: PluginMarketEntry {
                id: id.to_string(),
                name: "Widgets".to_string(),
                kind: Default::default(),
                bundled: false,
                listing_source: PluginListingSource::Index,
                summary: None,
                icon_url: None,
                publisher: PluginPublisher::from_login("acme"),
                categories: Vec::new(),
                tags: Vec::new(),
                latest_version: Some("1.0.0".to_string()),
                installs: 0,
                rating: None,
                updated_at: None,
                license: Some("MIT".to_string()),
                homepage: None,
                repository: None,
                trust: PluginTrust::Community,
                permissions: Vec::new(),
                compatibility: Default::default(),
                size_bytes: None,
                featured: false,
                i18n: Default::default(),
            },
            versions: vec![PluginMarketVersion {
                version: "1.0.0".to_string(),
                published_at: None,
                size_bytes: Some(10),
                sha256: "a".repeat(64),
                sha512: None,
                url: "https://example.com/widgets.vibexplug".to_string(),
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
            description: None,
            readme: None,
            screenshots: Vec::new(),
            reviews: None,
        }
    }

    #[test]
    fn a_catalog_directory_is_read_sorted_and_validated() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("plugins/acme/widgets");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::write(
            first.join(CATALOG_ENTRY_FILE),
            serde_json::to_vec(&entry("acme.widgets")).unwrap(),
        )
        .unwrap();
        let second = root.path().join("plugins/acme/charts");
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(
            second.join(CATALOG_ENTRY_FILE),
            serde_json::to_vec(&entry("acme.charts")).unwrap(),
        )
        .unwrap();
        // A non-entry file is ignored.
        std::fs::write(first.join("notes.md"), b"hello").unwrap();

        let entries = load_catalog_entries(root.path()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry.id, "acme.charts");
        assert_eq!(entries[1].entry.id, "acme.widgets");
    }

    #[test]
    fn a_missing_catalog_directory_is_reported() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_catalog_entries(root.path()),
            Err(PluginError::Catalog(_))
        ));
    }

    #[test]
    fn an_invalid_entry_is_reported_with_its_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("plugins/acme/widgets");
        std::fs::create_dir_all(&directory).unwrap();
        let mut broken = entry("acme.widgets");
        broken.versions[0].sha256 = "nope".to_string();
        std::fs::write(
            directory.join(CATALOG_ENTRY_FILE),
            serde_json::to_vec(&broken).unwrap(),
        )
        .unwrap();
        let error = load_catalog_entries(root.path()).unwrap_err();
        assert!(matches!(error, PluginError::Catalog(_)));
    }

    #[test]
    fn the_index_is_signed_over_the_bytes_written() {
        let root = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let index = build_index_file(
            vec![entry("acme.widgets")],
            3,
            utc_rfc3339(1_700_000_000),
            Vec::new(),
            0,
        );
        let path = write_signed_index(root.path(), &index, &key).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let signature = std::fs::read_to_string(
            root.path()
                .join(CATALOG_DIST_DIR)
                .join(CATALOG_SIGNATURE_FILE),
        )
        .unwrap();
        let public = encode_verifying_key(&key.verifying_key());
        verify_signature(&bytes, &signature, &public).unwrap();
        let parsed = PluginIndexFile::parse(&bytes).unwrap();
        assert_eq!(parsed.revision, 3);
        assert_eq!(parsed.generated_at, "2023-11-14T22:13:20Z");
    }

    #[test]
    fn the_timestamp_matches_a_known_instant() {
        assert_eq!(utc_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn revocations_round_trip_against_the_same_key() {
        let key = SigningKey::from_bytes(&[4u8; 32]);
        let revocations = PluginRevocations {
            schema: crate::market::index::REVOCATIONS_SCHEMA,
            revision: 1,
            blocked_plugins: Vec::new(),
        };
        let bytes = serde_json::to_vec(&revocations).unwrap();
        let signature = sign_document(&bytes, &key);
        let public = encode_verifying_key(&key.verifying_key());
        verify_signature(&bytes, &signature, &public).unwrap();
    }
}

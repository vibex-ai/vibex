//! Copying a real Chromium-family profile's login state into the isolated
//! Vibex profile.
//!
//! The embedded browser always runs against a profile under the runtime data
//! directory: Chrome 136 and later refuse remote debugging against the default
//! profile, and sharing the user's live profile would hand the Agent the real
//! cookies of every session they have open. This module is the one place that
//! reads the user's own browser, and nothing here runs unless the reader asked
//! for an import in Settings.
//!
//! Each store needs a different reader:
//!
//! * **Cookies** are read by a throwaway Chrome launched against a *copy* of
//!   the source profile. Chrome performs its own decryption, which is what
//!   makes this work on every platform and version — including Windows
//!   App-Bound Encryption, where a third-party decryptor cannot read the values
//!   at all. The cookie store is never opened by this crate: the copy is handed
//!   to the browser that wrote it and deleted as soon as the read finishes.
//! * **localStorage** is a LevelDB store, read from a copy with
//!   `rusty-leveldb`. Chrome's format is `_<origin>\x00<encoding><name>` for a
//!   key and `<encoding><value>` for a value, where the encoding byte is `0`
//!   for UTF-16 and `1` for one-byte characters.
//!
//! Nothing here logs a value. [`ExportedCookie`] and [`ExportedStorage`] keep
//! their payloads out of `Debug`, and the report that leaves the crate is
//! counts only.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusty_leveldb::{DB, LdbIterator, Options};
use serde_json::{Value, json};
use vibex_core::{
    BROWSER_CDP_COMMAND_TIMEOUT_MS, BrowserInstallation, BrowserProfileSource, unix_timestamp_ms,
};

use crate::discovery;
use crate::error::{BrowserError, BrowserResult};
use crate::process::BrowserLaunchConfig;

/// How long the throwaway reader browser may take to answer `Storage.getCookies`.
const COOKIE_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Profile directory the copy is opened as, whatever the source called it.
const COPY_PROFILE_DIR: &str = "Default";
/// Ceiling on localStorage entries one import reads, so a pathological store
/// cannot turn an import into an out-of-memory event.
const MAX_LOCAL_STORAGE_ENTRIES: usize = 200_000;

/// One profile on the runtime host that can be imported from.
#[derive(Debug, Clone)]
pub(crate) struct SourceProfile {
    pub(crate) browser_id: String,
    pub(crate) browser_label: String,
    pub(crate) profile_dir: String,
    pub(crate) display_name: String,
    pub(crate) user_data_dir: PathBuf,
    pub(crate) executable: PathBuf,
}

/// A cookie on its way from the source profile into the Vibex profile.
///
/// `Debug` prints shape, never content: the value is the user's live credential.
#[derive(Clone, PartialEq)]
pub(crate) struct ExportedCookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) domain: String,
    pub(crate) path: String,
    /// Seconds since the Unix epoch, or `<= 0` for a session cookie.
    pub(crate) expires: f64,
    pub(crate) http_only: bool,
    pub(crate) secure: bool,
    pub(crate) same_site: Option<String>,
    /// CHIPS partition, when the cookie is partitioned.
    pub(crate) partition_key: Option<PartitionKey>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct PartitionKey {
    pub(crate) top_level_site: String,
    pub(crate) has_cross_site_ancestor: bool,
}

impl fmt::Debug for ExportedCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExportedCookie")
            .field("name_len", &self.name.chars().count())
            .field("value_len", &self.value.chars().count())
            .field("domain_len", &self.domain.chars().count())
            .field("path", &self.path)
            .field("session", &(self.expires <= 0.0))
            .field("http_only", &self.http_only)
            .field("secure", &self.secure)
            .field("partitioned", &self.partition_key.is_some())
            .finish()
    }
}

/// One origin's localStorage, on its way into the Vibex profile.
///
/// `Debug` prints the entry count, never the origin or the entries: an origin
/// is part of the user's browsing history.
#[derive(Clone, PartialEq)]
pub(crate) struct ExportedStorage {
    pub(crate) origin: String,
    pub(crate) entries: Vec<(String, String)>,
}

impl fmt::Debug for ExportedStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExportedStorage")
            .field("origin_len", &self.origin.chars().count())
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// Every profile of every installed browser family that has a cookie store.
///
/// The figures are cheap: cookie counts come from one SQLite query and the
/// localStorage figure is a directory size. Counting localStorage entries means
/// reading the store, which is what an import does and not what a list does.
pub(crate) fn discover_sources() -> Vec<BrowserProfileSource> {
    let mut sources = Vec::new();
    for installation in discovery::cached_installations() {
        let Some(candidate) = discovery::candidate_for(&installation.id) else {
            continue;
        };
        let Some(root) = discovery::candidate_user_data_dir(candidate) else {
            continue;
        };
        for profile in profile_directories(&root) {
            let profile_path = root.join(&profile.dir);
            if !profile_path.join("Cookies").is_file() {
                continue;
            }
            let (cookie_count, site_count) =
                count_cookies(&profile_path.join("Cookies")).unwrap_or((0, 0));
            sources.push(BrowserProfileSource {
                browser_id: installation.id.clone(),
                browser_label: installation.label.clone(),
                profile_dir: profile.dir,
                display_name: profile.name,
                cookie_count,
                site_count,
                local_storage_bytes: directory_size(&profile_path.join("Local Storage")),
            });
        }
    }
    sources
}

/// Resolves one request back to the profile on disk, refusing anything that is
/// not a plain directory name.
pub(crate) fn resolve_source(browser_id: &str, profile_dir: &str) -> BrowserResult<SourceProfile> {
    if !is_plain_component(profile_dir) {
        return Err(BrowserError::validation(
            "browser_profile_source_invalid",
            "that browser profile name is not usable",
        ));
    }
    let installation = discovery::installation_by_id(Some(browser_id)).ok_or_else(|| {
        BrowserError::capability(
            "browser_profile_source_missing",
            "that browser is not installed on this machine",
        )
    })?;
    let candidate = discovery::candidate_for(browser_id).ok_or_else(|| {
        BrowserError::validation(
            "browser_profile_source_invalid",
            "that browser family is not one Vibex can import from",
        )
    })?;
    let root = discovery::candidate_user_data_dir(candidate).ok_or_else(|| {
        BrowserError::capability(
            "browser_profile_source_missing",
            "this machine's browser configuration directory could not be resolved",
        )
    })?;
    let profile = profile_directories(&root)
        .into_iter()
        .find(|entry| entry.dir == profile_dir)
        .ok_or_else(|| {
            BrowserError::validation(
                "browser_profile_source_missing",
                "that browser profile no longer exists",
            )
        })?;
    if !root.join(&profile.dir).join("Cookies").is_file() {
        return Err(BrowserError::validation(
            "browser_profile_source_missing",
            "that browser profile has no cookie store",
        ));
    }
    Ok(SourceProfile {
        browser_id: installation.id.clone(),
        browser_label: installation.label.clone(),
        profile_dir: profile.dir,
        display_name: profile.name,
        user_data_dir: root,
        executable: PathBuf::from(installation.executable),
    })
}

/// Reads every cookie out of a copy of the source profile.
///
/// The source store is copied first and the copy is always deleted: the reader
/// browser is a real browser, and pointing it at the live store would risk
/// writing to the user's own profile.
pub(crate) async fn export_cookies(
    staging_root: &Path,
    source: &SourceProfile,
) -> BrowserResult<Vec<ExportedCookie>> {
    let staging = staging_root.join("cookies");
    let outcome = read_cookies_through_browser(&staging, source).await;
    let _ = fs::remove_dir_all(&staging);
    outcome
}

async fn read_cookies_through_browser(
    staging: &Path,
    source: &SourceProfile,
) -> BrowserResult<Vec<ExportedCookie>> {
    let profile_copy = staging.join(COPY_PROFILE_DIR);
    fs::create_dir_all(&profile_copy).map_err(|error| copy_failed(&profile_copy, error))?;
    let source_profile = source.user_data_dir.join(&source.profile_dir);
    // `Local State` carries the key material on Windows; without it the copied
    // cookie store cannot be decrypted even by Chrome.
    copy_file(
        &source.user_data_dir.join("Local State"),
        &staging.join("Local State"),
        false,
    )?;
    for name in ["Cookies", "Cookies-journal", "Cookies-wal"] {
        copy_file(&source_profile.join(name), &profile_copy.join(name), false)?;
    }

    let installation = BrowserInstallation {
        id: source.browser_id.clone(),
        label: source.browser_label.clone(),
        executable: source.executable.to_string_lossy().into_owned(),
        version: None,
    };
    let config = BrowserLaunchConfig::new(
        installation,
        staging.to_path_buf(),
        crate::service::preferred_transport(),
    );
    let process = crate::process::launch(&config).await.map_err(|error| {
        error.with_recovery_hint(
            "The browser that wrote this profile has to be usable to read its cookies.",
        )
    })?;
    let connection = process.connection();
    let answer = connection
        .command(
            "Storage.getCookies",
            json!({}),
            COOKIE_READ_TIMEOUT.min(Duration::from_millis(BROWSER_CDP_COMMAND_TIMEOUT_MS * 4)),
        )
        .await;
    process.shutdown().await;
    let answer = answer?;
    parse_cookie_answer(&answer)
}

fn parse_cookie_answer(answer: &Value) -> BrowserResult<Vec<ExportedCookie>> {
    let cookies = answer
        .get("cookies")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BrowserError::cdp(
                "browser_cookie_read_failed",
                "the reader browser did not return a cookie list",
            )
        })?;
    Ok(cookies.iter().filter_map(parse_cookie).collect())
}

fn parse_cookie(cookie: &Value) -> Option<ExportedCookie> {
    let name = cookie.get("name")?.as_str()?.to_string();
    let domain = cookie.get("domain")?.as_str()?.to_string();
    // A cookie with no name or no domain cannot be set again.
    if name.is_empty() || domain.is_empty() {
        return None;
    }
    Some(ExportedCookie {
        name,
        value: cookie
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        domain,
        path: cookie
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .unwrap_or("/")
            .to_string(),
        expires: cookie
            .get("expires")
            .and_then(Value::as_f64)
            .unwrap_or(-1.0),
        http_only: cookie
            .get("httpOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        secure: cookie
            .get("secure")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        same_site: cookie
            .get("sameSite")
            .and_then(Value::as_str)
            .map(str::to_string),
        partition_key: parse_partition_key(cookie.get("partitionKey")),
    })
}

fn parse_partition_key(value: Option<&Value>) -> Option<PartitionKey> {
    let value = value?;
    let top_level_site = value.get("topLevelSite")?.as_str()?.to_string();
    if top_level_site.is_empty() {
        return None;
    }
    Some(PartitionKey {
        top_level_site,
        // Chrome treats a partitioned cookie as cross-site unless told
        // otherwise, which is the safe default when the field is absent.
        has_cross_site_ancestor: value
            .get("hasCrossSiteAncestor")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// Reads every origin's localStorage out of a copy of the source profile.
pub(crate) fn export_local_storage(
    staging_root: &Path,
    source: &SourceProfile,
) -> BrowserResult<Vec<ExportedStorage>> {
    let source_store = source
        .user_data_dir
        .join(&source.profile_dir)
        .join("Local Storage")
        .join("leveldb");
    if !source_store.is_dir() {
        return Ok(Vec::new());
    }
    let staging = staging_root.join("storage").join("leveldb");
    let outcome = (|| {
        // The reader opens the store read-write -- it takes LevelDB's LOCK and
        // may replay the journal -- so it never sees the user's own directory.
        copy_directory(&source_store, &staging)?;
        read_local_storage(&staging)
    })();
    let _ = fs::remove_dir_all(staging_root.join("storage"));
    outcome
}

fn read_local_storage(path: &Path) -> BrowserResult<Vec<ExportedStorage>> {
    let options = Options {
        create_if_missing: false,
        ..Options::default()
    };
    let mut database = DB::open(path, options).map_err(|error| {
        BrowserError::storage(
            "browser_local_storage_read_failed",
            format!("the localStorage store could not be opened: {error}"),
        )
    })?;
    let mut iterator = database.new_iter().map_err(|error| {
        BrowserError::storage(
            "browser_local_storage_read_failed",
            format!("the localStorage store could not be read: {error}"),
        )
    })?;
    let mut by_origin: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut seen = 0usize;
    // `advance`/`current` rather than `Iterator`: the LevelDB iterator's own
    // `next` is a trait method with the same name.
    while iterator.advance() {
        let Some((key, value)) = iterator.current() else {
            continue;
        };
        seen += 1;
        if seen > MAX_LOCAL_STORAGE_ENTRIES {
            break;
        }
        let Some((origin, name)) = split_storage_key(&key) else {
            continue;
        };
        let Some(decoded) = decode_storage_value(&value) else {
            continue;
        };
        by_origin.entry(origin).or_default().push((name, decoded));
    }
    Ok(by_origin
        .into_iter()
        .map(|(origin, entries)| ExportedStorage { origin, entries })
        .collect())
}

/// Splits `_<origin>\x00<encoding><name>` into its origin and name.
///
/// A key without an encoding byte is read as UTF-8: the encoding prefix is the
/// current format, and a legacy store is worth a best-effort read rather than
/// silently importing nothing.
fn split_storage_key(key: &[u8]) -> Option<(String, String)> {
    let rest = key.strip_prefix(b"_")?;
    let separator = rest.iter().position(|byte| *byte == 0)?;
    let (origin_bytes, tail) = rest.split_at(separator);
    let tail = tail.strip_prefix(&[0u8])?;
    if origin_bytes.is_empty() {
        return None;
    }
    let origin = std::str::from_utf8(origin_bytes).ok()?.to_string();
    let name = match tail.split_first() {
        Some((0, bytes)) => decode_utf16(bytes)?,
        Some((1, bytes)) => decode_single_byte(bytes),
        _ => std::str::from_utf8(tail).ok()?.to_string(),
    };
    if name.is_empty() {
        return None;
    }
    Some((origin, name))
}

/// Decodes `<encoding><value>`; `None` for an unknown encoding byte.
fn decode_storage_value(value: &[u8]) -> Option<String> {
    match value.split_first() {
        Some((0, bytes)) => decode_utf16(bytes),
        Some((1, bytes)) => Some(decode_single_byte(bytes)),
        _ => None,
    }
}

/// UTF-16LE, as Chrome stores a value that has a character outside Latin-1.
fn decode_utf16(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Chrome's "Latin-1" is one code unit per byte, not a Windows code page.
fn decode_single_byte(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// A profile directory the browser itself lists, in the order it lists them.
#[derive(Debug, Clone)]
struct ProfileEntry {
    dir: String,
    name: String,
}

fn profile_directories(root: &Path) -> Vec<ProfileEntry> {
    let mut entries = profiles_from_local_state(root);
    if entries.is_empty() {
        entries = profiles_from_disk(root);
    }
    entries.retain(|entry| root.join(&entry.dir).is_dir());
    // A profile the browser has forgotten about is still readable, so a
    // directory with a cookie store that `Local State` does not list is added
    // rather than hidden.
    for entry in profiles_from_disk(root) {
        if !entries.iter().any(|known| known.dir == entry.dir) {
            entries.push(entry);
        }
    }
    entries
}

fn profiles_from_local_state(root: &Path) -> Vec<ProfileEntry> {
    let Ok(raw) = fs::read_to_string(root.join("Local State")) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(info_cache) = state
        .pointer("/profile/info_cache")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let order: Vec<String> = state
        .pointer("/profile/profiles_order")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut directories = Vec::new();
    for dir in order {
        if let Some(entry) = profile_entry(info_cache, &dir) {
            directories.push(entry);
        }
    }
    let mut remaining: Vec<String> = info_cache.keys().cloned().collect();
    remaining.sort();
    for dir in remaining {
        if !directories.iter().any(|entry| entry.dir == dir)
            && let Some(entry) = profile_entry(info_cache, &dir)
        {
            directories.push(entry);
        }
    }
    directories
}

fn profile_entry(info_cache: &serde_json::Map<String, Value>, dir: &str) -> Option<ProfileEntry> {
    if !is_plain_component(dir) {
        return None;
    }
    let details = info_cache.get(dir);
    let name = details
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            details
                .and_then(|value| value.get("gaia_name"))
                .and_then(Value::as_str)
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(dir);
    Some(ProfileEntry {
        dir: dir.to_string(),
        name: name.to_string(),
    })
}

fn profiles_from_disk(root: &Path) -> Vec<ProfileEntry> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name == "Default" || name.starts_with("Profile "))
        .collect();
    // "Default" first, then the numbered profiles in numeric order rather than
    // the lexicographic one (`Profile 10` after `Profile 9`).
    names.sort_by_key(|name| {
        if name == "Default" {
            (0, 0)
        } else {
            let index = name
                .strip_prefix("Profile ")
                .and_then(|rest| rest.parse::<u32>().ok())
                .unwrap_or(u32::MAX);
            (1, index)
        }
    });
    names
        .into_iter()
        .map(|dir| ProfileEntry {
            name: dir.clone(),
            dir,
        })
        .collect()
}

/// Cookie rows and distinct hosts, or `None` when the store cannot be read
/// (a locked file, a schema this build does not know).
fn count_cookies(path: &Path) -> Option<(u64, u64)> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    connection.busy_timeout(Duration::from_millis(250)).ok()?;
    connection
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT host_key) FROM cookies",
            [],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )
        .ok()
}

fn directory_size(path: &Path) -> u64 {
    crate::service::tree_usage(path).1
}

fn copy_file(from: &Path, to: &Path, required: bool) -> BrowserResult<()> {
    if !from.is_file() {
        if required {
            return Err(BrowserError::storage(
                "browser_profile_source_missing",
                format!("the source profile is missing {}", from.display()),
            ));
        }
        return Ok(());
    }
    fs::copy(from, to).map_err(|error| copy_failed(from, error))?;
    Ok(())
}

fn copy_directory(from: &Path, to: &Path) -> BrowserResult<()> {
    fs::create_dir_all(to).map_err(|error| copy_failed(to, error))?;
    let entries = fs::read_dir(from).map_err(|error| copy_failed(from, error))?;
    for entry in entries.flatten() {
        let source = entry.path();
        let target = to.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            // A LevelDB store has no subdirectories; recurse defensively.
            copy_directory(&source, &target)?;
        } else if kind.is_file() {
            fs::copy(&source, &target).map_err(|error| copy_failed(&source, error))?;
        }
    }
    Ok(())
}

fn copy_failed(path: &Path, error: std::io::Error) -> BrowserError {
    BrowserError::storage(
        "browser_profile_copy_failed",
        format!("{} could not be copied: {error}", path.display()),
    )
}

/// A single path component: no separators, no `.`/`..`, no drive prefix.
fn is_plain_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\'])
        && !value.contains('\0')
}

/// Removes any staging area a previous run left behind.
///
/// The staging directory holds a copy of the user's cookie store, so an import
/// that was killed before its cleanup must not leave it on disk.
pub(crate) fn sweep_staged_copies(staging_root: &Path) {
    if staging_root.exists() {
        let _ = fs::remove_dir_all(staging_root);
    }
}

/// Creates a private staging directory for one import.
pub(crate) fn staging_directory(staging_root: &Path) -> BrowserResult<PathBuf> {
    sweep_staged_copies(staging_root);
    let directory = staging_root.join(format!("{}-{}", std::process::id(), unix_timestamp_ms()));
    fs::create_dir_all(&directory).map_err(|error| copy_failed(&directory, error))?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_keys_decode_both_encodings() {
        // Latin-1: the common case.
        let mut key = b"_https://example.com".to_vec();
        key.push(0);
        key.push(1);
        key.extend_from_slice(b"token");
        assert_eq!(
            split_storage_key(&key),
            Some(("https://example.com".to_string(), "token".to_string()))
        );

        // UTF-16LE, which Chrome uses once a name leaves Latin-1.
        let mut key = b"_https://example.com".to_vec();
        key.push(0);
        key.push(0);
        for unit in "键".encode_utf16() {
            key.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            split_storage_key(&key),
            Some(("https://example.com".to_string(), "键".to_string()))
        );
    }

    #[test]
    fn storage_keys_that_are_not_entries_are_refused() {
        assert_eq!(split_storage_key(b"METAACCESS:https://example.com"), None);
        assert_eq!(split_storage_key(b"_https://example.com"), None);
        assert_eq!(split_storage_key(b"_\x00\x01name"), None);
        assert_eq!(split_storage_key(b"_https://example.com\x00\x01"), None);
    }

    #[test]
    fn storage_values_decode_both_encodings() {
        assert_eq!(
            decode_storage_value(&[1, b'h', b'i']),
            Some("hi".to_string())
        );
        assert_eq!(
            decode_storage_value(&[0, 0x2d, 0x4e]),
            Some("中".to_string())
        );
        assert_eq!(decode_storage_value(&[]), None);
        assert_eq!(decode_storage_value(&[0, 0x2d]), None);
        assert_eq!(decode_storage_value(&[7, b'x']), None);
    }

    #[test]
    fn single_byte_decoding_is_not_a_code_page() {
        // 0x80 is one code unit, not a Windows-1252 euro sign: Chrome stores a
        // genuine euro as UTF-16 (`0x00` encoding) instead.
        assert_eq!(decode_single_byte(&[0x80, 0xff]), "\u{80}\u{ff}");
    }

    #[test]
    fn profile_directories_are_ordered_default_then_numeric() {
        let root = tempfile::tempdir().expect("temp dir");
        for name in ["Profile 10", "Default", "Profile 2"] {
            fs::create_dir_all(root.path().join(name)).expect("profile dir");
        }
        let dirs: Vec<String> = profiles_from_disk(root.path())
            .into_iter()
            .map(|entry| entry.dir)
            .collect();
        assert_eq!(dirs, vec!["Default", "Profile 2", "Profile 10"]);
    }

    #[test]
    fn cookie_answer_maps_partitioned_session_cookies() {
        let answer = serde_json::json!({
            "cookies": [
                {
                    "name": "a",
                    "value": "1",
                    "domain": ".example.com",
                    "path": "/",
                    "expires": -1.0,
                    "httpOnly": true,
                    "secure": true,
                    "sameSite": "None",
                    "partitionKey": { "topLevelSite": "https://top.example", "hasCrossSiteAncestor": true }
                },
                { "name": "", "value": "x", "domain": ".example.com" },
                { "name": "b", "value": "2", "domain": "" }
            ]
        });
        let cookies = parse_cookie_answer(&answer).expect("cookie list");
        assert_eq!(cookies.len(), 1);
        let cookie = &cookies[0];
        assert_eq!(cookie.name, "a");
        assert_eq!(cookie.expires, -1.0);
        assert!(cookie.http_only && cookie.secure);
        assert_eq!(cookie.same_site.as_deref(), Some("None"));
        let partition = cookie.partition_key.as_ref().expect("partition");
        assert_eq!(partition.top_level_site, "https://top.example");
    }

    #[test]
    fn cookie_debug_never_prints_the_value() {
        let cookie = ExportedCookie {
            name: "session".to_string(),
            value: "super-secret".to_string(),
            domain: ".example.com".to_string(),
            path: "/".to_string(),
            expires: 1.0,
            http_only: true,
            secure: true,
            same_site: None,
            partition_key: None,
        };
        let rendered = format!("{cookie:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("example.com"));
        assert!(rendered.contains("value_len"));
    }

    #[test]
    fn storage_debug_never_prints_entries_or_origin() {
        let storage = ExportedStorage {
            origin: "https://private.example".to_string(),
            entries: vec![("token".to_string(), "super-secret".to_string())],
        };
        let rendered = format!("{storage:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("private.example"));
        assert!(rendered.contains("entries: 1"));
    }

    #[test]
    fn plain_components_refuse_traversal() {
        assert!(is_plain_component("Default"));
        assert!(is_plain_component("Profile 1"));
        assert!(!is_plain_component(""));
        assert!(!is_plain_component(".."));
        assert!(!is_plain_component("../Default"));
        assert!(!is_plain_component("a/b"));
        assert!(!is_plain_component("a\\b"));
    }

    #[test]
    fn source_browser_ids_all_have_a_user_data_directory() {
        for candidate in discovery::BROWSER_CANDIDATES {
            assert!(
                !candidate.linux_user_data.is_empty(),
                "{} has no Linux user-data directory",
                candidate.id
            );
            assert!(
                !candidate.mac_user_data.is_empty(),
                "{} has no macOS user-data directory",
                candidate.id
            );
            assert!(
                !candidate.windows_user_data.is_empty(),
                "{} has no Windows user-data directory",
                candidate.id
            );
            assert!(discovery::candidate_for(candidate.id).is_some());
        }
    }
}

/// Tests that run against a real browser.
///
/// A machine without a Chromium-family browser is the only reason these may
/// skip, matching the rest of the crate's live tests. Everything else panics:
/// a silent pass here would hide the reader drifting away from what Chrome
/// actually writes.
#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::cdp::CdpSession;
    use crate::service::{BrowserService, BrowserServiceConfig};
    use std::io::{Read, Write};
    use std::sync::Arc;
    use vibex_core::BrowserLoginImportRequest;

    fn deadline() -> Duration {
        Duration::from_secs(8)
    }

    /// Serves one fixed page on a loopback port for the life of the test, so an
    /// origin can be visited and its localStorage written.
    fn spawn_page_server() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer);
                let body = "<!doctype html><title>import probe</title>ok";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        port
    }

    /// Builds a source profile with a real browser: one cookie and one
    /// localStorage entry.
    ///
    /// The caller must have established that a browser exists; every failure
    /// below panics, because a swallowed error here would read as a machine
    /// without a browser.
    async fn build_source_profile(root: &Path, origin: &str) {
        let installation = discovery::installation_by_id(None).expect("browser checked by caller");
        let config = BrowserLaunchConfig::new(
            installation,
            root.to_path_buf(),
            crate::service::preferred_transport(),
        );
        let process = crate::process::launch(&config)
            .await
            .expect("launch the source browser");
        let connection = process.connection();
        let created = connection
            .command(
                "Target.createTarget",
                json!({ "url": "about:blank" }),
                deadline(),
            )
            .await
            .expect("create the source tab");
        let target_id = created
            .get("targetId")
            .and_then(Value::as_str)
            .expect("target id")
            .to_string();
        let attached = connection
            .command(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                deadline(),
            )
            .await
            .expect("attach to the source tab");
        let session_id = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .expect("session id")
            .to_string();
        let session = CdpSession::new(Arc::clone(&connection), session_id, target_id);
        session
            .command("Page.enable", json!({}), deadline())
            .await
            .expect("enable Page");
        session
            .command("Network.enable", json!({}), deadline())
            .await
            .expect("enable Network");
        let expires = (unix_timestamp_ms() / 1000) as f64 + 3600.0;
        session
            .command(
                "Network.setCookies",
                json!({ "cookies": [{
                    "name": "session",
                    "value": "vibex-import-value",
                    "domain": "127.0.0.1",
                    "path": "/",
                    "httpOnly": true,
                    "expires": expires,
                }]}),
                deadline(),
            )
            .await
            .expect("write the source cookie");
        session
            .command("Page.navigate", json!({ "url": origin }), deadline())
            .await
            .expect("navigate the source tab");
        let mut committed = false;
        for _ in 0..80 {
            // The command helper answers with the CDP result payload already
            // unwrapped, so the value sits one level in.
            if let Ok(answer) = session
                .command(
                    "Runtime.evaluate",
                    json!({ "expression": "location.origin", "returnByValue": true }),
                    Duration::from_secs(2),
                )
                .await
                && answer.pointer("/result/value").and_then(Value::as_str) == Some(origin)
            {
                committed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(committed, "the source page never committed {origin}");
        session
            .command("DOMStorage.enable", json!({}), deadline())
            .await
            .expect("enable DOMStorage");
        session
            .command(
                "DOMStorage.setDOMStorageItem",
                json!({
                    "storageId": { "securityOrigin": origin, "isLocalStorage": true },
                    "key": "token",
                    "value": "vibex-storage-value",
                }),
                deadline(),
            )
            .await
            .expect("write the source localStorage entry");
        // A killed browser loses whatever it had not written down yet, so the
        // source is closed the way a browser is meant to close.
        let _ = connection
            .command("Browser.close", json!({}), deadline())
            .await;
        tokio::time::sleep(Duration::from_millis(750)).await;
        process.shutdown().await;
    }

    fn source_profile(root: &Path) -> SourceProfile {
        let installation = discovery::installation_by_id(None).expect("browser checked by caller");
        SourceProfile {
            browser_id: installation.id.clone(),
            browser_label: installation.label.clone(),
            profile_dir: "Default".to_string(),
            display_name: "Default".to_string(),
            user_data_dir: root.to_path_buf(),
            executable: PathBuf::from(&installation.executable),
        }
    }

    #[tokio::test]
    async fn source_state_round_trips_through_the_readers() {
        if discovery::installation_by_id(None).is_none() {
            return;
        }
        let source_root = tempfile::tempdir().expect("source dir");
        let staging_root = tempfile::tempdir().expect("staging dir");
        let port = spawn_page_server();
        let origin = format!("http://127.0.0.1:{port}");
        build_source_profile(source_root.path(), &origin).await;
        let source = source_profile(source_root.path());

        let cookies = export_cookies(staging_root.path(), &source)
            .await
            .expect("cookie export");
        let cookie = cookies
            .iter()
            .find(|cookie| cookie.name == "session")
            .expect("the source cookie was not read back");
        assert_eq!(cookie.value, "vibex-import-value");
        assert!(cookie.http_only);
        assert_eq!(cookie.domain, "127.0.0.1");

        let storages = export_local_storage(staging_root.path(), &source).expect("storage export");
        let entries = storages
            .iter()
            .find(|storage| storage.origin == origin)
            .map(|storage| storage.entries.clone())
            .expect("the source origin was not read back");
        assert!(
            entries
                .iter()
                .any(|(key, value)| key == "token" && value == "vibex-storage-value"),
            "the source localStorage entry was not read back: {entries:?}"
        );
    }

    #[tokio::test]
    async fn importing_writes_the_state_into_the_isolated_profile() {
        if discovery::installation_by_id(None).is_none() {
            return;
        }
        let source_root = tempfile::tempdir().expect("source dir");
        let home = tempfile::tempdir().expect("runtime home");
        let port = spawn_page_server();
        let origin = format!("http://127.0.0.1:{port}");
        build_source_profile(source_root.path(), &origin).await;
        let source = source_profile(source_root.path());

        let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
        let staging_root = home.path().join("browser").join("import");
        let staging = staging_directory(&staging_root).expect("staging");
        let request = BrowserLoginImportRequest {
            browser_id: source.browser_id.clone(),
            profile_dir: source.profile_dir.clone(),
            include_cookies: true,
            include_local_storage: true,
        };
        let report = service
            .import_from_source(&request, &source, &staging, std::time::Instant::now())
            .await
            .expect("import");
        assert!(report.cookies_imported >= 1, "no cookie was imported");
        assert!(
            report.local_storage_entries >= 1,
            "no localStorage entry was imported"
        );
        assert_eq!(report.local_storage_origins_skipped, 0);

        // The cookie is in the isolated profile, not merely counted.
        let connection = service.connection().await.expect("connection");
        let cookies = connection
            .command("Storage.getCookies", json!({}), deadline())
            .await
            .expect("cookies");
        let imported = cookies
            .get("cookies")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter().any(|cookie| {
                    cookie.get("name").and_then(Value::as_str) == Some("session")
                        && cookie.get("value").and_then(Value::as_str) == Some("vibex-import-value")
                })
            })
            .unwrap_or(false);
        assert!(
            imported,
            "the imported cookie is not in the isolated profile"
        );

        // Nothing this import staged outlives it: the staging tree holds a copy
        // of the user's own cookie store.
        assert!(
            !staging.exists(),
            "the staged cookie copy outlived the import"
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn clearing_removes_the_whole_browser_profile() {
        let home = tempfile::tempdir().expect("runtime home");
        let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
        let profile_dir = home.path().join("browser").join("profiles").join("chrome");
        fs::create_dir_all(&profile_dir).expect("profile dir");
        fs::write(profile_dir.join("Cookies"), b"not really a cookie store").expect("seed file");

        let report = service.clear_browser_data().await.expect("clear");
        assert_eq!(report.removed_files, 1);
        assert!(report.reclaimed_bytes > 0);
        assert!(!home.path().join("browser").exists());
        // Clearing is not shutdown: the service still works afterwards.
        assert!(service.clear_browser_data().await.is_ok());
    }
}

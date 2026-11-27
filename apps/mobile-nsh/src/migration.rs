//! Imports the files the GPUI mobile client left on the phone.
//!
//! This client ships as the same application (`ai.vibex.mobile`), so the
//! system replaces the GPUI build in place and the phone keeps its files. That
//! is also why the old files are still on disk but not where `nsh` keeps its
//! state: `nsh` writes `<support>/nsh/<app id>`, while the GPUI build wrote at
//! the top of the files directory on Android and in `Application Support/Vibex`
//! on iOS. [`legacy_dirs_in`] walks to both, so the imports below find them.
//!
//! What can be carried over is carried over once: the saved credentials and
//! their names/timestamps, the install-wide device identity, the appearance and
//! language preferences, and the unsent composer drafts. The old files are left
//! in place; nothing here deletes them, and a second launch finds the new store
//! already filled and imports nothing.

use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use nsh_core::storage::Storage;
use serde::{Deserialize, Serialize};
use vibex_core::RemoteServerKind;
use vibex_mobile_pairing::MobileCredentialBundle;
use vibex_remote_client::ClientDeviceIdentity;

/// The old app's files.
const LEGACY_HOSTS: &str = "remote-hosts.json";
const LEGACY_CREDENTIAL: &str = "remote-credentials.json";
const LEGACY_IDENTITY: &str = "client-identity.json";
const LEGACY_SETTINGS: &str = "app-settings.json";
const LEGACY_COMPOSER: &str = "composer-state.json";
const LEGACY_IDENTITY_SCHEMA: &str = "vibex-native-mobile-client-identity.v1";

/// The directory name the GPUI client gave its own data on iOS
/// (`$HOME/Library/Application Support/Vibex`).
const LEGACY_IOS_FOLDER: &str = "Vibex";

/// Where the GPUI client's files may be on `os`, best first.
///
/// `nsh` keeps an app in `<support>/<app id>`, below the directory the platform
/// handed it, while the GPUI client wrote into a directory of its own:
///
/// - Android: `nsh` is `<filesDir>/nsh/ai.vibex.mobile`, the old client wrote
///   `<filesDir>` itself — the new directory's grandparent.
/// - iOS: `nsh` is `<Application Support>/nsh/ai.vibex.mobile`, the old client
///   wrote `<Application Support>/Vibex` — a named folder, because
///   `Application Support` is shared with every other app on the device.
///
/// The app's own directory always comes first, so a host that pointed `nsh` at
/// the old directory keeps working. Old files that are not there make every
/// import a no-op, which is what a fresh install sees.
fn legacy_dirs_in(os: &str, data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![data_dir.to_path_buf()];
    let Some(parent) = data_dir.parent() else {
        return dirs;
    };
    // Only `nsh`'s own directory says where the platform root is: a host that
    // pointed the app somewhere else (the old directory itself, a test temp
    // dir) gets no ancestor searched, so nothing outside the app is ever read.
    if parent.file_name().is_none_or(|name| name != "nsh") {
        return dirs;
    }
    let Some(root) = parent.parent() else {
        return dirs;
    };
    match os {
        // The GPUI client kept its files at the top of the app's own files
        // directory, which no other app can see.
        "android" => dirs.push(root.to_path_buf()),
        // A named folder, not the shared Application Support itself: another
        // app's `app-settings.json` must never be read as this one's.
        "ios" | "macos" => dirs.push(root.join(LEGACY_IOS_FOLDER)),
        // No other platform ran the GPUI mobile client.
        _ => {}
    }
    dirs
}

/// The first place `name` exists, in [`legacy_dirs_in`] order.
fn legacy_file_in(os: &str, data_dir: &Path, name: &str) -> Option<PathBuf> {
    legacy_dirs_in(os, data_dir)
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// One runtime imported from the old hosts file.
#[derive(Clone, Debug)]
pub struct ImportedHost {
    pub bundle: MobileCredentialBundle,
    pub name_override: Option<String>,
    pub added_at_ms: i64,
    pub last_connected_at_ms: Option<i64>,
    pub server_kind: RemoteServerKind,
}

/// What the old hosts file carried.
#[derive(Clone, Debug, Default)]
pub struct ImportedHosts {
    pub hosts: Vec<ImportedHost>,
    /// The runtime to reopen, when the file says one was used more recently.
    pub last_host: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyHostsV2 {
    #[serde(default)]
    hosts: Vec<LegacyHostEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyHostEntry {
    bundle: MobileCredentialBundle,
    #[serde(default)]
    name_override: Option<String>,
    #[serde(default)]
    added_at_ms: i64,
    #[serde(default)]
    last_connected_at_ms: Option<i64>,
    #[serde(default)]
    server_kind: RemoteServerKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyHostsV1 {
    #[serde(default)]
    hosts: Vec<MobileCredentialBundle>,
}

/// Reads the saved runtimes, preferring the hosts file and falling back to the
/// single-credential file an older build wrote.
pub fn import_hosts(data_dir: &Path) -> Option<ImportedHosts> {
    import_hosts_in(std::env::consts::OS, data_dir)
}

fn import_hosts_in(os: &str, data_dir: &Path) -> Option<ImportedHosts> {
    let mut hosts = legacy_file_in(os, data_dir, LEGACY_HOSTS)
        .map(|path| read_hosts(&path))
        .unwrap_or_default();
    if hosts.is_empty()
        && let Some(path) = legacy_file_in(os, data_dir, LEGACY_CREDENTIAL)
        && let Some(bundle) = read_single_credential(&path)
    {
        hosts.push(ImportedHost {
            bundle,
            name_override: None,
            added_at_ms: 0,
            last_connected_at_ms: None,
            server_kind: RemoteServerKind::Unknown,
        });
    }
    if hosts.is_empty() {
        return None;
    }

    // One entry per runtime: a re-pair can leave two records for one server id,
    // and the newest one is the credential that still works.
    let mut by_host: BTreeMap<String, ImportedHost> = BTreeMap::new();
    for host in hosts {
        if host.bundle.validate().is_err() {
            continue;
        }
        let id = host.bundle.host_id().to_owned();
        match by_host.get(&id) {
            Some(existing)
                if existing
                    .last_connected_at_ms
                    .unwrap_or(existing.added_at_ms)
                    >= host.last_connected_at_ms.unwrap_or(host.added_at_ms) => {}
            _ => {
                by_host.insert(id, host);
            }
        }
    }
    let mut hosts = by_host.into_values().collect::<Vec<_>>();
    hosts.sort_by(|left, right| {
        right
            .last_connected_at_ms
            .unwrap_or(right.added_at_ms)
            .cmp(&left.last_connected_at_ms.unwrap_or(left.added_at_ms))
    });
    if hosts.is_empty() {
        return None;
    }
    let last_host = hosts
        .iter()
        .max_by_key(|host| host.last_connected_at_ms.unwrap_or(host.added_at_ms))
        .filter(|host| host.last_connected_at_ms.is_some() || host.added_at_ms > 0)
        .map(|host| host.bundle.host_id().to_owned());
    Some(ImportedHosts { hosts, last_host })
}

fn read_hosts(path: &Path) -> Vec<ImportedHost> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    if let Ok(file) = serde_json::from_str::<LegacyHostsV2>(&text) {
        return file
            .hosts
            .into_iter()
            .map(|host| ImportedHost {
                bundle: host.bundle,
                name_override: host.name_override,
                added_at_ms: host.added_at_ms,
                last_connected_at_ms: host.last_connected_at_ms,
                server_kind: host.server_kind,
            })
            .collect();
    }
    if let Ok(file) = serde_json::from_str::<LegacyHostsV1>(&text) {
        return file
            .hosts
            .into_iter()
            .map(|bundle| ImportedHost {
                bundle,
                name_override: None,
                added_at_ms: 0,
                last_connected_at_ms: None,
                server_kind: RemoteServerKind::Unknown,
            })
            .collect();
    }
    Vec::new()
}

fn read_single_credential(path: &Path) -> Option<MobileCredentialBundle> {
    serde_json::from_str::<MobileCredentialBundle>(&fs::read_to_string(path).ok()?).ok()
}

/// The old app's appearance and language preferences.
#[derive(Clone, Debug, Default)]
pub struct ImportedSettings {
    pub theme: Option<String>,
    pub language: Option<String>,
    pub reduce_motion: Option<bool>,
    pub haptics: Option<bool>,
    pub keep_screen_on: Option<bool>,
    pub font_scale_percent: Option<u32>,
    pub notifications: Option<bool>,
    pub notify_completed: Option<bool>,
    pub notify_needs_input: Option<bool>,
    pub notify_failed: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySettingsFile {
    #[serde(default)]
    schema_version: Option<String>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    reduce_motion: Option<bool>,
    #[serde(default)]
    haptics: Option<bool>,
    #[serde(default)]
    keep_screen_on: Option<bool>,
    #[serde(default)]
    font_scale_percent: Option<u32>,
    #[serde(default)]
    notifications: Option<bool>,
    #[serde(default)]
    notify_completed: Option<bool>,
    #[serde(default)]
    notify_needs_input: Option<bool>,
    #[serde(default)]
    notify_failed: Option<bool>,
}

/// The v1 settings file wrote all four notification switches as `false`
/// because the defaults it serialized were `false`, not because the reader
/// turned them off. Carrying that shape over would silence a phone that was
/// never asked, so it is repaired to "on" here exactly as the GPUI client
/// repaired it in place.
const LEGACY_SETTINGS_V1: &str = "vibex-native-mobile-app-settings.v1";

/// Reads the old settings document, if it is there.
pub fn import_settings(data_dir: &Path) -> Option<ImportedSettings> {
    import_settings_in(std::env::consts::OS, data_dir)
}

fn import_settings_in(os: &str, data_dir: &Path) -> Option<ImportedSettings> {
    let path = legacy_file_in(os, data_dir, LEGACY_SETTINGS)?;
    let text = fs::read_to_string(path).ok()?;
    let file = serde_json::from_str::<LegacySettingsFile>(&text).ok()?;
    let derived_defaults = file.schema_version.as_deref() == Some(LEGACY_SETTINGS_V1)
        && file.notifications == Some(false)
        && file.notify_completed == Some(false)
        && file.notify_needs_input == Some(false)
        && file.notify_failed == Some(false);
    let switch = |stored: Option<bool>| {
        if derived_defaults { Some(true) } else { stored }
    };
    Some(ImportedSettings {
        theme: file.theme,
        language: file.language,
        reduce_motion: file.reduce_motion,
        haptics: file.haptics,
        keep_screen_on: file.keep_screen_on,
        font_scale_percent: file.font_scale_percent,
        notifications: switch(file.notifications),
        notify_completed: switch(file.notify_completed),
        notify_needs_input: switch(file.notify_needs_input),
        notify_failed: switch(file.notify_failed),
    })
}

/// Reads the unsent composer drafts, keyed by session id.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyComposerFile {
    #[serde(default)]
    sessions: Vec<LegacyComposerSession>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyComposerSession {
    session_id: String,
    #[serde(default)]
    draft: String,
}

/// The key the new composer keeps its per-session drafts under.
const NEW_DRAFTS_KEY: &str = "vibex.mobile.drafts.v1";

/// Writes the old drafts into the new store when it has none yet.
pub fn migrate_drafts(data_dir: &Path) {
    if Storage::get_item(NEW_DRAFTS_KEY).is_some() {
        return;
    }
    let drafts = import_drafts(data_dir);
    if drafts.is_empty() {
        return;
    }
    if let Ok(text) = serde_json::to_string(&drafts) {
        let _ = Storage::set_item(NEW_DRAFTS_KEY, text);
    }
}

pub fn import_drafts(data_dir: &Path) -> BTreeMap<String, String> {
    import_drafts_in(std::env::consts::OS, data_dir)
}

fn import_drafts_in(os: &str, data_dir: &Path) -> BTreeMap<String, String> {
    let Some(path) = legacy_file_in(os, data_dir, LEGACY_COMPOSER) else {
        return BTreeMap::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(file) = serde_json::from_str::<LegacyComposerFile>(&text) else {
        return BTreeMap::new();
    };
    file.sessions
        .into_iter()
        .filter(|session| !session.draft.trim().is_empty())
        .map(|session| (session.session_id, session.draft))
        .collect()
}

/// The install-wide identity the old app used for re-pairing.
///
/// The old file carries only the private key; the device id lives in the
/// credential the key signed, so the newest bundle whose public key matches is
/// the one to present.
pub fn import_identity(
    data_dir: &Path,
    hosts: &[MobileCredentialBundle],
) -> Option<ClientDeviceIdentity> {
    import_identity_in(std::env::consts::OS, data_dir, hosts)
}

fn import_identity_in(
    os: &str,
    data_dir: &Path,
    hosts: &[MobileCredentialBundle],
) -> Option<ClientDeviceIdentity> {
    let private_key = legacy_file_in(os, data_dir, LEGACY_IDENTITY)
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<LegacyIdentityFile>(&text).ok())
        .filter(|file| file.schema_version == LEGACY_IDENTITY_SCHEMA)
        .map(|file| file.private_key)
        .filter(|key| !key.trim().is_empty());
    let key = match private_key {
        Some(key) => key,
        None => hosts
            .first()
            .map(|bundle| bundle.identity_private_key.clone())?,
    };
    for bundle in hosts {
        let Ok(identity) = ClientDeviceIdentity::from_private_key_base64(
            bundle.record.auth.device_id.clone(),
            &key,
        ) else {
            continue;
        };
        if identity.public_key_base64() == bundle.record.device_identity_public_key {
            return Some(identity);
        }
    }
    // A key that matches no stored credential is not usable; fall back to the
    // newest credential's own key rather than presenting an unrelated one.
    hosts.first().and_then(|bundle| {
        ClientDeviceIdentity::from_private_key_base64(
            bundle.record.auth.device_id.clone(),
            &bundle.identity_private_key,
        )
        .ok()
    })
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LegacyIdentityFile {
    #[serde(default)]
    schema_version: String,
    private_key: String,
}

/// Keeps the identity a completed pairing used, so a re-pair updates the entry
/// the runtime already has for this phone.
pub fn save_identity(data_dir: &Path, bundle: &MobileCredentialBundle) {
    let file = LegacyIdentityFile {
        schema_version: LEGACY_IDENTITY_SCHEMA.to_owned(),
        private_key: bundle.identity_private_key.clone(),
    };
    let Ok(encoded) = serde_json::to_vec(&file) else {
        return;
    };
    let path: PathBuf = data_dir.join(LEGACY_IDENTITY);
    let temporary = data_dir.join(format!("{LEGACY_IDENTITY}.tmp"));
    let _ = fs::remove_file(&temporary);
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let Ok(mut handle) = options.open(&temporary) else {
        return;
    };
    if handle
        .write_all(&encoded)
        .and_then(|_| handle.sync_all())
        .is_ok()
    {
        let _ = fs::rename(&temporary, &path);
    } else {
        let _ = fs::remove_file(&temporary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(directory: &Path, name: &str, value: &str) {
        fs::write(directory.join(name), value).unwrap();
    }

    #[test]
    fn settings_and_drafts_are_read_from_the_old_files() {
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            LEGACY_SETTINGS,
            r#"{"schemaVersion":"vibex-native-mobile-app-settings.v3","theme":"dark","language":"zh-cn","haptics":false,"keepScreenOn":true,"fontScalePercent":115}"#,
        );
        write(
            directory.path(),
            LEGACY_COMPOSER,
            r#"{"schemaVersion":"vibex-native-mobile-composer.v1","sessions":[{"sessionId":"s1","draft":"hello"},{"sessionId":"s2","draft":"  "}]}"#,
        );
        let settings = import_settings(directory.path()).unwrap();
        assert_eq!(settings.theme.as_deref(), Some("dark"));
        assert_eq!(settings.language.as_deref(), Some("zh-cn"));
        assert_eq!(settings.haptics, Some(false));
        assert_eq!(settings.keep_screen_on, Some(true));
        assert_eq!(settings.font_scale_percent, Some(115));
        let drafts = import_drafts(directory.path());
        assert_eq!(drafts.get("s1").map(String::as_str), Some("hello"));
        assert!(!drafts.contains_key("s2"));
    }

    /// The Android layout after an in-place upgrade: `nsh` keeps
    /// `<filesDir>/nsh/ai.vibex.mobile` while the GPUI build wrote into
    /// `<filesDir>` itself.
    #[test]
    fn the_android_upgrade_finds_the_old_files_above_the_new_directory() {
        let files = tempfile::tempdir().unwrap();
        let data = files.path().join("nsh").join("ai.vibex.mobile");
        fs::create_dir_all(&data).unwrap();
        write(
            files.path(),
            LEGACY_SETTINGS,
            r#"{"schemaVersion":"vibex-native-mobile-app-settings.v3","theme":"system","language":"ja"}"#,
        );
        write(
            files.path(),
            LEGACY_COMPOSER,
            r#"{"schemaVersion":"vibex-native-mobile-composer.v1","sessions":[{"sessionId":"s1","draft":"kept"}]}"#,
        );
        write(
            files.path(),
            LEGACY_IDENTITY,
            r#"{"schemaVersion":"vibex-native-mobile-client-identity.v1","privateKey":"unused"}"#,
        );
        let settings =
            import_settings_in("android", &data).expect("the old settings, two levels up");
        assert_eq!(settings.theme.as_deref(), Some("system"));
        assert_eq!(settings.language.as_deref(), Some("ja"));
        assert_eq!(
            import_drafts_in("android", &data)
                .get("s1")
                .map(String::as_str),
            Some("kept")
        );
        assert!(legacy_file_in("android", &data, LEGACY_IDENTITY).is_some());
        // Nothing is moved or deleted: a re-launch re-reads the same files.
        assert!(files.path().join(LEGACY_SETTINGS).is_file());
        assert!(!data.join(LEGACY_SETTINGS).exists());
    }

    /// The iOS layout after an in-place upgrade: `nsh` keeps
    /// `<Application Support>/nsh/ai.vibex.mobile` while the GPUI build wrote
    /// into `<Application Support>/Vibex`.
    #[test]
    fn the_ios_upgrade_finds_the_old_directory_beside_nsh() {
        let support = tempfile::tempdir().unwrap();
        let data = support.path().join("nsh").join("ai.vibex.mobile");
        fs::create_dir_all(&data).unwrap();
        let old = support.path().join(LEGACY_IOS_FOLDER);
        fs::create_dir_all(&old).unwrap();
        write(
            &old,
            LEGACY_SETTINGS,
            r#"{"schemaVersion":"vibex-native-mobile-app-settings.v3","theme":"dark"}"#,
        );
        let settings = import_settings_in("ios", &data)
            .expect("the old settings in Application Support/Vibex");
        assert_eq!(settings.theme.as_deref(), Some("dark"));
    }

    /// The v1 settings file wrote `false` into every notification switch
    /// because that is what the defaults it serialized were. The reader never
    /// chose that, so the upgrade must not silence the phone.
    #[test]
    fn a_v1_settings_file_does_not_carry_its_derived_off_switches() {
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            LEGACY_SETTINGS,
            r#"{"schemaVersion":"vibex-native-mobile-app-settings.v1","theme":"dark","notifications":false,"notifyCompleted":false,"notifyNeedsInput":false,"notifyFailed":false}"#,
        );
        let settings = import_settings(directory.path()).unwrap();
        assert_eq!(settings.notifications, Some(true));
        assert_eq!(settings.notify_completed, Some(true));
        assert_eq!(settings.notify_needs_input, Some(true));
        assert_eq!(settings.notify_failed, Some(true));
        assert_eq!(settings.theme.as_deref(), Some("dark"));
    }

    /// A reader who really turned every switch off wrote a v3 file, and that
    /// choice survives.
    #[test]
    fn a_v3_settings_file_keeps_switches_that_were_turned_off() {
        let directory = tempfile::tempdir().unwrap();
        write(
            directory.path(),
            LEGACY_SETTINGS,
            r#"{"schemaVersion":"vibex-native-mobile-app-settings.v3","notifications":false,"notifyCompleted":false,"notifyNeedsInput":false,"notifyFailed":false,"reduceMotion":true}"#,
        );
        let settings = import_settings(directory.path()).unwrap();
        assert_eq!(settings.notifications, Some(false));
        assert_eq!(settings.notify_completed, Some(false));
        assert_eq!(settings.reduce_motion, Some(true));
    }

    /// Only `nsh`'s own layout makes an ancestor a place to look. A host that
    /// keeps the app somewhere else must not pick up an unrelated
    /// `app-settings.json` that happens to sit above it.
    #[test]
    fn an_unrelated_ancestor_is_not_searched() {
        let elsewhere = tempfile::tempdir().unwrap();
        write(
            elsewhere.path(),
            LEGACY_SETTINGS,
            r#"{"schemaVersion":"vibex-native-mobile-app-settings.v3","theme":"dark"}"#,
        );
        let data = elsewhere.path().join("some-host").join("ai.vibex.mobile");
        fs::create_dir_all(&data).unwrap();
        assert!(import_settings(&data).is_none());
        assert!(legacy_file_in("android", &data, LEGACY_SETTINGS).is_none());
    }

    /// The new store wins: once the app has saved its own settings, the old
    /// file is not read again on every launch.
    #[test]
    fn an_empty_old_directory_imports_nothing() {
        let files = tempfile::tempdir().unwrap();
        let data = files.path().join("nsh").join("ai.vibex.mobile");
        fs::create_dir_all(&data).unwrap();
        assert!(import_settings(&data).is_none());
        assert!(import_drafts(&data).is_empty());
        assert!(legacy_file_in("android", &data, LEGACY_IDENTITY).is_none());
    }

    fn valid_bundle(server_id: &str) -> MobileCredentialBundle {
        use vibex_core::{DeviceId, RemoteAuthProof, RemoteClientType};
        use vibex_mobile_pairing::{MOBILE_CREDENTIAL_SCHEMA_VERSION, MobileRemoteRouteBundle};
        use vibex_remote_client::RemoteCredentialRecord;

        let device_id = DeviceId::new();
        let identity = ClientDeviceIdentity::generate(device_id.clone()).unwrap();
        let server = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
        MobileCredentialBundle {
            schema_version: MOBILE_CREDENTIAL_SCHEMA_VERSION.to_owned(),
            record: RemoteCredentialRecord {
                server_url: "https://desktop.example".to_owned(),
                auth: RemoteAuthProof {
                    device_id,
                    auth_token: "grant".to_owned(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some(server.public_key_base64()),
            },
            identity_private_key: identity.private_key_base64(),
            expected_server_id: server_id.to_owned(),
            client_type: RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            display_name: Some("Studio".to_owned()),
            route: Some(MobileRemoteRouteBundle {
                local_network: None,
                direct_candidates: vec!["https://desktop.example".to_owned()],
                relay: None,
            }),
        }
    }

    #[test]
    fn valid_legacy_hosts_are_imported_newest_first() {
        let directory = tempfile::tempdir().unwrap();
        let older = valid_bundle("server-older");
        let newer = valid_bundle("server-newer");
        let file = serde_json::json!({
            "schemaVersion": "vibex-native-mobile-hosts.v2",
            "hosts": [
                {
                    "bundle": older,
                    "nameOverride": "Old studio",
                    "addedAtMs": 10,
                    "lastConnectedAtMs": 20
                },
                {
                    "bundle": newer,
                    "addedAtMs": 30,
                    "lastConnectedAtMs": 40
                }
            ]
        });
        write(directory.path(), LEGACY_HOSTS, &file.to_string());
        let imported = import_hosts(directory.path()).unwrap();
        assert_eq!(imported.hosts.len(), 2);
        assert_eq!(imported.hosts[0].bundle.host_id(), "server-newer");
        assert_eq!(
            imported.hosts[1].name_override.as_deref(),
            Some("Old studio")
        );
        assert_eq!(imported.last_host.as_deref(), Some("server-newer"));
        let identity = import_identity(
            directory.path(),
            &imported
                .hosts
                .iter()
                .map(|host| host.bundle.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            identity.public_key_base64(),
            newer.record.device_identity_public_key
        );
    }

    #[test]
    fn a_missing_old_file_imports_nothing() {
        let directory = tempfile::tempdir().unwrap();
        assert!(import_settings(directory.path()).is_none());
        assert!(import_drafts(directory.path()).is_empty());
        assert!(import_hosts(directory.path()).is_none());
        assert!(import_identity(directory.path(), &[]).is_none());
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_backend::{BackendError, BackendResult};
use vibex_core::{
    AgentTimelineReasoningDisplayMode, RemoteServerKind, TimelineItem, VibexSessionId,
};
use vibex_desktop_model::{
    RUNTIME_MODEL_FAVORITE_LIMIT, RUNTIME_SELECTION_USAGE_LIMIT, RuntimeModelFavorite,
    ThemeSelection,
};
use vibex_remote_client::ClientDeviceIdentity;

use crate::pairing::MobileCredentialBundle;

const CREDENTIAL_FILE: &str = "remote-credentials.json";
const HOSTS_FILE: &str = "remote-hosts.json";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const MAX_HOSTS: usize = 32;
const MAX_HOST_BYTES: u64 = 2 * 1024 * 1024;
const HOSTS_SCHEMA_VERSION: &str = "vibex-native-mobile-hosts.v2";
/// The v1 hosts file stored a bare credential bundle per host. It is still
/// accepted and upgraded in place, so a phone that paired before the runtime
/// list gained names and timestamps keeps its runtimes.
const HOSTS_SCHEMA_VERSION_V1: &str = "vibex-native-mobile-hosts.v1";
const TIMELINE_DISPLAY_SETTINGS_FILE: &str = "timeline-display-settings.json";
const TIMELINE_DISPLAY_SETTINGS_SCHEMA_VERSION: &str =
    "vibex-native-mobile-timeline-display-settings.v1";
const MAX_TIMELINE_DISPLAY_SETTINGS_BYTES: u64 = 128 * 1024;
const MAX_TIMELINE_DISPLAY_SETTINGS_OVERRIDES: usize = 32;
const MAX_TIMELINE_DISPLAY_SETTINGS_HOST_ID_BYTES: usize = 256;
const APP_SETTINGS_FILE: &str = "app-settings.json";
const APP_SETTINGS_SCHEMA_VERSION: &str = "vibex-native-mobile-app-settings.v3";
/// The v1 settings file carries the notification switches that the build which
/// wrote it derived from `Default`, which was `false` for all four. It is still
/// read, so a phone that upgraded keeps its appearance and language, but the
/// switches are not carried over — see [`CredentialStorage::load_app_settings`].
const APP_SETTINGS_SCHEMA_VERSION_V1: &str = "vibex-native-mobile-app-settings.v1";
/// The v2 file is the same record without the picker's stars and usage counts.
const APP_SETTINGS_SCHEMA_VERSION_V2: &str = "vibex-native-mobile-app-settings.v2";
/// Stars and frequency counts live beside the settings, so the bound has to
/// cover a full favourites list and a bounded usage table rather than only the
/// handful of switches it held before.
const MAX_APP_SETTINGS_BYTES: u64 = 256 * 1024;
/// The summary window of the sessions this phone opened last, kept so a cold
/// start paints the conversation before the network answers. One file rather
/// than one per session: the set is small and bounded, and eviction has to see
/// every record to drop the oldest.
const SESSION_TIMELINES_FILE: &str = "session-timelines.json";
const SESSION_TIMELINES_SCHEMA_VERSION: &str = "vibex-native-mobile-session-timelines.v1";
/// At most four conversations: a reader reopens the handful they were last in,
/// and everything here is re-fetchable.
const MAX_SESSION_TIMELINES: usize = 4;
/// A hard cap on the whole file. A window is one page or a few, so this bounds
/// the cache well above the common case while keeping a runaway projection
/// from filling the phone.
const MAX_SESSION_TIMELINES_BYTES: u64 = 6 * 1024 * 1024;
/// The newest items of one session's window that are worth keeping. A cold open
/// only shows the tail; older items would be re-fetched on the same gesture
/// that scrolls to them.
pub const MAX_SESSION_TIMELINE_ITEMS: usize = 512;
const MAX_SESSION_TIMELINE_ID_BYTES: usize = 256;
/// The install-wide device identity a pairing presents. It is separate from
/// the per-host credential bundles because it outlives any single pairing and
/// is what lets the desktop recognise a re-pairing as the same client.
const CLIENT_IDENTITY_FILE: &str = "client-identity.json";
const CLIENT_IDENTITY_SCHEMA_VERSION: &str = "vibex-native-mobile-client-identity.v1";
const MAX_CLIENT_IDENTITY_BYTES: u64 = 1024;
/// Upper bound on stored preference strings; every value written by the
/// settings UI is one of a fixed enum, so this only guards corruption.
const MAX_APP_SETTINGS_VALUE_BYTES: usize = 32;
/// The composer's un-sent work, per session: the draft, the files waiting on
/// it, and the prompts queued behind a running turn.
///
/// The desktop keeps its composer in memory because it is the process that
/// owns the session; a phone process is killed in the background without
/// warning, so anything typed and not sent has to survive on disk.
const COMPOSER_STATE_FILE: &str = "composer-state.json";
const COMPOSER_STATE_SCHEMA_VERSION: &str = "vibex-native-mobile-composer.v1";
/// How many sessions' composers are worth keeping. A reader moves between a
/// handful of conversations, and the oldest is dropped first.
const MAX_COMPOSER_SESSIONS: usize = 16;
/// The whole file, which is dominated by base64 attachments.
const MAX_COMPOSER_STATE_BYTES: u64 = 8 * 1024 * 1024;
/// The attachments the file may carry in total. Past this the text is kept and
/// the files are dropped, because the words are what cannot be retyped.
const MAX_COMPOSER_ATTACHMENT_BYTES: usize = 4 * 1024 * 1024;
/// One prompt's text. The authority rejects longer prompts anyway; this only
/// keeps a corrupt or runaway value out of the file.
const MAX_COMPOSER_TEXT_BYTES: usize = 256 * 1024;
const MAX_COMPOSER_SESSION_ID_BYTES: usize = 256;

/// One file waiting on a draft, exactly as the picker handed it over.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredComposerAttachment {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The `data:<mime>;base64,<payload>` URI the picker produced.
    pub data_url: String,
    /// Where the picture sat in the draft, in UTF-16 units, so a restored
    /// draft keeps the order the reader wrote it in. Absent on records written
    /// before the offset travelled, which then read as an attachment at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_text_offset: Option<u32>,
}

/// One prompt held behind a running turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredComposerQueuedMessage {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<StoredComposerAttachment>,
    /// The `@` references the reader wrote, as the catalogue resolved them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<vibex_core::VibexUseMention>,
}

/// One session's composer.
///
/// The session is named by the composer's own draft key — a session id as text
/// — rather than a parsed `VibexSessionId`, so one record naming something this
/// build cannot make sense of is dropped on its own instead of failing the
/// whole file and taking every other session's work with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredComposerSession {
    pub session_id: String,
    pub saved_at_ms: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<StoredComposerAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<StoredComposerQueuedMessage>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredComposerStateFile {
    schema_version: String,
    sessions: Vec<StoredComposerSession>,
}

/// Trims one piece of composer text to what the file will carry, cutting on a
/// character boundary so the result is still valid text.
fn clamp_composer_text(text: String) -> String {
    if text.len() <= MAX_COMPOSER_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_COMPOSER_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Mobile-owned appearance and language preferences. When a value is `None`
/// the mobile client follows the platform (system appearance and system
/// locale), matching the desktop's `System` defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSettings {
    pub theme: Option<crate::theme::AppearanceMode>,
    pub language: Option<crate::locale::LanguagePreference>,
    /// The light/dark palette pair, mirroring what the desktop selected so the
    /// phone can paint the same catalog entries instead of always falling back
    /// to the catalog default.
    pub theme_selection: ThemeSelection,
    /// Collapses every transition in the interface. Mobile is the one surface
    /// where the OS setting is not automatically the app's.
    pub reduce_motion: bool,
    /// Holds the display awake while a turn is running or the terminal is on
    /// screen. A phone that sleeps mid-run stops showing the thing the reader
    /// opened it to watch.
    pub keep_screen_on: bool,
    /// Plays the device's own feedback for gestures and finished actions. On by
    /// default: the switch exists so a reader who does not want it can say so.
    /// It is independent of `reduce_motion` — a reader may want the animations
    /// gone and still want the confirmation in their hand.
    pub haptics: bool,
    /// The text-size step, as a percentage of the type ladder. The platform's
    /// own text-scaling setting still applies on top of it.
    pub font_scale_percent: u32,
    /// The notification categories the user wants to hear about. `notifications`
    /// is the master switch; the three below gate the individual events.
    pub notifications: bool,
    pub notify_completed: bool,
    pub notify_needs_input: bool,
    pub notify_failed: bool,
    /// Starred Agent + auth source + model triples, oldest first. The picker
    /// keeps them on the phone because the desktop's own stars travel through
    /// the UI-state projection, which a remote client cannot write.
    pub favorite_runtime_models: Vec<RuntimeModelFavorite>,
    /// How often each Agent + auth source + model triple has been chosen. The
    /// picker orders Providers, routes, and models by these counts.
    pub runtime_selection_usage: BTreeMap<String, u64>,
}

/// What a phone with no stored settings file starts on.
///
/// Written by hand rather than derived because the text-size step and the four
/// notification switches do not start at `0` / `false`. A derived default gave
/// a fresh install 0% text — which the rem base clamps to a step the setting
/// does not even offer — and left notifications off until the reader found the
/// switch, even though pairing requests authorization on its own.
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: None,
            language: None,
            theme_selection: ThemeSelection::default(),
            reduce_motion: false,
            keep_screen_on: false,
            haptics: true,
            font_scale_percent: crate::theme::FONT_SCALE_INITIAL,
            notifications: true,
            notify_completed: true,
            notify_needs_input: true,
            notify_failed: true,
            favorite_runtime_models: Vec::new(),
            runtime_selection_usage: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredAppSettings {
    schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme_selection: Option<ThemeSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reduce_motion: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keep_screen_on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    haptics: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_scale_percent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify_completed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify_needs_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify_failed: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    favorite_runtime_models: Vec<RuntimeModelFavorite>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    runtime_selection_usage: BTreeMap<String, u64>,
}

/// One paired runtime: the credential plus the mobile-owned metadata the
/// runtime list shows. Keeping the metadata beside the bundle means a rename
/// and the "last connected" history survive a re-pair and a restart.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredHostEntry {
    pub bundle: MobileCredentialBundle,
    /// A name the user typed on the phone. `None` means the label derived from
    /// the credential (desktop-advertised name, route host, or server id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_override: Option<String>,
    /// When this runtime was first paired. Zero marks a v1 entry migrated
    /// before the timestamp existed.
    #[serde(default)]
    pub added_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_at_ms: Option<i64>,
    /// What the peer said it was, as observed at pairing or on the last
    /// successful connect. Kept beside the credential rather than inside it:
    /// the credential schema is frozen, while a hosts file written before this
    /// key existed still loads and reads as [`RemoteServerKind::Unknown`].
    ///
    /// An unknown kind is not serialized, so a phone that never learned one
    /// writes exactly the v2 file the previous build wrote.
    #[serde(default, skip_serializing_if = "server_kind_is_unknown")]
    pub server_kind: RemoteServerKind,
}

fn server_kind_is_unknown(kind: &RemoteServerKind) -> bool {
    *kind == RemoteServerKind::Unknown
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredClientIdentity {
    schema_version: String,
    private_key: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredHosts {
    schema_version: String,
    hosts: Vec<StoredHostEntry>,
}

/// The pre-metadata file shape, read only to migrate it to [`StoredHosts`].
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredHostsV1 {
    schema_version: String,
    hosts: Vec<MobileCredentialBundle>,
}

/// Reads just the discriminator, so the payload can then be decoded with the
/// struct matching its schema version.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredHostsVersion {
    schema_version: String,
}

/// Per-host mobile presentation overrides. `None` means that the desktop
/// value should be used for that setting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MobileTimelineDisplaySettingsOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_agent_generation_status: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_display_mode: Option<AgentTimelineReasoningDisplayMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_expanded_by_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhanced_command_execution_display: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhanced_file_operation_display: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredTimelineDisplaySettingsOverrides {
    schema_version: String,
    overrides: BTreeMap<String, MobileTimelineDisplaySettingsOverride>,
}

/// The newest summary window of one runtime session.
///
/// Only summary payloads are held here: tool detail is the large part of a
/// timeline and is read back one item at a time, which is exactly what makes a
/// cache of whole conversations small enough to keep on a phone.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredSessionTimeline {
    /// The paired runtime that produced the window. Session ids are minted per
    /// runtime, so the pair is the key — never the session id alone.
    pub host_id: String,
    pub session_id: VibexSessionId,
    /// When this window was last written, in Unix milliseconds. Eviction is
    /// oldest-first, so this is the record's whole age signal.
    pub saved_at_ms: i64,
    /// Whether the runtime reported history below this window. Kept beside the
    /// items because it describes them, not the client that reads them back.
    pub has_older: bool,
    pub items: Vec<TimelineItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredSessionTimelines {
    schema_version: String,
    sessions: Vec<StoredSessionTimeline>,
}

#[derive(Clone, Debug)]
pub struct CredentialStorage {
    data_dir: PathBuf,
}

impl CredentialStorage {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    pub fn path(&self) -> PathBuf {
        self.data_dir.join(CREDENTIAL_FILE)
    }

    pub fn hosts_path(&self) -> PathBuf {
        self.data_dir.join(HOSTS_FILE)
    }

    pub fn timeline_display_settings_overrides_path(&self) -> PathBuf {
        self.data_dir.join(TIMELINE_DISPLAY_SETTINGS_FILE)
    }

    pub fn load(&self) -> BackendResult<Option<MobileCredentialBundle>> {
        let path = self.path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(storage_error("mobile_credentials_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_CREDENTIAL_BYTES {
            return self.reject_invalid(&path);
        }
        let bytes = fs::read(&path).map_err(|_| storage_error("mobile_credentials_read_failed"))?;
        let bundle: MobileCredentialBundle = match serde_json::from_slice(&bytes) {
            Ok(bundle) => bundle,
            Err(_) => return self.reject_invalid(&path),
        };
        if bundle.validate().is_err() {
            return self.reject_invalid(&path);
        }
        Ok(Some(bundle))
    }

    pub fn save(&self, bundle: &MobileCredentialBundle) -> BackendResult<()> {
        bundle.validate()?;
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
        let encoded = serde_json::to_vec(bundle)
            .map_err(|_| storage_error("mobile_credentials_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(storage_error("mobile_credentials_invalid"));
        }
        let path = self.path();
        let temporary = temporary_path(&path);
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(storage_error("mobile_credentials_write_failed")),
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let outcome = (|| {
            let mut file = options
                .open(&temporary)
                .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            file.write_all(&encoded)
                .and_then(|_| file.sync_all())
                .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            fs::rename(&temporary, &path)
                .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            }
            Ok(())
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome
    }

    pub fn load_hosts(&self) -> BackendResult<Vec<StoredHostEntry>> {
        let path = self.hosts_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(storage_error("mobile_hosts_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_HOST_BYTES {
            return self.reject_invalid_hosts(&path);
        }
        let bytes = fs::read(&path).map_err(|_| storage_error("mobile_hosts_read_failed"))?;
        let version: StoredHostsVersion = match serde_json::from_slice(&bytes) {
            Ok(version) => version,
            Err(_) => return self.reject_invalid_hosts(&path),
        };
        match version.schema_version.as_str() {
            HOSTS_SCHEMA_VERSION => {
                let stored: StoredHosts = match serde_json::from_slice(&bytes) {
                    Ok(stored) => stored,
                    Err(_) => return self.reject_invalid_hosts(&path),
                };
                if stored.hosts.len() > MAX_HOSTS {
                    return self.reject_invalid_hosts(&path);
                }
                for host in &stored.hosts {
                    if host.bundle.validate().is_err() {
                        return self.reject_invalid_hosts(&path);
                    }
                }
                Ok(stored.hosts)
            }
            HOSTS_SCHEMA_VERSION_V1 => {
                let stored: StoredHostsV1 = match serde_json::from_slice(&bytes) {
                    Ok(stored) => stored,
                    Err(_) => return self.reject_invalid_hosts(&path),
                };
                if stored.hosts.len() > MAX_HOSTS {
                    return self.reject_invalid_hosts(&path);
                }
                let mut migrated = Vec::with_capacity(stored.hosts.len());
                for bundle in stored.hosts {
                    if bundle.validate().is_err() {
                        return self.reject_invalid_hosts(&path);
                    }
                    migrated.push(StoredHostEntry {
                        bundle,
                        name_override: None,
                        added_at_ms: 0,
                        last_connected_at_ms: None,
                        server_kind: RemoteServerKind::Unknown,
                    });
                }
                // Best effort: the upgrade is a convenience, and a read-only or
                // full data dir must not cost the user the hosts just decoded.
                let _ = self.save_stored_hosts(&migrated);
                Ok(migrated)
            }
            _ => self.reject_invalid_hosts(&path),
        }
    }

    pub fn save_stored_hosts(&self, hosts: &[StoredHostEntry]) -> BackendResult<()> {
        if hosts.len() > MAX_HOSTS {
            return Err(storage_error("mobile_hosts_invalid"));
        }
        for host in hosts {
            host.bundle.validate()?;
        }
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_hosts_write_failed"))?;
        let stored = StoredHosts {
            schema_version: HOSTS_SCHEMA_VERSION.to_string(),
            hosts: hosts.to_vec(),
        };
        let encoded =
            serde_json::to_vec(&stored).map_err(|_| storage_error("mobile_hosts_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_HOST_BYTES {
            return Err(storage_error("mobile_hosts_invalid"));
        }
        write_atomic(&self.hosts_path(), &encoded, "mobile_hosts_write_failed")
    }

    pub fn load_timeline_display_settings_overrides(
        &self,
    ) -> BackendResult<BTreeMap<String, MobileTimelineDisplaySettingsOverride>> {
        let path = self.timeline_display_settings_overrides_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(_) => return Err(storage_error("mobile_timeline_settings_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_TIMELINE_DISPLAY_SETTINGS_BYTES {
            return self.reject_invalid_timeline_settings(&path);
        }
        let bytes =
            fs::read(&path).map_err(|_| storage_error("mobile_timeline_settings_read_failed"))?;
        let stored: StoredTimelineDisplaySettingsOverrides = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(_) => return self.reject_invalid_timeline_settings(&path),
        };
        if stored.schema_version != TIMELINE_DISPLAY_SETTINGS_SCHEMA_VERSION
            || stored.overrides.len() > MAX_TIMELINE_DISPLAY_SETTINGS_OVERRIDES
            || stored.overrides.keys().any(|host_id| {
                host_id.is_empty()
                    || host_id.len() > MAX_TIMELINE_DISPLAY_SETTINGS_HOST_ID_BYTES
                    || host_id.chars().any(char::is_control)
            })
        {
            return self.reject_invalid_timeline_settings(&path);
        }
        Ok(stored.overrides)
    }

    pub fn save_timeline_display_settings_overrides(
        &self,
        overrides: &BTreeMap<String, MobileTimelineDisplaySettingsOverride>,
    ) -> BackendResult<()> {
        if overrides.len() > MAX_TIMELINE_DISPLAY_SETTINGS_OVERRIDES
            || overrides.keys().any(|host_id| {
                host_id.is_empty()
                    || host_id.len() > MAX_TIMELINE_DISPLAY_SETTINGS_HOST_ID_BYTES
                    || host_id.chars().any(char::is_control)
            })
        {
            return Err(storage_error("mobile_timeline_settings_invalid"));
        }
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_timeline_settings_write_failed"))?;
        let stored = StoredTimelineDisplaySettingsOverrides {
            schema_version: TIMELINE_DISPLAY_SETTINGS_SCHEMA_VERSION.to_string(),
            overrides: overrides.clone(),
        };
        let encoded = serde_json::to_vec(&stored)
            .map_err(|_| storage_error("mobile_timeline_settings_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_TIMELINE_DISPLAY_SETTINGS_BYTES {
            return Err(storage_error("mobile_timeline_settings_invalid"));
        }
        let path = self.timeline_display_settings_overrides_path();
        let temporary = temporary_path(&path);
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(storage_error("mobile_timeline_settings_write_failed")),
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let outcome = (|| {
            let mut file = options
                .open(&temporary)
                .map_err(|_| storage_error("mobile_timeline_settings_write_failed"))?;
            file.write_all(&encoded)
                .and_then(|_| file.sync_all())
                .map_err(|_| storage_error("mobile_timeline_settings_write_failed"))?;
            fs::rename(&temporary, &path)
                .map_err(|_| storage_error("mobile_timeline_settings_write_failed"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .map_err(|_| storage_error("mobile_timeline_settings_write_failed"))?;
            }
            Ok(())
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome
    }

    pub fn clear_timeline_display_settings_overrides(&self) -> BackendResult<()> {
        match fs::remove_file(self.timeline_display_settings_overrides_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_timeline_settings_clear_failed")),
        }
    }

    pub fn session_timelines_path(&self) -> PathBuf {
        self.data_dir.join(SESSION_TIMELINES_FILE)
    }

    /// The cached window for one runtime session, if this phone stored one.
    ///
    /// Only a record whose runtime and session both match is returned, so a
    /// window written for another desktop can never be painted into a session
    /// that merely reused the id. A record that fails validation is treated as
    /// absent rather than partially applied.
    pub fn load_session_timeline(
        &self,
        host_id: &str,
        session_id: &VibexSessionId,
    ) -> BackendResult<Option<StoredSessionTimeline>> {
        Ok(self
            .load_stored_session_timelines()?
            .into_iter()
            .find(|record| {
                record.host_id == host_id
                    && record.session_id == *session_id
                    && session_timeline_record_is_valid(record)
            }))
    }

    /// Remembers one session's newest window, evicting the oldest records to
    /// stay within the record count and the file's byte cap.
    ///
    /// A window too large to fit on its own is rejected instead of trimming
    /// the payload: the caller would rather re-fetch than show a conversation
    /// with a hole in it.
    pub fn save_session_timeline(&self, record: &StoredSessionTimeline) -> BackendResult<()> {
        let mut record = record.clone();
        // Detail is what a reader opens an activity to fetch; storing it would
        // undo the point of the summary pages that filled this window.
        record.items = std::mem::take(&mut record.items)
            .into_iter()
            .map(TimelineItem::into_summary)
            .collect();
        if !session_timeline_record_is_valid(&record) {
            return Err(storage_error("mobile_session_timelines_invalid"));
        }
        // A corrupt file reads as no records at all: it is data this build
        // already refused to apply, and the write below replaces it wholesale.
        let mut sessions = self.load_stored_session_timelines().unwrap_or_default();
        sessions.retain(|existing| {
            existing.host_id != record.host_id || existing.session_id != record.session_id
        });
        sessions.push(record);
        sessions.sort_by_key(|record| std::cmp::Reverse(record.saved_at_ms));
        sessions.truncate(MAX_SESSION_TIMELINES);

        let mut stored = StoredSessionTimelines {
            schema_version: SESSION_TIMELINES_SCHEMA_VERSION.to_string(),
            sessions,
        };
        loop {
            let encoded = serde_json::to_vec(&stored)
                .map_err(|_| storage_error("mobile_session_timelines_encode_failed"))?;
            if encoded.is_empty() {
                return Err(storage_error("mobile_session_timelines_invalid"));
            }
            if encoded.len() as u64 <= MAX_SESSION_TIMELINES_BYTES {
                fs::create_dir_all(&self.data_dir)
                    .map_err(|_| storage_error("mobile_session_timelines_write_failed"))?;
                return write_atomic(
                    &self.session_timelines_path(),
                    &encoded,
                    "mobile_session_timelines_write_failed",
                );
            }
            // Over the cap: the oldest conversation is the one a cold start is
            // least likely to reopen, so it goes first.
            if stored.sessions.len() <= 1 {
                return Err(storage_error("mobile_session_timelines_invalid"));
            }
            stored.sessions.pop();
        }
    }

    pub fn clear_session_timelines(&self) -> BackendResult<()> {
        match fs::remove_file(self.session_timelines_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_session_timelines_clear_failed")),
        }
    }

    fn load_stored_session_timelines(&self) -> BackendResult<Vec<StoredSessionTimeline>> {
        let path = self.session_timelines_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(storage_error("mobile_session_timelines_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_SESSION_TIMELINES_BYTES {
            return self.reject_invalid_session_timelines(&path);
        }
        let bytes =
            fs::read(&path).map_err(|_| storage_error("mobile_session_timelines_read_failed"))?;
        let stored: StoredSessionTimelines = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(_) => return self.reject_invalid_session_timelines(&path),
        };
        if stored.schema_version != SESSION_TIMELINES_SCHEMA_VERSION
            || stored.sessions.len() > MAX_SESSION_TIMELINES
        {
            return self.reject_invalid_session_timelines(&path);
        }
        Ok(stored.sessions)
    }

    pub fn app_settings_path(&self) -> PathBuf {
        self.data_dir.join(APP_SETTINGS_FILE)
    }

    /// Reads the persisted appearance and language preferences. A missing or
    /// corrupt file falls back to the platform defaults, matching the other
    /// stored payloads' reject-invalid contract.
    ///
    /// A v1 file is still read: its appearance, language, text size and motion
    /// preferences are kept, and only the all-off notification shape the
    /// first-launch bug wrote is repaired. See the note at the upgrade below.
    pub fn load_app_settings(&self) -> BackendResult<AppSettings> {
        let path = self.app_settings_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AppSettings::default());
            }
            Err(_) => return Err(storage_error("mobile_app_settings_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_APP_SETTINGS_BYTES {
            return self.reject_invalid_app_settings(&path);
        }
        let bytes =
            fs::read(&path).map_err(|_| storage_error("mobile_app_settings_read_failed"))?;
        let stored: StoredAppSettings = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(_) => return self.reject_invalid_app_settings(&path),
        };
        if stored.schema_version != APP_SETTINGS_SCHEMA_VERSION
            && stored.schema_version != APP_SETTINGS_SCHEMA_VERSION_V2
            && stored.schema_version != APP_SETTINGS_SCHEMA_VERSION_V1
        {
            return self.reject_invalid_app_settings(&path);
        }
        // A v1 file can carry the derived `Default` that the first-launch bug
        // wrote: all four switches `false`, saved along with the first theme or
        // language change the reader made. That record is indistinguishable
        // from a reader who turned all four off by hand, so the migration keeps
        // every switch that reads as a choice — a file with the master on, or
        // with only some categories off, is left exactly as stored — and
        // repairs only the all-off shape the bug produced. Once anything is
        // saved the file is v3 and this never runs again.
        let carries_derived_notification_defaults = stored.schema_version
            == APP_SETTINGS_SCHEMA_VERSION_V1
            && stored.notifications == Some(false)
            && stored.notify_completed == Some(false)
            && stored.notify_needs_input == Some(false)
            && stored.notify_failed == Some(false);
        let notification_switch = |stored: Option<bool>| {
            if carries_derived_notification_defaults {
                true
            } else {
                stored.unwrap_or(true)
            }
        };
        let theme = match stored.theme.as_deref() {
            None => None,
            Some(value) => match crate::theme::AppearanceMode::from_storage(value) {
                Some(mode) if value.len() <= MAX_APP_SETTINGS_VALUE_BYTES => Some(mode),
                _ => return self.reject_invalid_app_settings(&path),
            },
        };
        let language = match stored.language.as_deref() {
            None => None,
            Some(value) => match crate::locale::LanguagePreference::from_storage(value) {
                Some(language) if value.len() <= MAX_APP_SETTINGS_VALUE_BYTES => Some(language),
                _ => return self.reject_invalid_app_settings(&path),
            },
        };
        let mut seen_favorites = BTreeSet::new();
        let favorite_runtime_models = stored
            .favorite_runtime_models
            .into_iter()
            .filter(|favorite| {
                favorite.auth_source.is_some()
                    && !favorite.model_key.is_empty()
                    && favorite.model_key.len() <= 256
            })
            .filter(|favorite| {
                seen_favorites.insert((
                    favorite.agent_id.clone(),
                    favorite.auth_source.clone(),
                    favorite.model_key.clone(),
                ))
            })
            .take(RUNTIME_MODEL_FAVORITE_LIMIT)
            .collect::<Vec<_>>();
        let runtime_selection_usage = stored
            .runtime_selection_usage
            .into_iter()
            .filter(|(key, count)| !key.is_empty() && key.len() <= 512 && *count > 0)
            .take(RUNTIME_SELECTION_USAGE_LIMIT)
            .collect::<BTreeMap<_, _>>();
        Ok(AppSettings {
            theme,
            language,
            theme_selection: stored.theme_selection.unwrap_or_default(),
            reduce_motion: stored.reduce_motion.unwrap_or(false),
            keep_screen_on: stored.keep_screen_on.unwrap_or(false),
            // A file written before the switch existed has no opinion, and the
            // default is on, so an upgrade never silences a phone that was
            // never asked.
            haptics: stored.haptics.unwrap_or(true),
            font_scale_percent: stored
                .font_scale_percent
                .filter(|percent| crate::theme::FONT_SCALE_STEPS.contains(percent))
                .unwrap_or(crate::theme::FONT_SCALE_INITIAL),
            notifications: notification_switch(stored.notifications),
            notify_completed: notification_switch(stored.notify_completed),
            notify_needs_input: notification_switch(stored.notify_needs_input),
            notify_failed: notification_switch(stored.notify_failed),
            favorite_runtime_models,
            runtime_selection_usage,
        })
    }

    pub fn save_app_settings(&self, settings: &AppSettings) -> BackendResult<()> {
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_app_settings_write_failed"))?;
        let stored = StoredAppSettings {
            schema_version: APP_SETTINGS_SCHEMA_VERSION.to_string(),
            theme: settings.theme.map(|mode| mode.to_storage().to_string()),
            language: settings
                .language
                .map(|language| language.to_storage().to_string()),
            theme_selection: Some(settings.theme_selection.clone())
                .filter(|selection| selection.light.is_some() || selection.dark.is_some()),
            reduce_motion: Some(settings.reduce_motion),
            keep_screen_on: Some(settings.keep_screen_on),
            haptics: Some(settings.haptics),
            font_scale_percent: Some(settings.font_scale_percent),
            notifications: Some(settings.notifications),
            notify_completed: Some(settings.notify_completed),
            notify_needs_input: Some(settings.notify_needs_input),
            notify_failed: Some(settings.notify_failed),
            favorite_runtime_models: settings.favorite_runtime_models.clone(),
            runtime_selection_usage: settings.runtime_selection_usage.clone(),
        };
        let encoded = serde_json::to_vec(&stored)
            .map_err(|_| storage_error("mobile_app_settings_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_APP_SETTINGS_BYTES {
            return Err(storage_error("mobile_app_settings_invalid"));
        }
        write_atomic(
            &self.app_settings_path(),
            &encoded,
            "mobile_app_settings_write_failed",
        )
    }

    pub fn clear_app_settings(&self) -> BackendResult<()> {
        match fs::remove_file(self.app_settings_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_app_settings_clear_failed")),
        }
    }

    pub fn client_identity_path(&self) -> PathBuf {
        self.data_dir.join(CLIENT_IDENTITY_FILE)
    }

    /// The device identity this install presents when it pairs.
    ///
    /// One identity belongs to the phone, not to a single pairing: the desktop
    /// keeps one trust-store entry per identity, so re-pairing with the stored
    /// key updates that entry instead of adding another. The returned identity
    /// carries a placeholder device id because the request only needs the
    /// public key; the server-issued id is bound after the claim. A missing or
    /// unreadable file reads as "no identity yet" so a corrupt payload cannot
    /// block pairing.
    pub fn load_client_identity(&self) -> Option<ClientDeviceIdentity> {
        let path = self.client_identity_path();
        match fs::read(&path) {
            Ok(bytes) => {
                let identity = decode_client_identity(&bytes);
                if identity.is_none() {
                    let _ = fs::remove_file(&path);
                }
                identity
            }
            Err(_) => self.identity_from_stored_hosts(),
        }
    }

    /// A phone that paired before the identity file existed still owns one: the
    /// key inside its most recently used credential. Adopting it keeps the next
    /// pairing on the entry the runtime already holds for this phone instead of
    /// minting a new device.
    fn identity_from_stored_hosts(&self) -> Option<ClientDeviceIdentity> {
        let hosts = self.load_hosts().ok()?;
        let newest = hosts
            .iter()
            .max_by_key(|entry| entry.last_connected_at_ms.unwrap_or(entry.added_at_ms))?;
        ClientDeviceIdentity::from_private_key_base64(
            newest.bundle.record.auth.device_id.clone(),
            &newest.bundle.identity_private_key,
        )
        .ok()
    }

    /// Remembers the identity a completed pairing used, so the next pairing is
    /// recognised as the same client.
    pub fn save_client_identity(&self, identity: &ClientDeviceIdentity) -> BackendResult<()> {
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_client_identity_write_failed"))?;
        let stored = StoredClientIdentity {
            schema_version: CLIENT_IDENTITY_SCHEMA_VERSION.to_string(),
            private_key: identity.private_key_base64(),
        };
        let encoded = serde_json::to_vec(&stored)
            .map_err(|_| storage_error("mobile_client_identity_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_CLIENT_IDENTITY_BYTES {
            return Err(storage_error("mobile_client_identity_invalid"));
        }
        write_atomic(
            &self.client_identity_path(),
            &encoded,
            "mobile_client_identity_write_failed",
        )
    }

    fn reject_invalid_app_settings(&self, path: &Path) -> BackendResult<AppSettings> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_app_settings_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_app_settings_invalid"))
            }
            Err(_) => Err(storage_error("mobile_app_settings_clear_failed")),
        }
    }

    pub fn composer_state_path(&self) -> PathBuf {
        self.data_dir.join(COMPOSER_STATE_FILE)
    }

    /// Reads the composer's un-sent work. A missing, empty, or corrupt file
    /// reads as "nothing was half-written", which is the safe answer: the
    /// drafts are a courtesy, not the session itself.
    pub fn load_composer_state(&self) -> BackendResult<Vec<StoredComposerSession>> {
        let path = self.composer_state_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(storage_error("mobile_composer_state_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_COMPOSER_STATE_BYTES {
            return self.reject_invalid_composer_state(&path);
        }
        let bytes =
            fs::read(&path).map_err(|_| storage_error("mobile_composer_state_read_failed"))?;
        let stored: StoredComposerStateFile = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(_) => return self.reject_invalid_composer_state(&path),
        };
        if stored.schema_version != COMPOSER_STATE_SCHEMA_VERSION {
            return self.reject_invalid_composer_state(&path);
        }
        let mut sessions = stored
            .sessions
            .into_iter()
            .filter(|session| {
                // The key has to name a session: a record this client could
                // never open a conversation for is not worth carrying, and
                // dropping it here keeps a single bad entry from costing every
                // other session its draft.
                session.session_id.len() <= MAX_COMPOSER_SESSION_ID_BYTES
                    && VibexSessionId::parse(session.session_id.clone()).is_ok()
            })
            .collect::<Vec<_>>();
        sessions.truncate(MAX_COMPOSER_SESSIONS);
        Ok(sessions)
    }

    /// Writes the composer's un-sent work, newest sessions first and within the
    /// attachment budget. A record that does not fit keeps its text and loses
    /// the files hanging on it: the reader's words are the part they cannot
    /// retype, and an attachment is one tap away from being picked again.
    pub fn save_composer_state(&self, sessions: &[StoredComposerSession]) -> BackendResult<()> {
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_composer_state_write_failed"))?;
        let mut ordered = sessions.to_vec();
        ordered.sort_by_key(|session| std::cmp::Reverse(session.saved_at_ms));
        ordered.truncate(MAX_COMPOSER_SESSIONS);
        let mut spent = 0usize;
        let stored = ordered
            .into_iter()
            .map(|mut session| {
                session.draft = clamp_composer_text(session.draft);
                session.attachments.retain(|attachment| {
                    let cost = attachment.data_url.len();
                    if spent.saturating_add(cost) > MAX_COMPOSER_ATTACHMENT_BYTES {
                        return false;
                    }
                    spent += cost;
                    true
                });
                session.queued = session
                    .queued
                    .into_iter()
                    .filter_map(|mut message| {
                        message.text = clamp_composer_text(message.text);
                        if message.text.is_empty() && message.attachments.is_empty() {
                            return None;
                        }
                        message.attachments.retain(|attachment| {
                            let cost = attachment.data_url.len();
                            if spent.saturating_add(cost) > MAX_COMPOSER_ATTACHMENT_BYTES {
                                return false;
                            }
                            spent += cost;
                            true
                        });
                        Some(message)
                    })
                    .collect();
                session
            })
            .filter(|session| {
                !session.draft.is_empty()
                    || !session.attachments.is_empty()
                    || !session.queued.is_empty()
            })
            .collect::<Vec<_>>();
        let encoded = serde_json::to_vec(&StoredComposerStateFile {
            schema_version: COMPOSER_STATE_SCHEMA_VERSION.to_string(),
            sessions: stored,
        })
        .map_err(|_| storage_error("mobile_composer_state_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_COMPOSER_STATE_BYTES {
            return Err(storage_error("mobile_composer_state_invalid"));
        }
        write_atomic(
            &self.composer_state_path(),
            &encoded,
            "mobile_composer_state_write_failed",
        )
    }

    pub fn clear_composer_state(&self) -> BackendResult<()> {
        match fs::remove_file(self.composer_state_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_composer_state_clear_failed")),
        }
    }

    fn reject_invalid_composer_state(
        &self,
        path: &Path,
    ) -> BackendResult<Vec<StoredComposerSession>> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_composer_state_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_composer_state_invalid"))
            }
            Err(_) => Err(storage_error("mobile_composer_state_clear_failed")),
        }
    }

    pub fn clear(&self) -> BackendResult<()> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_credentials_clear_failed")),
        }
    }

    pub fn clear_hosts(&self) -> BackendResult<()> {
        match fs::remove_file(self.hosts_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_hosts_clear_failed")),
        }
    }

    fn reject_invalid(&self, path: &Path) -> BackendResult<Option<MobileCredentialBundle>> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_credentials_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_credentials_invalid"))
            }
            Err(_) => Err(storage_error("mobile_credentials_clear_failed")),
        }
    }

    fn reject_invalid_hosts(&self, path: &Path) -> BackendResult<Vec<StoredHostEntry>> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_hosts_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_hosts_invalid"))
            }
            Err(_) => Err(storage_error("mobile_hosts_clear_failed")),
        }
    }

    fn reject_invalid_timeline_settings(
        &self,
        path: &Path,
    ) -> BackendResult<BTreeMap<String, MobileTimelineDisplaySettingsOverride>> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_timeline_settings_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_timeline_settings_invalid"))
            }
            Err(_) => Err(storage_error("mobile_timeline_settings_clear_failed")),
        }
    }

    fn reject_invalid_session_timelines(
        &self,
        path: &Path,
    ) -> BackendResult<Vec<StoredSessionTimeline>> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_session_timelines_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_session_timelines_invalid"))
            }
            Err(_) => Err(storage_error("mobile_session_timelines_clear_failed")),
        }
    }
}

/// Whether one cached window may be painted into the session it names.
///
/// The whole record is judged at once: a window is either the conversation it
/// claims to be or it is dropped, never kept up to the point something looked
/// wrong.
fn session_timeline_record_is_valid(record: &StoredSessionTimeline) -> bool {
    let named = |id: &str| {
        !id.is_empty()
            && id.len() <= MAX_SESSION_TIMELINE_ID_BYTES
            && !id.chars().any(char::is_control)
    };
    named(&record.host_id)
        && record.saved_at_ms >= 0
        && record.items.len() <= MAX_SESSION_TIMELINE_ITEMS
        && record
            .items
            .iter()
            .all(|item| item.session_id == record.session_id)
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

/// Creates the data dir, writes `encoded` through a temporary file, fsyncs,
/// and renames it into place with owner-only permissions. The shared body of
/// every stored payload's save path.
fn write_atomic(path: &Path, encoded: &[u8], write_failed_code: &'static str) -> BackendResult<()> {
    let temporary = temporary_path(path);
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(storage_error(write_failed_code)),
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let outcome = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|_| storage_error(write_failed_code))?;
        file.write_all(encoded)
            .and_then(|_| file.sync_all())
            .map_err(|_| storage_error(write_failed_code))?;
        fs::rename(&temporary, path).map_err(|_| storage_error(write_failed_code))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|_| storage_error(write_failed_code))?;
        }
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

fn storage_error(code: &'static str) -> BackendError {
    BackendError::failed(code, "native mobile credential storage is unavailable")
}

/// Decodes the stored install identity, rejecting anything this build cannot
/// trust as an X25519 key.
fn decode_client_identity(bytes: &[u8]) -> Option<ClientDeviceIdentity> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_CLIENT_IDENTITY_BYTES {
        return None;
    }
    let stored: StoredClientIdentity = serde_json::from_slice(bytes).ok()?;
    if stored.schema_version != CLIENT_IDENTITY_SCHEMA_VERSION {
        return None;
    }
    ClientDeviceIdentity::from_private_key_base64(
        vibex_core::DeviceId::new(),
        stored.private_key.trim(),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentTimelineReasoningDisplayMode, RemoteAuthProof, RemoteClientType};
    use vibex_remote_client::{ClientDeviceIdentity, RemoteCredentialRecord};

    use crate::pairing::{MobileLocalNetworkCandidate, MobileRemoteRouteBundle};

    fn fixture() -> MobileCredentialBundle {
        let identity = ClientDeviceIdentity::generate(vibex_core::DeviceId::new()).unwrap();
        MobileCredentialBundle {
            schema_version: crate::pairing::MOBILE_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: RemoteCredentialRecord {
                server_url: "https://desktop.example".to_string(),
                auth: RemoteAuthProof {
                    device_id: identity.device_id().clone(),
                    auth_token: "grant".to_string(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some("server-public".to_string()),
            },
            identity_private_key: identity.private_key_base64(),
            expected_server_id: "desktop".to_string(),
            client_type: RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            display_name: None,
            route: Some(MobileRemoteRouteBundle {
                local_network: None,
                direct_candidates: vec!["https://desktop.example".to_string()],
                relay: None,
            }),
        }
    }

    #[test]
    fn credentials_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let fixture = fixture();
        assert!(storage.load().unwrap().is_none());
        storage.save(&fixture).unwrap();
        assert_eq!(storage.load().unwrap().unwrap(), fixture);
        storage.clear().unwrap();
        assert!(storage.load().unwrap().is_none());
    }

    #[test]
    fn client_identity_round_trips_and_survives_a_corrupt_file() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        assert!(storage.load_client_identity().is_none());

        let identity = ClientDeviceIdentity::generate(vibex_core::DeviceId::new()).unwrap();
        storage.save_client_identity(&identity).unwrap();
        let stored = storage
            .load_client_identity()
            .expect("a saved identity should load again");
        // Only the key is stored: the device id is issued by each runtime.
        assert_eq!(stored.public_key_base64(), identity.public_key_base64());
        assert_ne!(stored.device_id(), identity.device_id());

        // A corrupt payload reads as "no identity yet" instead of blocking the
        // next pairing, and the unreadable file is dropped.
        fs::write(storage.client_identity_path(), b"{").unwrap();
        assert!(storage.load_client_identity().is_none());
        assert!(!storage.client_identity_path().exists());
    }

    /// A phone that paired before the identity file existed adopts the key from
    /// its most recently used credential, so its next pairing is recognised as
    /// the client the runtime already knows.
    #[test]
    fn client_identity_falls_back_to_the_newest_stored_credential() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let connected = fixture();
        let mut added_only = fixture();
        added_only.expected_server_id = "added-only-desktop".to_string();
        storage
            .save_stored_hosts(&[
                StoredHostEntry {
                    bundle: connected.clone(),
                    name_override: None,
                    added_at_ms: 1_700_000_000_000,
                    last_connected_at_ms: Some(1_700_000_500_000),
                    server_kind: RemoteServerKind::Desktop,
                },
                StoredHostEntry {
                    bundle: added_only.clone(),
                    name_override: None,
                    added_at_ms: 1_700_000_100_000,
                    last_connected_at_ms: None,
                    server_kind: RemoteServerKind::Headless,
                },
            ])
            .unwrap();

        // A connection is the strongest signal; `added_at_ms` only breaks ties
        // between credentials that were never used.
        let adopted = storage
            .load_client_identity()
            .expect("a stored credential carries an identity");
        assert_eq!(adopted.private_key_base64(), connected.identity_private_key);
        // Reading it must not invent a file: the adoption is a fallback until
        // the next pairing stores the identity explicitly.
        assert!(!storage.client_identity_path().exists());

        storage
            .save_stored_hosts(&[StoredHostEntry {
                bundle: added_only.clone(),
                name_override: None,
                added_at_ms: 1_700_000_100_000,
                last_connected_at_ms: None,
                server_kind: RemoteServerKind::Headless,
            }])
            .unwrap();
        let adopted = storage
            .load_client_identity()
            .expect("a stored credential carries an identity");
        assert_eq!(
            adopted.private_key_base64(),
            added_only.identity_private_key
        );
    }

    #[test]
    fn hosts_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let mut second = fixture();
        second.expected_server_id = "second-desktop".to_string();
        let entries = vec![
            StoredHostEntry {
                bundle: fixture(),
                name_override: Some("Studio desktop".to_string()),
                added_at_ms: 1_700_000_000_000,
                last_connected_at_ms: Some(1_700_000_100_000),
                server_kind: RemoteServerKind::Desktop,
            },
            StoredHostEntry {
                bundle: second,
                name_override: None,
                added_at_ms: 1_700_000_200_000,
                last_connected_at_ms: None,
                server_kind: RemoteServerKind::Headless,
            },
        ];
        storage.save_stored_hosts(&entries).unwrap();
        assert_eq!(storage.load_hosts().unwrap(), entries);
        storage.clear_hosts().unwrap();
        assert!(storage.load_hosts().unwrap().is_empty());
    }

    /// A hosts file written by the build before the runtime kind existed has no
    /// `serverKind` key. It must load, and the kind must read as unknown rather
    /// than as a runtime this phone claims to have identified.
    #[test]
    fn v2_hosts_without_a_server_kind_load_as_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        let mut host = serde_json::to_value(StoredHostEntry {
            bundle: fixture(),
            name_override: None,
            added_at_ms: 1_700_000_000_000,
            last_connected_at_ms: None,
            server_kind: RemoteServerKind::Unknown,
        })
        .unwrap();
        // Exactly the pre-field shape: the key is absent, not null.
        host.as_object_mut().unwrap().remove("serverKind");
        assert!(host.get("serverKind").is_none());
        fs::write(
            storage.hosts_path(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": HOSTS_SCHEMA_VERSION,
                "hosts": [host],
            }))
            .unwrap(),
        )
        .unwrap();

        let loaded = storage.load_hosts().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].server_kind, RemoteServerKind::Unknown);
        assert_eq!(loaded[0].bundle.expected_server_id, "desktop");
    }

    /// The kind is mobile-side metadata: the credential schema stays frozen, so
    /// a known kind must never appear inside the stored bundle.
    #[test]
    fn a_known_server_kind_round_trips_outside_the_credential() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        storage
            .save_stored_hosts(&[StoredHostEntry {
                bundle: fixture(),
                name_override: None,
                added_at_ms: 1,
                last_connected_at_ms: None,
                server_kind: RemoteServerKind::Headless,
            }])
            .unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_slice(&fs::read(storage.hosts_path()).unwrap()).unwrap();
        assert_eq!(rewritten["hosts"][0]["serverKind"], "headless");
        assert!(
            rewritten["hosts"][0]["bundle"].get("serverKind").is_none(),
            "the credential schema must not carry the runtime kind"
        );
        assert_eq!(
            storage.load_hosts().unwrap()[0].server_kind,
            RemoteServerKind::Headless
        );
    }

    /// An unknown kind writes no key at all, so the common case stays readable
    /// by the build that wrote the previous v2 files.
    #[test]
    fn an_unknown_server_kind_is_not_written() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        storage
            .save_stored_hosts(&[StoredHostEntry {
                bundle: fixture(),
                name_override: None,
                added_at_ms: 1,
                last_connected_at_ms: None,
                server_kind: RemoteServerKind::Unknown,
            }])
            .unwrap();

        let rewritten: serde_json::Value =
            serde_json::from_slice(&fs::read(storage.hosts_path()).unwrap()).unwrap();
        assert!(rewritten["hosts"][0].get("serverKind").is_none());
        assert_eq!(
            storage.load_hosts().unwrap()[0].server_kind,
            RemoteServerKind::Unknown
        );
    }

    /// A phone that paired before the runtime list gained metadata must keep
    /// every runtime, upgraded to the v2 shape on disk.
    #[test]
    fn v1_hosts_are_migrated_to_v2() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let first = fixture();
        let mut second = fixture();
        second.expected_server_id = "second-desktop".to_string();
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.hosts_path(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": HOSTS_SCHEMA_VERSION_V1,
                "hosts": [first, second],
            }))
            .unwrap(),
        )
        .unwrap();

        let loaded = storage.load_hosts().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].bundle.expected_server_id, "desktop");
        assert_eq!(loaded[1].bundle.expected_server_id, "second-desktop");
        assert_eq!(loaded[0].name_override, None);
        assert_eq!(loaded[0].added_at_ms, 0);
        assert_eq!(loaded[0].last_connected_at_ms, None);
        assert_eq!(loaded[0].server_kind, RemoteServerKind::Unknown);

        // The upgrade is persisted, so the next launch reads v2 directly.
        let rewritten: serde_json::Value =
            serde_json::from_slice(&fs::read(storage.hosts_path()).unwrap()).unwrap();
        assert_eq!(rewritten["schemaVersion"], HOSTS_SCHEMA_VERSION);
        assert_eq!(rewritten["hosts"].as_array().unwrap().len(), 2);
        assert_eq!(storage.load_hosts().unwrap().len(), 2);
    }

    /// The migration is a convenience: hosts that were already decoded must
    /// survive a data dir that cannot be written.
    #[test]
    fn a_failed_v1_migration_still_returns_the_hosts() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.hosts_path(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": HOSTS_SCHEMA_VERSION_V1,
                "hosts": [fixture()],
            }))
            .unwrap(),
        )
        .unwrap();
        // A directory where the atomic write wants its temporary file.
        fs::create_dir(temporary_path(&storage.hosts_path())).unwrap();

        let loaded = storage.load_hosts().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].bundle.expected_server_id, "desktop");
    }

    #[test]
    fn unknown_hosts_schema_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.hosts_path(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": "vibex-native-mobile-hosts.v3",
                "hosts": [],
            }))
            .unwrap(),
        )
        .unwrap();

        let error = storage.load_hosts().unwrap_err();
        assert_eq!(error.code, "mobile_hosts_invalid");
        assert!(!storage.hosts_path().exists());
    }

    #[test]
    fn malformed_hosts_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(storage.hosts_path(), b"not-json").unwrap();

        let error = storage.load_hosts().unwrap_err();
        assert_eq!(error.code, "mobile_hosts_invalid");
        assert!(!storage.hosts_path().exists());
    }

    #[test]
    fn timeline_display_settings_overrides_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "desktop-primary".to_string(),
            MobileTimelineDisplaySettingsOverride {
                show_agent_generation_status: Some(false),
                reasoning_display_mode: Some(AgentTimelineReasoningDisplayMode::Timeline),
                ..Default::default()
            },
        );

        storage
            .save_timeline_display_settings_overrides(&overrides)
            .unwrap();
        assert_eq!(
            storage.load_timeline_display_settings_overrides().unwrap(),
            overrides
        );

        storage.clear_timeline_display_settings_overrides().unwrap();
        assert!(
            storage
                .load_timeline_display_settings_overrides()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_timeline_display_settings_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.timeline_display_settings_overrides_path(),
            b"not-json",
        )
        .unwrap();

        let error = storage
            .load_timeline_display_settings_overrides()
            .unwrap_err();
        assert_eq!(error.code, "mobile_timeline_settings_invalid");
        assert!(!storage.timeline_display_settings_overrides_path().exists());
    }

    #[test]
    fn app_settings_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        assert_eq!(storage.load_app_settings().unwrap(), AppSettings::default());

        let selection = vibex_core::SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse("codex").unwrap(),
            vibex_core::ProviderProfileId::new(),
            "gpt-5",
        );
        let settings = AppSettings {
            theme: Some(crate::theme::AppearanceMode::Light),
            language: Some(crate::locale::LanguagePreference::ZhCn),
            favorite_runtime_models: vec![RuntimeModelFavorite::new(
                selection.agent_id.clone(),
                selection.auth_source.clone(),
                "model:gpt-5",
            )],
            runtime_selection_usage: BTreeMap::from([(
                vibex_desktop_model::runtime_selection_usage_key(&selection),
                3,
            )]),
            ..AppSettings::default()
        };
        storage.save_app_settings(&settings).unwrap();
        assert_eq!(storage.load_app_settings().unwrap(), settings);

        storage.clear_app_settings().unwrap();
        assert_eq!(storage.load_app_settings().unwrap(), AppSettings::default());
    }

    #[test]
    fn absent_app_settings_fields_follow_the_platform() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        storage
            .save_app_settings(&AppSettings {
                theme: Some(crate::theme::AppearanceMode::Dark),
                language: None,
                ..AppSettings::default()
            })
            .unwrap();
        let loaded = storage.load_app_settings().unwrap();
        assert_eq!(loaded.theme, Some(crate::theme::AppearanceMode::Dark));
        assert_eq!(loaded.language, None);
    }

    /// A phone that has never written a settings file must start on the
    /// smallest text-size step, not on 0%.
    ///
    /// The step is a percentage the rem base clamps, so a derived default of
    /// zero did not read as "unset": it read as the smallest size the clamp
    /// allows, which is a step the setting does not even offer. The same
    /// record also left the notification switches off while pairing requested
    /// authorization, so the master switch contradicted the permission the app
    /// had just asked for.
    #[test]
    fn a_fresh_install_starts_on_the_smallest_text_step_and_notifications_on() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        // No file at all: the loader's own fallback, which is what a first
        // launch reads.
        let fresh = storage.load_app_settings().unwrap();
        assert_eq!(fresh.font_scale_percent, crate::theme::FONT_SCALE_INITIAL);
        // A first launch sits on the smallest rung the row offers, so the only
        // direction an untouched phone can step is up.
        assert_eq!(fresh.font_scale_percent, crate::theme::FONT_SCALE_TINY);
        assert!(crate::theme::FONT_SCALE_STEPS.contains(&fresh.font_scale_percent));
        assert!(fresh.notifications);
        assert!(fresh.notify_completed);
        assert!(fresh.notify_needs_input);
        assert!(fresh.notify_failed);
        assert!(
            fresh.haptics,
            "feedback is on for a phone that was never asked"
        );
        // `AppSettings::default()` and the loader have to agree, or a corrupt
        // file and a missing one would launch on two different sizes.
        assert_eq!(fresh, AppSettings::default());
    }

    /// A stored step the ladder no longer offers is read as the first-launch
    /// step, and a corrupt step cannot smuggle a clamped value into the first
    /// frame.
    #[test]
    fn an_unknown_stored_text_step_falls_back_to_the_initial_step() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        for stored in ["0", "101", "50", "1000"] {
            fs::write(
                storage.app_settings_path(),
                format!(
                    r#"{{"schemaVersion":"{APP_SETTINGS_SCHEMA_VERSION}","fontScalePercent":{stored}}}"#
                ),
            )
            .unwrap();
            let loaded = storage.load_app_settings().unwrap();
            assert_eq!(
                loaded.font_scale_percent,
                crate::theme::FONT_SCALE_INITIAL,
                "stored step {stored} is not on the ladder"
            );
            assert!(
                crate::theme::FONT_SCALE_STEPS.contains(&loaded.font_scale_percent),
                "a loaded step must always be one the row can name"
            );
        }
    }

    /// A phone that ran the build with the derived `Default` already has a v1
    /// file whose four switches are `false`, because the first unrelated
    /// settings change (the theme, the language, the text size) saved the
    /// derived record. Upgrading must not leave those phones silently muted:
    /// the switches are read as on, and the rest of the file still applies.
    #[test]
    fn a_v1_settings_file_upgrades_with_notifications_on() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        // The shape the old build actually wrote: every field present, the
        // switches at the derived `false`.
        fs::write(
            storage.app_settings_path(),
            br#"{
                "schemaVersion":"vibex-native-mobile-app-settings.v1",
                "theme":"dark",
                "language":"zh-cn",
                "reduceMotion":true,
                "keepScreenOn":false,
                "fontScalePercent":90,
                "notifications":false,
                "notifyCompleted":false,
                "notifyNeedsInput":false,
                "notifyFailed":false
            }"#,
        )
        .unwrap();

        let loaded = storage.load_app_settings().unwrap();
        assert!(loaded.notifications, "the master switch upgrades on");
        assert!(loaded.notify_completed);
        assert!(loaded.notify_needs_input);
        assert!(loaded.notify_failed);
        // Everything the reader could actually choose is still theirs.
        assert_eq!(loaded.theme, Some(crate::theme::AppearanceMode::Dark));
        assert_eq!(
            loaded.language,
            Some(crate::locale::LanguagePreference::ZhCn)
        );
        assert!(loaded.reduce_motion);
        assert_eq!(loaded.font_scale_percent, crate::theme::FONT_SCALE_COMPACT);
        assert!(
            loaded.haptics,
            "a file from before the switch existed keeps the default on"
        );
    }

    /// A v1 file whose switches are not the all-off shape was written by a
    /// reader, not by the bug: one category off, or the master on with a
    /// category off, has to survive the upgrade untouched.
    #[test]
    fn a_v1_settings_file_keeps_a_notification_choice_it_can_trust() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.app_settings_path(),
            br#"{
                "schemaVersion":"vibex-native-mobile-app-settings.v1",
                "notifications":true,
                "notifyCompleted":false,
                "notifyNeedsInput":true,
                "notifyFailed":true
            }"#,
        )
        .unwrap();

        let loaded = storage.load_app_settings().unwrap();
        assert!(loaded.notifications);
        assert!(
            !loaded.notify_completed,
            "a category the reader turned off stays off"
        );
        assert!(loaded.notify_needs_input);
        assert!(loaded.notify_failed);
    }

    /// Once the file is written again it is v2, so a switch the reader turns
    /// off from then on is theirs and survives the next load.
    #[test]
    fn a_v2_settings_file_keeps_the_switches_the_reader_chose() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let settings = AppSettings {
            notifications: true,
            notify_completed: false,
            notify_needs_input: true,
            notify_failed: false,
            ..AppSettings::default()
        };
        storage.save_app_settings(&settings).unwrap();

        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(storage.app_settings_path()).unwrap()).unwrap();
        assert_eq!(stored["schemaVersion"], APP_SETTINGS_SCHEMA_VERSION);
        assert_eq!(storage.load_app_settings().unwrap(), settings);
    }

    #[test]
    fn composer_state_round_trips_and_clears() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        assert!(storage.load_composer_state().unwrap().is_empty());

        let sessions = vec![StoredComposerSession {
            session_id: "session_test".to_string(),
            saved_at_ms: 10,
            draft: "half a thought".to_string(),
            attachments: vec![StoredComposerAttachment {
                label: "shot.png".to_string(),
                mime_type: Some("image/png".to_string()),
                data_url: "data:image/png;base64,aGVsbG8=".to_string(),
                inline_text_offset: Some(3),
            }],
            queued: vec![StoredComposerQueuedMessage {
                text: "and another".to_string(),
                attachments: Vec::new(),
                mentions: Vec::new(),
            }],
        }];
        storage.save_composer_state(&sessions).unwrap();
        assert_eq!(storage.load_composer_state().unwrap(), sessions);

        storage.clear_composer_state().unwrap();
        assert!(storage.load_composer_state().unwrap().is_empty());
    }

    /// One record naming something that is not a session costs only itself:
    /// the sessions either side of it keep their drafts.
    #[test]
    fn a_composer_record_naming_no_session_is_dropped_on_its_own() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.composer_state_path(),
            br#"{
                "schemaVersion": "vibex-native-mobile-composer.v1",
                "sessions": [
                    {"sessionId": "session_kept", "savedAtMs": 2, "draft": "kept"},
                    {"sessionId": "not-a-session", "savedAtMs": 3, "draft": "dropped"},
                    {"sessionId": "", "savedAtMs": 4, "draft": "dropped"}
                ]
            }"#,
        )
        .unwrap();

        let loaded = storage.load_composer_state().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id, "session_kept");
        assert_eq!(loaded[0].draft, "kept");
    }

    /// The words are what cannot be retyped, so a record past the attachment
    /// budget keeps its text and loses the files.
    #[test]
    fn composer_state_drops_attachments_past_its_budget() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let payload = "A".repeat(MAX_COMPOSER_ATTACHMENT_BYTES / 2 + 1);
        let attachment = |label: &str| StoredComposerAttachment {
            label: label.to_string(),
            mime_type: None,
            data_url: format!("data:application/octet-stream;base64,{payload}"),
            inline_text_offset: None,
        };
        storage
            .save_composer_state(&[StoredComposerSession {
                session_id: "session_test".to_string(),
                saved_at_ms: 10,
                draft: "keep me".to_string(),
                attachments: vec![attachment("first"), attachment("second")],
                queued: Vec::new(),
            }])
            .unwrap();

        let loaded = storage.load_composer_state().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].draft, "keep me");
        assert_eq!(loaded[0].attachments.len(), 1);
        assert_eq!(loaded[0].attachments[0].label, "first");
    }

    #[test]
    fn unreadable_composer_state_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(storage.composer_state_path(), b"not-json").unwrap();

        let error = storage.load_composer_state().unwrap_err();
        assert_eq!(error.code, "mobile_composer_state_invalid");
        assert!(!storage.composer_state_path().exists());
    }

    #[test]
    fn malformed_app_settings_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(storage.app_settings_path(), b"not-json").unwrap();

        let error = storage.load_app_settings().unwrap_err();
        assert_eq!(error.code, "mobile_app_settings_invalid");
        assert!(!storage.app_settings_path().exists());
    }

    #[test]
    fn unknown_app_settings_values_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.app_settings_path(),
            br#"{"schemaVersion":"vibex-native-mobile-app-settings.v2","theme":"sepia"}"#,
        )
        .unwrap();

        let error = storage.load_app_settings().unwrap_err();
        assert_eq!(error.code, "mobile_app_settings_invalid");
        assert!(!storage.app_settings_path().exists());
    }

    #[test]
    fn credentials_without_local_network_route_remain_backward_compatible() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let mut encoded = serde_json::to_value(fixture()).unwrap();
        encoded
            .get_mut("route")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("localNetwork");
        fs::write(storage.path(), serde_json::to_vec(&encoded).unwrap()).unwrap();

        let loaded = storage.load().unwrap().unwrap();
        assert!(loaded.route.unwrap().local_network.is_none());
    }

    /// A connection-string pairing stores the server's own certificate in the
    /// route. Losing it on reload would silently drop the phone back to the
    /// public roots, which no LAN runtime is signed by.
    #[test]
    fn a_pinned_local_certificate_survives_a_reload() {
        use base64::Engine as _;

        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        // A minimal DER envelope: the transport decodes and pins it, while the
        // TLS stack is what validates the certificate itself.
        let mut certificate = vec![0x30, 0x82, 0x01, 0x00];
        certificate.extend(std::iter::repeat_n(0x41_u8, 252));
        let encoded_certificate =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&certificate);

        let mut bundle = fixture();
        bundle.record.server_url = "https://192.168.1.10:8765".to_string();
        bundle.route = Some(MobileRemoteRouteBundle {
            local_network: Some(MobileLocalNetworkCandidate {
                url: "https://192.168.1.10:8765".to_string(),
                tls_certificate_der: encoded_certificate.clone(),
            }),
            direct_candidates: Vec::new(),
            relay: None,
        });
        storage.save(&bundle).unwrap();

        let loaded = storage.load().unwrap().unwrap();
        let route = loaded.route.clone().unwrap();
        assert_eq!(
            route.local_network.unwrap().tls_certificate_der,
            encoded_certificate
        );
        let config = loaded.auto_transport_config().unwrap();
        let candidate = config.direct_candidates.first().expect("pinned route");
        assert_eq!(candidate.label, "local-network");
        assert_eq!(candidate.priority, 0);
        assert_eq!(
            candidate.tls_certificate_der.as_deref(),
            Some(encoded_certificate.as_str())
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        storage.save(&fixture()).unwrap();
        assert_eq!(
            fs::metadata(storage.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_credentials_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(storage.path(), b"not-json").unwrap();

        let error = storage.load().unwrap_err();
        assert_eq!(error.code, "mobile_credentials_invalid");
        assert!(!storage.path().exists());
    }

    #[test]
    fn semantically_invalid_credentials_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let mut invalid = fixture();
        invalid.schema_version = "future-schema".to_string();
        fs::write(storage.path(), serde_json::to_vec(&invalid).unwrap()).unwrap();

        let error = storage.load().unwrap_err();
        assert_eq!(error.code, "mobile_credentials_invalid");
        assert!(!storage.path().exists());
    }

    #[test]
    fn failed_atomic_replace_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir(storage.path()).unwrap();

        let error = storage.save(&fixture()).unwrap_err();
        assert_eq!(error.code, "mobile_credentials_write_failed");
        assert!(!temporary_path(&storage.path()).exists());
    }

    fn timeline_item(session_id: &VibexSessionId, sequence: i64) -> TimelineItem {
        let payload = vibex_core::TimelinePayload::UserMessage(Default::default());
        TimelineItem {
            id: vibex_core::TimelineItemId::parse(format!("timeline_mobile_{sequence}")).unwrap(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: sequence,
            source: vibex_core::TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
            payload,
        }
    }

    fn session_timeline(
        host_id: &str,
        session_id: &VibexSessionId,
        saved_at_ms: i64,
    ) -> StoredSessionTimeline {
        StoredSessionTimeline {
            host_id: host_id.to_string(),
            session_id: session_id.clone(),
            saved_at_ms,
            has_older: true,
            items: vec![timeline_item(session_id, 1), timeline_item(session_id, 2)],
        }
    }

    #[test]
    fn session_timelines_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let session = VibexSessionId::new();
        let record = session_timeline("desktop-primary", &session, 1_700_000_000_000);
        assert!(
            storage
                .load_session_timeline("desktop-primary", &session)
                .unwrap()
                .is_none()
        );

        storage.save_session_timeline(&record).unwrap();
        let loaded = storage
            .load_session_timeline("desktop-primary", &session)
            .unwrap()
            .expect("a saved window should load again");
        assert_eq!(loaded.session_id, session);
        assert!(loaded.has_older);
        assert_eq!(
            loaded
                .items
                .iter()
                .map(|item| item.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        storage.clear_session_timelines().unwrap();
        assert!(
            storage
                .load_session_timeline("desktop-primary", &session)
                .unwrap()
                .is_none()
        );
    }

    /// Session ids are minted per runtime, so only the pair the record was
    /// written under may read it back.
    #[test]
    fn a_session_timeline_is_never_returned_for_another_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let session = VibexSessionId::new();
        storage
            .save_session_timeline(&session_timeline("desktop-primary", &session, 1))
            .unwrap();

        assert!(
            storage
                .load_session_timeline("desktop-secondary", &session)
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .load_session_timeline("desktop-primary", &VibexSessionId::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn session_timelines_keep_only_the_newest_windows() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let sessions = (0..MAX_SESSION_TIMELINES + 1)
            .map(|_| VibexSessionId::new())
            .collect::<Vec<_>>();
        for (index, session) in sessions.iter().enumerate() {
            storage
                .save_session_timeline(&session_timeline(
                    "desktop-primary",
                    session,
                    1_700_000_000_000 + index as i64,
                ))
                .unwrap();
        }

        for session in &sessions[1..] {
            assert!(
                storage
                    .load_session_timeline("desktop-primary", session)
                    .unwrap()
                    .is_some(),
                "the newer windows should survive"
            );
        }
        assert!(
            storage
                .load_session_timeline("desktop-primary", &sessions[0])
                .unwrap()
                .is_none(),
            "the oldest window is the one evicted"
        );
    }

    /// The file's byte cap evicts oldest-first even when the record count
    /// alone would allow the write.
    #[test]
    fn session_timelines_evict_by_bytes_too() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let oversized = "x".repeat(MAX_SESSION_TIMELINES_BYTES as usize / 2 + 1024);
        let older_session = VibexSessionId::new();
        let mut older = session_timeline("desktop-primary", &older_session, 1);
        older.items = vec![timeline_item(&older_session, 1)];
        older.items[0].payload =
            vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: oversized.clone(),
                ..Default::default()
            });
        let newest_session = VibexSessionId::new();
        let mut newest = session_timeline("desktop-primary", &newest_session, 2);
        newest.items = vec![timeline_item(&newest_session, 1)];
        newest.items[0].payload =
            vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: oversized,
                ..Default::default()
            });

        storage.save_session_timeline(&older).unwrap();
        storage.save_session_timeline(&newest).unwrap();

        assert!(
            storage
                .load_session_timeline("desktop-primary", &older_session)
                .unwrap()
                .is_none(),
            "the older window is dropped to fit the cap"
        );
        let loaded = storage
            .load_session_timeline("desktop-primary", &newest_session)
            .unwrap()
            .expect("the newer window should survive");
        assert_eq!(loaded.items.len(), 1);
    }

    /// The window is stored at summary depth: detail is the large part of a
    /// timeline and is re-read one item at a time when a reader opens it.
    #[test]
    fn stored_session_timelines_carry_no_tool_detail() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let session = VibexSessionId::new();
        let mut record = session_timeline("desktop-primary", &session, 1);
        record.items[0].payload =
            vibex_core::TimelinePayload::FileOperation(vibex_core::FileOperationPayload {
                operation: vibex_core::FileOperationKind::Edit,
                path: "/work/vibex/src/main.rs".to_string(),
                summary: "edited main.rs".to_string(),
                old_text: Some("x".repeat(4096)),
                new_text: Some("y".repeat(4096)),
                patch: None,
                raw_extension: None,
            });
        assert!(record.items[0].has_complete_payload());

        storage.save_session_timeline(&record).unwrap();

        let written = fs::read_to_string(storage.session_timelines_path()).unwrap();
        assert!(!written.contains(&"x".repeat(4096)));
        let stored: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(stored["sessions"][0]["items"][0]["payloadDepth"], "summary");
        let loaded = storage
            .load_session_timeline("desktop-primary", &session)
            .unwrap()
            .expect("a saved window should load again");
        assert!(!loaded.items[0].has_complete_payload());
    }

    /// A window naming another session is not this session's conversation, so
    /// the record is dropped whole instead of painted.
    #[test]
    fn a_session_timeline_holding_another_sessions_items_is_discarded() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let session = VibexSessionId::new();
        let mut record = session_timeline("desktop-primary", &session, 1);
        record.items[0] = timeline_item(&VibexSessionId::new(), 1);
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.session_timelines_path(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": SESSION_TIMELINES_SCHEMA_VERSION,
                "sessions": [record],
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(
            storage
                .load_session_timeline("desktop-primary", &session)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_oversized_session_timeline_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let session = VibexSessionId::new();
        let mut record = session_timeline("desktop-primary", &session, 1);
        record.items = (0..MAX_SESSION_TIMELINE_ITEMS as i64 + 1)
            .map(|sequence| timeline_item(&session, sequence))
            .collect();

        let error = storage.save_session_timeline(&record).unwrap_err();
        assert_eq!(error.code, "mobile_session_timelines_invalid");
        assert!(!storage.session_timelines_path().exists());
    }

    #[test]
    fn malformed_session_timelines_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(storage.session_timelines_path(), b"not-json").unwrap();

        let error = storage
            .load_session_timeline("desktop-primary", &VibexSessionId::new())
            .unwrap_err();
        assert_eq!(error.code, "mobile_session_timelines_invalid");
        assert!(!storage.session_timelines_path().exists());
    }

    #[test]
    fn unknown_session_timelines_schema_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(
            storage.session_timelines_path(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": "vibex-native-mobile-session-timelines.v2",
                "sessions": [],
            }))
            .unwrap(),
        )
        .unwrap();

        let error = storage
            .load_session_timeline("desktop-primary", &VibexSessionId::new())
            .unwrap_err();
        assert_eq!(error.code, "mobile_session_timelines_invalid");
        assert!(!storage.session_timelines_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn session_timelines_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        storage
            .save_session_timeline(&session_timeline(
                "desktop-primary",
                &VibexSessionId::new(),
                1,
            ))
            .unwrap();
        assert_eq!(
            fs::metadata(storage.session_timelines_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

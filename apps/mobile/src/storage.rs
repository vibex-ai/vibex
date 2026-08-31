use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_backend::{BackendError, BackendResult};
use vibex_core::AgentTimelineReasoningDisplayMode;

use crate::pairing::MobileCredentialBundle;

const CREDENTIAL_FILE: &str = "remote-credentials.json";
const HOSTS_FILE: &str = "remote-hosts.json";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const MAX_HOSTS: usize = 32;
const MAX_HOST_BYTES: u64 = 2 * 1024 * 1024;
const HOSTS_SCHEMA_VERSION: &str = "vibex-native-mobile-hosts.v1";
const TIMELINE_DISPLAY_SETTINGS_FILE: &str = "timeline-display-settings.json";
const TIMELINE_DISPLAY_SETTINGS_SCHEMA_VERSION: &str =
    "vibex-native-mobile-timeline-display-settings.v1";
const MAX_TIMELINE_DISPLAY_SETTINGS_BYTES: u64 = 128 * 1024;
const MAX_TIMELINE_DISPLAY_SETTINGS_OVERRIDES: usize = 32;
const MAX_TIMELINE_DISPLAY_SETTINGS_HOST_ID_BYTES: usize = 256;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredHosts {
    schema_version: String,
    hosts: Vec<MobileCredentialBundle>,
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

    pub fn load_hosts(&self) -> BackendResult<Vec<MobileCredentialBundle>> {
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
        let stored: StoredHosts = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(_) => return self.reject_invalid_hosts(&path),
        };
        if stored.schema_version != HOSTS_SCHEMA_VERSION || stored.hosts.len() > MAX_HOSTS {
            return self.reject_invalid_hosts(&path);
        }
        for bundle in &stored.hosts {
            if bundle.validate().is_err() {
                return self.reject_invalid_hosts(&path);
            }
        }
        Ok(stored.hosts)
    }

    pub fn save_hosts(&self, hosts: &[MobileCredentialBundle]) -> BackendResult<()> {
        if hosts.len() > MAX_HOSTS {
            return Err(storage_error("mobile_hosts_invalid"));
        }
        for bundle in hosts {
            bundle.validate()?;
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
        let path = self.hosts_path();
        let temporary = temporary_path(&path);
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(storage_error("mobile_hosts_write_failed")),
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
                .map_err(|_| storage_error("mobile_hosts_write_failed"))?;
            file.write_all(&encoded)
                .and_then(|_| file.sync_all())
                .map_err(|_| storage_error("mobile_hosts_write_failed"))?;
            fs::rename(&temporary, &path)
                .map_err(|_| storage_error("mobile_hosts_write_failed"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .map_err(|_| storage_error("mobile_hosts_write_failed"))?;
            }
            Ok(())
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome
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

    fn reject_invalid_hosts(&self, path: &Path) -> BackendResult<Vec<MobileCredentialBundle>> {
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
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn storage_error(code: &'static str) -> BackendError {
    BackendError::failed(code, "native mobile credential storage is unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentTimelineReasoningDisplayMode, RemoteAuthProof, RemoteClientType};
    use vibex_remote_client::{ClientDeviceIdentity, RemoteCredentialRecord};

    use crate::pairing::MobileRemoteRouteBundle;

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
    fn hosts_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let first = fixture();
        let mut second = fixture();
        second.expected_server_id = "second-desktop".to_string();
        storage
            .save_hosts(&[first.clone(), second.clone()])
            .unwrap();
        assert_eq!(storage.load_hosts().unwrap(), vec![first, second]);
        storage.clear_hosts().unwrap();
        assert!(storage.load_hosts().unwrap().is_empty());
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
}

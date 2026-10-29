//! Persistent SSH connection profiles.
//!
//! The profile is the desktop's memory of one SSH-reachable host: the alias it
//! was discovered from, the resolved host/user/port, the ports used for the
//! tunnel and the remote runtime, and the install roots. It never carries a
//! private key, password, passphrase, pairing code, or auth token.

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_core::{VibexError, VibexResult};

pub const STORE_FILE_NAME: &str = "ssh-connections.json";
pub const SCHEMA_VERSION: u32 = 1;

const MAX_TEXT: usize = 256;
const MAX_PATH: usize = 1024;
const MAX_VERSION: usize = 64;
const MAX_WORKSPACE_ROOTS: usize = 64;

/// Release channel an SSH-provisioned runtime follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshRuntimeChannel {
    #[default]
    Stable,
    Rc,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SshUpgradePolicy {
    #[default]
    Notify,
    WhenIdle,
    Pinned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SshMacOsLifecycle {
    #[default]
    Nohup,
    LaunchAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionProfile {
    pub id: String,
    pub display_name: String,
    /// The `~/.ssh/config` alias this profile was discovered from. Empty for a
    /// manually added host that never had an alias.
    pub ssh_alias: String,
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
    pub ssh_client_path: Option<PathBuf>,
    pub ssh_config_path: Option<PathBuf>,
    /// Path only; the key material never leaves the user's SSH agent/home.
    pub identity_file: Option<PathBuf>,
    pub proxy_jump: Option<String>,
    pub remote_install_root: String,
    pub remote_home: String,
    /// OS reported by the environment probe when known (linux/macos/...).
    #[serde(default)]
    pub remote_os: Option<String>,
    pub workspace_roots: Vec<String>,
    pub remote_port: u16,
    pub local_tunnel_port: u16,
    pub channel: SshRuntimeChannel,
    pub runtime_version: Option<String>,
    #[serde(default)]
    pub upgrade_policy: SshUpgradePolicy,
    #[serde(default)]
    pub macos_lifecycle: SshMacOsLifecycle,
    /// The registry id this profile produced, when it has been provisioned.
    /// Optional so stores written before runtime binding remain valid.
    #[serde(default)]
    pub registered_runtime_id: Option<String>,
    pub created_at_ms: i64,
    pub last_connected_at_ms: Option<i64>,
}

impl SshConnectionProfile {
    /// Rejects values that would make the profile unusable or unsafe to pass
    /// to an SSH child process.
    pub fn validate(&self) -> VibexResult<()> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(invalid(
                "id",
                "must be 1-64 characters of letters, digits, '_' or '-'",
            ));
        }
        validate_text("displayName", &self.display_name, MAX_TEXT)?;
        if !self.ssh_alias.is_empty() {
            // Manual hosts have no `~/.ssh/config` alias; the resolved host is
            // the only target in that case.
            validate_ssh_token("sshAlias", &self.ssh_alias, 255)?;
        }
        validate_ssh_token("host", &self.host, 255)?;
        if let Some(user) = &self.user {
            validate_user(user)?;
        }
        validate_port("port", self.port)?;
        if let Some(path) = &self.ssh_client_path {
            validate_local_path("sshClientPath", path)?;
        }
        if let Some(path) = &self.ssh_config_path {
            validate_local_path_or_home("sshConfigPath", path)?;
        }
        if let Some(path) = &self.identity_file {
            validate_local_path_or_home("identityFile", path)?;
        }
        if let Some(proxy) = &self.proxy_jump {
            validate_ssh_token("proxyJump", proxy, 512)?;
        }
        validate_remote_path("remoteInstallRoot", &self.remote_install_root)?;
        validate_remote_path("remoteHome", &self.remote_home)?;
        if let Some(remote_os) = &self.remote_os {
            validate_text("remoteOs", remote_os, 32)?;
        }
        if self.workspace_roots.len() > MAX_WORKSPACE_ROOTS {
            return Err(invalid(
                "workspaceRoots",
                "must contain at most 64 workspace roots",
            ));
        }
        for root in &self.workspace_roots {
            validate_remote_path("workspaceRoots", root)?;
        }
        validate_port("remotePort", self.remote_port)?;
        validate_port("localTunnelPort", self.local_tunnel_port)?;
        if let Some(version) = &self.runtime_version {
            validate_version(version)?;
        }
        if let Some(runtime_id) = &self.registered_runtime_id
            && (runtime_id.is_empty()
                || runtime_id.len() > 64
                || !runtime_id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                }))
        {
            return Err(invalid(
                "registeredRuntimeId",
                "must be 1-64 characters of letters, digits, '_' or '-'",
            ));
        }
        if self.created_at_ms < 0 {
            return Err(invalid("createdAtMs", "must not be negative"));
        }
        if self.last_connected_at_ms.is_some_and(|value| value < 0) {
            return Err(invalid("lastConnectedAtMs", "must not be negative"));
        }
        Ok(())
    }
}

/// One JSON file beside the desktop runtime home.
#[derive(Debug, Clone)]
pub struct SshProfileStore {
    path: PathBuf,
}

impl SshProfileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn at_home(home: &Path) -> Self {
        Self::new(home.join(STORE_FILE_NAME))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads all usable profiles. A missing file is an empty list. A corrupt
    /// file or record is copied to a `.corrupt-*` backup and skipped so one bad
    /// record cannot block Connections.
    pub fn load(&self) -> VibexResult<Vec<SshConnectionProfile>> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(_) => {
                return Err(VibexError::storage(
                    "ssh_profile_store_read_failed",
                    "the SSH connection store could not be read",
                ));
            }
        };
        repair_private_permissions(&self.path)?;

        let value = match serde_json::from_slice::<serde_json::Value>(&raw) {
            Ok(value) => value,
            Err(_) => {
                self.backup_corrupt();
                return Ok(Vec::new());
            }
        };
        let items = match profiles_from_value(&value) {
            Ok(items) => items,
            Err(error) => {
                self.backup_corrupt();
                return Err(error);
            }
        };

        let mut dropped = false;
        let mut seen = HashSet::new();
        let mut profiles = Vec::new();
        for item in items {
            let Ok(profile) = serde_json::from_value::<SshConnectionProfile>(item) else {
                dropped = true;
                continue;
            };
            if profile.validate().is_err() || !seen.insert(profile.id.clone()) {
                dropped = true;
                continue;
            }
            profiles.push(profile);
        }
        if dropped {
            self.backup_corrupt();
        }
        profiles.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(profiles)
    }

    /// Validates and atomically replaces the store. A failed validation never
    /// touches the existing file.
    pub fn save(&self, profiles: &[SshConnectionProfile]) -> VibexResult<()> {
        let mut sorted = profiles.to_vec();
        sorted.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut seen = HashSet::new();
        for profile in &sorted {
            profile.validate()?;
            if !seen.insert(profile.id.clone()) {
                return Err(invalid("id", "duplicate profile id"));
            }
        }
        let file = SshProfileFile {
            schema_version: SCHEMA_VERSION,
            profiles: sorted,
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|_| {
            VibexError::storage(
                "ssh_profile_store_encode_failed",
                "the SSH connection store could not be encoded",
            )
        })?;
        self.write_atomic(&bytes)
    }

    fn write_atomic(&self, bytes: &[u8]) -> VibexResult<()> {
        let parent = self.path.parent().ok_or_else(|| {
            VibexError::storage(
                "ssh_profile_store_path_invalid",
                "the SSH connection store has no parent directory",
            )
        })?;
        fs::create_dir_all(parent).map_err(|_| {
            VibexError::storage(
                "ssh_profile_store_directory_failed",
                "the SSH connection store directory could not be created",
            )
        })?;

        let temporary = self.path.with_extension(format!(
            "json.tmp-{}",
            vibex_core::RequestId::new().as_str()
        ));
        let result = self
            .write_temporary(&temporary, bytes)
            .and_then(|()| set_private_permissions(&temporary))
            .and_then(|()| commit(&temporary, &self.path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        set_private_permissions(&self.path)?;
        Ok(())
    }

    fn write_temporary(&self, path: &Path, bytes: &[u8]) -> VibexResult<()> {
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt as _;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        };
        #[cfg(not(unix))]
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path);
        let mut file = file.map_err(|_| {
            VibexError::storage(
                "ssh_profile_store_write_failed",
                "the SSH connection store could not be written",
            )
        })?;
        file.write_all(bytes).map_err(|_| {
            VibexError::storage(
                "ssh_profile_store_write_failed",
                "the SSH connection store could not be written",
            )
        })?;
        file.sync_all().map_err(|_| {
            VibexError::storage(
                "ssh_profile_store_sync_failed",
                "the SSH connection store could not be synchronized",
            )
        })?;
        Ok(())
    }

    fn backup_corrupt(&self) -> Option<PathBuf> {
        let backup = self.path.with_extension(format!(
            "json.corrupt-{}",
            vibex_core::RequestId::new().as_str()
        ));
        fs::copy(&self.path, &backup).ok()?;
        let _ = set_private_permissions(&backup);
        Some(backup)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SshProfileFile {
    schema_version: u32,
    profiles: Vec<SshConnectionProfile>,
}

/// Accepts the current envelope and the legacy bare-array shape. A future
/// schema is rejected without deleting the original file.
fn profiles_from_value(value: &serde_json::Value) -> VibexResult<Vec<serde_json::Value>> {
    match value {
        serde_json::Value::Array(items) => Ok(items.clone()),
        serde_json::Value::Object(map) => {
            let version = map
                .get("schemaVersion")
                .or_else(|| map.get("schema_version"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if version > u64::from(SCHEMA_VERSION) {
                return Err(VibexError::capability(
                    "ssh_profile_schema_unsupported",
                    "the SSH connection store was written by a newer Vibex version",
                ));
            }
            match map.get("profiles").or_else(|| map.get("connections")) {
                None => Ok(Vec::new()),
                Some(serde_json::Value::Array(items)) => Ok(items.clone()),
                Some(_) => Err(VibexError::validation(
                    "ssh_profile_store_corrupt",
                    "the SSH connection store has an invalid profiles field",
                )),
            }
        }
        _ => Err(VibexError::validation(
            "ssh_profile_store_corrupt",
            "the SSH connection store is not an object or array",
        )),
    }
}

fn invalid(field: &str, reason: &str) -> VibexError {
    VibexError::validation(
        "ssh_profile_invalid",
        "the SSH connection profile is invalid",
    )
    .with_diagnostic("field", field)
    .with_diagnostic("reason", reason)
}

fn validate_text(field: &str, value: &str, max: usize) -> VibexResult<()> {
    if value.trim().is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.len() > max {
        return Err(invalid(field, "is too long"));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

fn validate_ssh_token(field: &str, value: &str, max: usize) -> VibexResult<()> {
    validate_text(field, value, max)?;
    if value.starts_with('-') {
        return Err(invalid(field, "must not start with '-'"));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(invalid(field, "must not contain whitespace"));
    }
    if value.contains('*') || value.contains('?') {
        return Err(invalid(field, "must not contain a wildcard"));
    }
    Ok(())
}

fn validate_user(value: &str) -> VibexResult<()> {
    validate_text("user", value, 64)?;
    if value.starts_with('-')
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return Err(invalid(
            "user",
            "must contain only letters, digits, '.', '_' or '-'",
        ));
    }
    Ok(())
}

fn validate_local_path(field: &str, path: &Path) -> VibexResult<()> {
    let text = path.to_string_lossy();
    validate_text(field, &text, MAX_PATH)?;
    if !path.is_absolute() {
        return Err(invalid(field, "must be an absolute path"));
    }
    Ok(())
}

/// Config and identity paths may be written as `~/.ssh/...`, because that is
/// the shape `~/.ssh/config` and `ssh -G` use. They are expanded when used.
fn validate_local_path_or_home(field: &str, path: &Path) -> VibexResult<()> {
    let text = path.to_string_lossy();
    validate_text(field, &text, MAX_PATH)?;
    if path.is_absolute() || text == "~" || text.starts_with("~/") {
        return Ok(());
    }
    Err(invalid(
        field,
        "must be an absolute path or start with '~/'",
    ))
}

fn validate_remote_path(field: &str, value: &str) -> VibexResult<()> {
    validate_text(field, value, MAX_PATH)?;
    let rooted = value.starts_with('/')
        || value == "~"
        || value.starts_with("~/")
        || value == "$HOME"
        || value.starts_with("$HOME/");
    if !rooted {
        return Err(invalid(field, "must start with '/', '~/', or '$HOME/'"));
    }
    if value.split('/').any(|segment| segment == "..") {
        return Err(invalid(field, "must not contain '..'"));
    }
    Ok(())
}

fn validate_port(field: &str, port: u16) -> VibexResult<()> {
    if port == 0 {
        return Err(invalid(field, "must be between 1 and 65535"));
    }
    Ok(())
}

fn validate_version(value: &str) -> VibexResult<()> {
    validate_text("runtimeVersion", value, MAX_VERSION)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+'))
    {
        return Err(invalid(
            "runtimeVersion",
            "must contain only letters, digits, '.', '-' or '+'",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| {
        VibexError::storage(
            "ssh_profile_store_permissions_failed",
            "the SSH connection store could not be restricted to 0600",
        )
    })
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

#[cfg(unix)]
fn repair_private_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path)
        .map_err(|_| {
            VibexError::storage(
                "ssh_profile_store_read_failed",
                "the SSH connection store could not be inspected",
            )
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        set_private_permissions(path)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn repair_private_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

#[cfg(windows)]
fn commit(temporary: &Path, target: &Path) -> VibexResult<()> {
    let _ = fs::remove_file(target);
    fs::rename(temporary, target).map_err(|_| {
        VibexError::storage(
            "ssh_profile_store_commit_failed",
            "the SSH connection store could not be committed",
        )
    })
}

#[cfg(not(windows))]
fn commit(temporary: &Path, target: &Path) -> VibexResult<()> {
    fs::rename(temporary, target).map_err(|_| {
        VibexError::storage(
            "ssh_profile_store_commit_failed",
            "the SSH connection store could not be committed",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn sample() -> SshConnectionProfile {
        SshConnectionProfile {
            id: "ssh-1".to_string(),
            display_name: "Build box".to_string(),
            ssh_alias: "build-box".to_string(),
            host: "build.example.com".to_string(),
            user: Some("dev".to_string()),
            port: 22,
            ssh_client_path: Some(PathBuf::from("/usr/bin/ssh")),
            ssh_config_path: Some(PathBuf::from("/home/dev/.ssh/config")),
            identity_file: Some(PathBuf::from("/home/dev/.ssh/id_ed25519")),
            proxy_jump: Some("bastion.example.com".to_string()),
            remote_install_root: "~/.local/share/vibex".to_string(),
            remote_home: "~/.local/share/vibex/server/home".to_string(),
            remote_os: None,
            workspace_roots: vec!["$HOME".to_string(), "/srv/src".to_string()],
            remote_port: 8765,
            local_tunnel_port: 18765,
            channel: SshRuntimeChannel::Rc,
            runtime_version: Some("0.1.0-rc.8".to_string()),
            upgrade_policy: SshUpgradePolicy::Notify,
            macos_lifecycle: SshMacOsLifecycle::Nohup,
            registered_runtime_id: None,
            created_at_ms: 1_700_000_000_000,
            last_connected_at_ms: Some(1_700_000_100_000),
        }
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".corrupt-"))
            })
            .collect()
    }

    #[test]
    fn round_trips_every_field_and_sorts_by_creation() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        let mut newer = sample();
        newer.id = "ssh-2".to_string();
        newer.created_at_ms += 1;
        store.save(&[newer.clone(), sample()]).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![sample(), newer]);
        assert!(backups(dir.path()).is_empty());
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_0600_and_repairs_a_loose_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        store.save(&[sample()]).unwrap();
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![sample()]);
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn skips_a_corrupt_record_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        let file = json!({
            "schemaVersion": 1,
            "profiles": [serde_json::to_value(sample()).unwrap(), {"id": ""}]
        });
        fs::write(store.path(), serde_json::to_vec(&file).unwrap()).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![sample()]);
        assert_eq!(backups(dir.path()).len(), 1);
    }

    #[test]
    fn invalid_json_is_backed_up_and_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        fs::write(store.path(), b"{not json").unwrap();

        assert!(store.load().unwrap().is_empty());
        assert_eq!(backups(dir.path()).len(), 1);
        assert!(store.path().exists());
    }

    #[test]
    fn migrates_a_legacy_bare_array() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        fs::write(store.path(), serde_json::to_vec(&vec![sample()]).unwrap()).unwrap();

        assert_eq!(store.load().unwrap(), vec![sample()]);
        assert!(backups(dir.path()).is_empty());
        store.save(&[sample()]).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(saved["schemaVersion"], 1);
    }

    #[test]
    fn rejects_a_future_schema_without_deleting_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        fs::write(store.path(), br#"{"schemaVersion":99,"profiles":[]}"#).unwrap();

        let error = store.load().unwrap_err();
        assert_eq!(error.code, "ssh_profile_schema_unsupported");
        assert!(store.path().exists());
        assert_eq!(backups(dir.path()).len(), 1);
    }

    #[test]
    fn validation_rejects_unsafe_or_empty_fields() {
        let mut profile = sample();
        profile.port = 0;
        assert_eq!(profile.validate().unwrap_err().code, "ssh_profile_invalid");

        let mut profile = sample();
        profile.display_name = "bad\nname".to_string();
        assert_eq!(profile.validate().unwrap_err().code, "ssh_profile_invalid");

        let mut profile = sample();
        profile.ssh_client_path = Some(PathBuf::from("ssh"));
        assert_eq!(profile.validate().unwrap_err().code, "ssh_profile_invalid");

        let mut profile = sample();
        profile.remote_home = "../etc".to_string();
        assert_eq!(profile.validate().unwrap_err().code, "ssh_profile_invalid");

        let mut profile = sample();
        profile.id = String::new();
        assert_eq!(profile.validate().unwrap_err().code, "ssh_profile_invalid");
    }

    #[test]
    fn registered_runtime_id_must_be_registry_safe() {
        let mut profile = sample();
        profile.registered_runtime_id = Some("server-1".to_string());
        profile.validate().unwrap();
        profile.registered_runtime_id = Some("not a registry id".to_string());
        assert_eq!(profile.validate().unwrap_err().code, "ssh_profile_invalid");
    }

    #[test]
    fn allows_home_relative_config_and_identity_paths() {
        let mut profile = sample();
        profile.ssh_config_path = Some(PathBuf::from("~/.ssh/config"));
        profile.identity_file = Some(PathBuf::from("~/.ssh/id_ed25519"));
        profile.validate().unwrap();
    }

    #[test]
    fn allows_a_manual_profile_without_an_alias() {
        let mut profile = sample();
        profile.ssh_alias = String::new();
        profile.validate().unwrap();
    }

    #[test]
    fn save_rejects_duplicate_ids_and_keeps_the_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshProfileStore::at_home(dir.path());
        store.save(&[sample()]).unwrap();
        let before = fs::read(store.path()).unwrap();

        assert_eq!(
            store.save(&[sample(), sample()]).unwrap_err().code,
            "ssh_profile_invalid"
        );
        assert_eq!(fs::read(store.path()).unwrap(), before);
    }
}

//! Persisted WSL connection metadata.
//!
//! WSL is local and has no SSH profile, but the workbench still needs a stable
//! id, distro name, runtime paths, and the registry id it produced. Keep this
//! in its own file rather than overloading the SSH profile schema.

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_core::{VibexError, VibexResult};

pub const WSL_STORE_FILE: &str = "wsl-connections.json";
pub const WSL_SCHEMA_VERSION: u32 = 1;

const MAX_DISPLAY_NAME: usize = 256;
const MAX_DISTRO_NAME: usize = 128;
const MAX_PATH: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WslConnectionProfile {
    pub id: String,
    pub distro: String,
    pub display_name: String,
    pub remote_home: String,
    pub install_root: String,
    pub workspace_roots: Vec<String>,
    pub remote_port: u16,
    #[serde(default)]
    pub registered_runtime_id: Option<String>,
    pub created_at_ms: i64,
    pub last_connected_at_ms: Option<i64>,
}

impl WslConnectionProfile {
    pub fn validate(&self) -> VibexResult<()> {
        validate_registry_id("id", &self.id)?;
        if self.distro.trim().is_empty()
            || self.distro.len() > MAX_DISTRO_NAME
            || self.distro.chars().any(char::is_control)
        {
            return Err(invalid("distro", "must name a valid WSL distro"));
        }
        if self.display_name.trim().is_empty()
            || self.display_name.len() > MAX_DISPLAY_NAME
            || self.display_name.chars().any(char::is_control)
        {
            return Err(invalid("displayName", "must be a valid display name"));
        }
        validate_linux_path("remoteHome", &self.remote_home)?;
        validate_linux_path("installRoot", &self.install_root)?;
        if !self
            .install_root
            .starts_with(&format!("{}/", self.remote_home.trim_end_matches('/')))
            || self.install_root == self.remote_home
        {
            return Err(invalid("installRoot", "must be inside remoteHome"));
        }
        if self.workspace_roots.is_empty() || self.workspace_roots.len() > 64 {
            return Err(invalid("workspaceRoots", "must be absolute paths"));
        }
        for root in &self.workspace_roots {
            validate_linux_path("workspaceRoots", root)?;
        }
        if let Some(runtime_id) = self.registered_runtime_id.as_deref() {
            validate_registry_id("registeredRuntimeId", runtime_id)?;
        }
        if self.remote_port == 0 {
            return Err(invalid("remotePort", "must not be zero"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WslProfileStore {
    path: PathBuf,
}

impl WslProfileStore {
    pub fn at_home(home: &Path) -> Self {
        Self {
            path: home.join(WSL_STORE_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> VibexResult<Vec<WslConnectionProfile>> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => {
                return Err(VibexError::storage(
                    "wsl_profile_read_failed",
                    "the WSL connection store could not be read",
                ));
            }
        };
        let value: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| {
            VibexError::storage("wsl_profile_invalid", "the WSL connection store is invalid")
        })?;
        if value.get("schemaVersion").and_then(|v| v.as_u64())
            != Some(u64::from(WSL_SCHEMA_VERSION))
        {
            return Err(VibexError::storage(
                "wsl_profile_schema_unsupported",
                "unsupported WSL profile schema",
            ));
        }
        let entries = value
            .get("profiles")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let mut seen = HashSet::new();
        let mut profiles = Vec::new();
        for entry in entries {
            let Ok(profile) = serde_json::from_value::<WslConnectionProfile>(entry) else {
                continue;
            };
            if profile.validate().is_ok() && seen.insert(profile.id.clone()) {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    pub fn save(&self, profiles: &[WslConnectionProfile]) -> VibexResult<()> {
        let mut seen = HashSet::new();
        for profile in profiles {
            profile.validate()?;
            if !seen.insert(profile.id.clone()) {
                return Err(invalid("id", "duplicate profile id"));
            }
        }
        let value = serde_json::json!({
            "schemaVersion": WSL_SCHEMA_VERSION,
            "profiles": profiles,
        });
        let bytes = serde_json::to_vec_pretty(&value).map_err(|_| {
            VibexError::storage(
                "wsl_profile_encode_failed",
                "the WSL store could not be encoded",
            )
        })?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                VibexError::storage(
                    "wsl_profile_directory_failed",
                    "the WSL store directory could not be created",
                )
            })?;
        }
        let temp = self.path.with_extension(format!(
            "json.tmp-{}",
            vibex_core::RequestId::new().as_str()
        ));
        let write_result = write_temp_file(&temp, &bytes).and_then(|()| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = fs::set_permissions(&temp, fs::Permissions::from_mode(0o600));
            }
            commit_file(&temp, &self.path)
        });
        if write_result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        write_result
    }
}

fn write_temp_file(path: &Path, bytes: &[u8]) -> VibexResult<()> {
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
            "wsl_profile_write_failed",
            "the WSL store could not be written",
        )
    })?;
    file.write_all(bytes).map_err(|_| {
        VibexError::storage(
            "wsl_profile_write_failed",
            "the WSL store could not be written",
        )
    })?;
    file.sync_all().map_err(|_| {
        VibexError::storage(
            "wsl_profile_sync_failed",
            "the WSL store could not be synchronized",
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn commit_file(temporary: &Path, target: &Path) -> VibexResult<()> {
    let _ = fs::remove_file(target);
    fs::rename(temporary, target).map_err(|_| {
        VibexError::storage(
            "wsl_profile_rename_failed",
            "the WSL store could not be replaced",
        )
    })
}

#[cfg(not(windows))]
fn commit_file(temporary: &Path, target: &Path) -> VibexResult<()> {
    fs::rename(temporary, target).map_err(|_| {
        VibexError::storage(
            "wsl_profile_rename_failed",
            "the WSL store could not be replaced",
        )
    })
}

fn validate_registry_id(field: &str, value: &str) -> VibexResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(invalid(field, "must be a registry-safe id"));
    }
    Ok(())
}

fn validate_linux_path(field: &str, value: &str) -> VibexResult<()> {
    if !value.starts_with('/')
        || value.len() > MAX_PATH
        || value.chars().any(char::is_control)
        || value.split('/').any(|segment| segment == "..")
    {
        return Err(invalid(
            field,
            "must be a safe absolute Linux path without '..'",
        ));
    }
    Ok(())
}

fn invalid(field: &str, reason: &str) -> VibexError {
    VibexError::validation(
        "wsl_profile_invalid",
        "the WSL connection profile is invalid",
    )
    .with_diagnostic("field", field)
    .with_diagnostic("reason", reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WslConnectionProfile {
        WslConnectionProfile {
            id: "wsl-ubuntu".to_string(),
            distro: "Ubuntu-24.04".to_string(),
            display_name: "Ubuntu".to_string(),
            remote_home: "/home/dev".to_string(),
            install_root: "/home/dev/.local/share/vibex/server".to_string(),
            workspace_roots: vec!["/home/dev".to_string()],
            remote_port: 8765,
            registered_runtime_id: None,
            created_at_ms: 1,
            last_connected_at_ms: None,
        }
    }

    #[test]
    fn round_trips_a_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = WslProfileStore::at_home(dir.path());
        store.save(&[sample()]).unwrap();
        assert_eq!(store.load().unwrap(), vec![sample()]);
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-"))
        );
    }

    #[test]
    fn rejects_unsafe_roots_and_control_chars() {
        let mut profile = sample();
        profile.install_root = "/etc".to_string();
        assert!(profile.validate().is_err());
        profile.install_root = profile.remote_home.clone();
        assert!(profile.validate().is_err());
        profile.install_root = "/home/dev/../etc".to_string();
        assert!(profile.validate().is_err());

        let mut profile = sample();
        profile.workspace_roots = vec!["/home/dev/../etc".to_string()];
        assert!(profile.validate().is_err());

        let mut profile = sample();
        profile.display_name = "bad\nname".to_string();
        assert!(profile.validate().is_err());
    }

    #[test]
    fn save_rejects_duplicate_ids_and_keeps_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = WslProfileStore::at_home(dir.path());
        store.save(&[sample()]).unwrap();
        let before = fs::read(store.path()).unwrap();
        assert_eq!(
            store.save(&[sample(), sample()]).unwrap_err().code,
            "wsl_profile_invalid"
        );
        assert_eq!(fs::read(store.path()).unwrap(), before);
    }
}

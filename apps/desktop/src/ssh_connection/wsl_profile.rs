//! Persisted WSL connection metadata.
//!
//! WSL is local and has no SSH profile, but the workbench still needs a stable
//! id, distro name, runtime paths, and the registry id it produced. Keep this
//! in its own file rather than overloading the SSH profile schema.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vibex_core::{VibexError, VibexResult};

pub const WSL_STORE_FILE: &str = "wsl-connections.json";
pub const WSL_SCHEMA_VERSION: u32 = 1;

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
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(invalid("id", "must be a registry-safe id"));
        }
        if self.distro.trim().is_empty() || self.distro.contains('\0') || self.distro.contains('\n')
        {
            return Err(invalid("distro", "must name a WSL distro"));
        }
        if self.display_name.trim().is_empty() {
            return Err(invalid("displayName", "must not be empty"));
        }
        if !self.remote_home.starts_with('/') || self.remote_home.contains('\n') {
            return Err(invalid("remoteHome", "must be an absolute Linux path"));
        }
        if !self
            .install_root
            .starts_with(&format!("{}/", self.remote_home.trim_end_matches('/')))
            || self.install_root == self.remote_home
        {
            return Err(invalid("installRoot", "must be inside remoteHome"));
        }
        if self.workspace_roots.is_empty()
            || self.workspace_roots.len() > 64
            || self
                .workspace_roots
                .iter()
                .any(|root| !root.starts_with('/'))
        {
            return Err(invalid("workspaceRoots", "must be absolute paths"));
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
        let mut profiles = Vec::new();
        for entry in entries {
            let Ok(profile) = serde_json::from_value::<WslConnectionProfile>(entry) else {
                continue;
            };
            if profile.validate().is_ok() {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    pub fn save(&self, profiles: &[WslConnectionProfile]) -> VibexResult<()> {
        for profile in profiles {
            profile.validate()?;
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
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, bytes).map_err(|_| {
            VibexError::storage(
                "wsl_profile_write_failed",
                "the WSL store could not be written",
            )
        })?;
        fs::rename(&temp, &self.path).map_err(|_| {
            VibexError::storage(
                "wsl_profile_rename_failed",
                "the WSL store could not be replaced",
            )
        })?;
        Ok(())
    }
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
    }

    #[test]
    fn rejects_unsafe_roots() {
        let mut profile = sample();
        profile.install_root = "/etc".to_string();
        assert!(profile.validate().is_err());
        profile.install_root = profile.remote_home.clone();
        assert!(profile.validate().is_err());
    }
}

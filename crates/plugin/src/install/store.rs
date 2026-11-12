//! The plugin store: version directories, atomic activation and recovery.
//!
//! ```text
//! <base>/plugins/<id>                 -> ../plugin-versions/<id>/<version>
//! <base>/plugin-versions/<id>/<ver>/  the unpacked package
//! <base>/plugin-data/<id>/            the plugin's own data
//! <base>/plugin-tmp/                  staging, removed after a publish
//! <base>/plugins/.pending-install.json
//! ```
//!
//! Activation is a link swap: the version directory is published first, then
//! the link moves to it, so an interrupted install leaves either the old
//! version or the new one, never a half-written one. The pending marker makes
//! the outcome recoverable on the next start.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::PluginError;

/// The merges and versions directory names.
pub const PLUGINS_DIR: &str = "plugins";
pub const VERSIONS_DIR: &str = "plugin-versions";
pub const DATA_DIR: &str = "plugin-data";
pub const STAGING_DIR: &str = "plugin-tmp";
pub const PENDING_FILE: &str = ".pending-install.json";
/// The directories the scripting host adds below the data home it is given.
///
/// The host is pointed at [`DATA_DIR`] and stores a plugin's `localStorage`
/// under `gpui-shell/plugins/<id>`; naming the suffix here keeps
/// [`PluginStore::host_data_dir`] honest without pulling the host crate into
/// this one.
pub const HOST_DATA_SUFFIX: &str = "gpui-shell/plugins";

/// The marker written before an install becomes visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInstall {
    pub plugin_id: String,
    pub previous_version: Option<String>,
    pub new_version: String,
    pub started_at_ms: u64,
}

/// What recovery did with a marker it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No marker was present.
    None,
    /// The install had completed; the marker was cleared.
    Completed { plugin_id: String, version: String },
    /// The install had not completed; the previous version was restored.
    RolledBack { plugin_id: String, version: String },
    /// Neither version was usable; the plugin is left deactivated.
    Broken { plugin_id: String },
}

/// The directory layout one Vibex installation owns.
#[derive(Debug, Clone)]
pub struct PluginStore {
    base: PathBuf,
}

impl PluginStore {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn plugins_root(&self) -> PathBuf {
        self.base.join(PLUGINS_DIR)
    }

    pub fn versions_root(&self) -> PathBuf {
        self.base.join(VERSIONS_DIR)
    }

    pub fn data_root(&self) -> PathBuf {
        self.base.join(DATA_DIR)
    }

    pub fn staging_root(&self) -> PathBuf {
        self.base.join(STAGING_DIR)
    }

    pub fn link_path(&self, plugin_id: &str) -> PathBuf {
        self.plugins_root().join(plugin_id)
    }

    pub fn current_link(&self, plugin_id: &str) -> PathBuf {
        self.plugins_root().join(plugin_id)
    }

    pub fn versions_dir(&self, plugin_id: &str) -> PathBuf {
        self.versions_root().join(plugin_id)
    }

    pub fn version_dir(&self, plugin_id: &str, version: &str) -> PathBuf {
        self.versions_dir(plugin_id).join(version)
    }

    pub fn data_dir(&self, plugin_id: &str) -> PathBuf {
        self.data_root().join(plugin_id)
    }

    /// Where the scripting host keeps a plugin's own storage.
    ///
    /// The host is given [`DATA_DIR`] as its data home and appends
    /// `gpui-shell/plugins/<id>`, so this is that path. It sits inside
    /// [`Self::data_dir`]'s parent, which is how uninstalling a plugin removes
    /// its `localStorage` with nothing left behind.
    pub fn host_data_dir(&self, plugin_id: &str) -> PathBuf {
        self.data_root().join(HOST_DATA_SUFFIX).join(plugin_id)
    }

    pub fn pending_path(&self) -> PathBuf {
        self.plugins_root().join(PENDING_FILE)
    }

    /// Creates every directory the store needs.
    pub fn ensure_layout(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.plugins_root())?;
        std::fs::create_dir_all(self.versions_root())?;
        std::fs::create_dir_all(self.data_root())?;
        std::fs::create_dir_all(self.staging_root())
    }

    /// Creates a fresh staging directory for one install.
    pub fn create_staging(&self, plugin_id: &str) -> std::io::Result<PathBuf> {
        self.ensure_layout()?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let directory = self.staging_root().join(format!("{plugin_id}-{unique}"));
        std::fs::create_dir_all(&directory)?;
        Ok(directory)
    }

    /// The version the plugin's link currently points at.
    pub fn current_version(&self, plugin_id: &str) -> Option<String> {
        let link = self.current_link(plugin_id);
        let target = std::fs::read_link(&link).ok()?;
        target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
    }

    /// Whether a version directory exists.
    pub fn has_version(&self, plugin_id: &str, version: &str) -> bool {
        self.version_dir(plugin_id, version).is_dir()
    }

    /// Publishes a staged directory as a version.
    ///
    /// Any previous directory for the same version is replaced; a failed rename
    /// leaves the old directory in place.
    pub fn publish_version(
        &self,
        plugin_id: &str,
        version: &str,
        staging: &Path,
    ) -> Result<PathBuf, PluginError> {
        self.ensure_layout()?;
        let versions = self.versions_dir(plugin_id);
        std::fs::create_dir_all(&versions)?;
        let destination = self.version_dir(plugin_id, version);
        if destination.exists() {
            std::fs::remove_dir_all(&destination)?;
        }
        std::fs::rename(staging, &destination)?;
        Ok(destination)
    }

    /// Points the plugin's link at a published version.
    pub fn activate(&self, plugin_id: &str, version: &str) -> Result<(), PluginError> {
        self.ensure_layout()?;
        let version_dir = self.version_dir(plugin_id, version);
        if !version_dir.is_dir() {
            return Err(PluginError::NotInstalled(format!("{plugin_id} {version}")));
        }
        let link = self.current_link(plugin_id);
        let relative = PathBuf::from("..")
            .join(VERSIONS_DIR)
            .join(plugin_id)
            .join(version);
        let temporary = link.with_extension("new");
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_dir(&temporary);
        create_directory_link(&relative, &temporary).map_err(|error| {
            PluginError::Io(std::io::Error::new(
                error.kind(),
                format!("the plugin link could not be created: {error}"),
            ))
        })?;
        // Renaming over the existing link is atomic on Unix.
        if let Err(error) = std::fs::rename(&temporary, &link) {
            let _ = remove_link(&temporary);
            return Err(PluginError::Io(error));
        }
        Ok(())
    }

    /// Removes the plugin's link, leaving its versions and data.
    pub fn deactivate(&self, plugin_id: &str) -> Result<(), PluginError> {
        remove_link(&self.current_link(plugin_id))?;
        Ok(())
    }

    /// Removes one published version.
    pub fn remove_version(&self, plugin_id: &str, version: &str) -> Result<(), PluginError> {
        let directory = self.version_dir(plugin_id, version);
        if directory.exists() {
            std::fs::remove_dir_all(&directory)?;
        }
        Ok(())
    }

    /// Removes every version not in `keep`.
    pub fn prune_versions(
        &self,
        plugin_id: &str,
        keep: &[String],
    ) -> Result<Vec<String>, PluginError> {
        let directory = self.versions_dir(plugin_id);
        let mut removed = Vec::new();
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return Ok(removed);
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || keep.contains(&name) {
                continue;
            }
            if entry.path().is_dir() {
                std::fs::remove_dir_all(entry.path())?;
                removed.push(name);
            }
        }
        Ok(removed)
    }

    /// Removes a plugin's own data directory and the host storage beside it.
    pub fn remove_data(&self, plugin_id: &str) -> Result<(), PluginError> {
        for directory in [self.data_dir(plugin_id), self.host_data_dir(plugin_id)] {
            if directory.exists() {
                std::fs::remove_dir_all(&directory)?;
            }
        }
        Ok(())
    }

    /// The bytes a plugin's data occupies, host storage included.
    pub fn data_size(&self, plugin_id: &str) -> u64 {
        directory_size(&self.data_dir(plugin_id)) + directory_size(&self.host_data_dir(plugin_id))
    }

    /// Writes the pending marker.
    pub fn write_pending(&self, pending: &PendingInstall) -> Result<(), PluginError> {
        self.ensure_layout()?;
        let bytes = serde_json::to_vec_pretty(pending)?;
        let path = self.pending_path();
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, &path)?;
        Ok(())
    }

    /// Clears the pending marker.
    pub fn clear_pending(&self) -> Result<(), PluginError> {
        let path = self.pending_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Reads the pending marker.
    pub fn read_pending(&self) -> Option<PendingInstall> {
        let bytes = std::fs::read(self.pending_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Resolves a marker left by an interrupted install.
    pub fn recover_pending(&self) -> Result<RecoveryOutcome, PluginError> {
        let Some(pending) = self.read_pending() else {
            return Ok(RecoveryOutcome::None);
        };
        let current = self.current_version(&pending.plugin_id);
        if current.as_deref() == Some(pending.new_version.as_str())
            && self.has_version(&pending.plugin_id, &pending.new_version)
        {
            self.clear_pending()?;
            return Ok(RecoveryOutcome::Completed {
                plugin_id: pending.plugin_id,
                version: pending.new_version,
            });
        }
        if let Some(previous) = pending.previous_version.as_deref()
            && self.has_version(&pending.plugin_id, previous)
        {
            let _ = self.remove_version(&pending.plugin_id, &pending.new_version);
            self.activate(&pending.plugin_id, previous)?;
            self.clear_pending()?;
            return Ok(RecoveryOutcome::RolledBack {
                plugin_id: pending.plugin_id,
                version: previous.to_string(),
            });
        }
        let _ = self.deactivate(&pending.plugin_id);
        let _ = self.remove_version(&pending.plugin_id, &pending.new_version);
        self.clear_pending()?;
        Ok(RecoveryOutcome::Broken {
            plugin_id: pending.plugin_id,
        })
    }
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += directory_size(&path);
        } else if let Ok(metadata) = entry.metadata() {
            total += metadata.len();
        }
    }
    total
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

fn remove_link(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                std::fs::remove_file(path)
            } else if metadata.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, PluginStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = PluginStore::new(directory.path());
        store.ensure_layout().unwrap();
        (directory, store)
    }

    fn publish(store: &PluginStore, id: &str, version: &str) {
        let staging = store.create_staging(id).unwrap();
        std::fs::write(staging.join("main.js"), version).unwrap();
        store.publish_version(id, version, &staging).unwrap();
    }

    #[test]
    fn publishing_and_activating_selects_a_version() {
        let (_directory, store) = store();
        publish(&store, "acme.widgets", "1.0.0");
        publish(&store, "acme.widgets", "1.1.0");
        store.activate("acme.widgets", "1.0.0").unwrap();
        assert_eq!(
            store.current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );
        store.activate("acme.widgets", "1.1.0").unwrap();
        assert_eq!(
            store.current_version("acme.widgets").as_deref(),
            Some("1.1.0")
        );
        assert!(store.has_version("acme.widgets", "1.0.0"));
    }

    #[test]
    fn pruning_keeps_only_the_named_versions() {
        let (_directory, store) = store();
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            publish(&store, "acme.widgets", version);
        }
        let removed = store
            .prune_versions("acme.widgets", &["1.1.0".to_string(), "1.2.0".to_string()])
            .unwrap();
        assert_eq!(removed, vec!["1.0.0"]);
        assert!(!store.has_version("acme.widgets", "1.0.0"));
        assert!(store.has_version("acme.widgets", "1.2.0"));
    }

    #[test]
    fn recovery_rolls_back_an_interrupted_upgrade() {
        let (_directory, store) = store();
        publish(&store, "acme.widgets", "1.0.0");
        store.activate("acme.widgets", "1.0.0").unwrap();
        // The new version was published but the link was never moved.
        publish(&store, "acme.widgets", "2.0.0");
        store
            .write_pending(&PendingInstall {
                plugin_id: "acme.widgets".to_string(),
                previous_version: Some("1.0.0".to_string()),
                new_version: "2.0.0".to_string(),
                started_at_ms: 1,
            })
            .unwrap();
        let outcome = store.recover_pending().unwrap();
        assert_eq!(
            outcome,
            RecoveryOutcome::RolledBack {
                plugin_id: "acme.widgets".to_string(),
                version: "1.0.0".to_string()
            }
        );
        assert_eq!(
            store.current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );
        assert!(store.read_pending().is_none());
    }

    #[test]
    fn recovery_completes_an_install_that_landed() {
        let (_directory, store) = store();
        publish(&store, "acme.widgets", "1.0.0");
        store.activate("acme.widgets", "1.0.0").unwrap();
        store
            .write_pending(&PendingInstall {
                plugin_id: "acme.widgets".to_string(),
                previous_version: None,
                new_version: "1.0.0".to_string(),
                started_at_ms: 1,
            })
            .unwrap();
        let outcome = store.recover_pending().unwrap();
        assert_eq!(
            outcome,
            RecoveryOutcome::Completed {
                plugin_id: "acme.widgets".to_string(),
                version: "1.0.0".to_string()
            }
        );
    }

    #[test]
    fn data_removal_is_separate_from_versions() {
        let (_directory, store) = store();
        publish(&store, "acme.widgets", "1.0.0");
        std::fs::create_dir_all(store.data_dir("acme.widgets")).unwrap();
        std::fs::write(store.data_dir("acme.widgets").join("store.json"), b"{}").unwrap();
        assert!(store.data_size("acme.widgets") > 0);
        store.remove_data("acme.widgets").unwrap();
        assert!(!store.data_dir("acme.widgets").exists());
        assert!(store.has_version("acme.widgets", "1.0.0"));
    }
}

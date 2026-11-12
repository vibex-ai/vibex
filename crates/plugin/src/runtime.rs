//! The seam between package management and the scripting host.
//!
//! The market and the installer never link a scripting engine. They hand a
//! loaded plugin to a [`PluginRuntime`], which the desktop implements over the
//! host it embeds. That keeps the host integration in one place and lets the
//! package pipeline be tested without an engine.

use std::path::PathBuf;

use crate::error::PluginError;

/// A plugin that has been published and is ready for the runtime to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPlugin {
    pub plugin_id: String,
    pub version: String,
    /// The version directory the link points at.
    pub directory: PathBuf,
    /// The plugin's own data directory.
    pub data_directory: PathBuf,
    /// The permission keys the user approved for this version.
    ///
    /// The host re-checks the generated manifest against this list before a
    /// line of script runs. The installer already enforced consent; this is
    /// the second layer, so a package swapped on disk after consent cannot
    /// widen what the host grants.
    pub granted_permissions: Vec<String>,
}

impl LoadedPlugin {
    pub fn new(
        plugin_id: impl Into<String>,
        version: impl Into<String>,
        directory: impl Into<PathBuf>,
        data_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            version: version.into(),
            directory: directory.into(),
            data_directory: data_directory.into(),
            granted_permissions: Vec::new(),
        }
    }

    pub fn with_granted_permissions(
        mut self,
        permissions: impl IntoIterator<Item = String>,
    ) -> Self {
        self.granted_permissions = permissions.into_iter().collect();
        self
    }
}

/// Loads and unloads script plugins.
///
/// Implementations run on the thread that owns the host; `unload` must cancel
/// the plugin's work before it returns, so an upgrade never leaves the old
/// version's tasks running against the new one.
pub trait PluginRuntime: Send + Sync {
    fn load(&self, plugin: &LoadedPlugin) -> Result<(), PluginError>;
    fn unload(&self, plugin_id: &str) -> Result<(), PluginError>;
}

/// A runtime that accepts everything and loads nothing.
///
/// Used where the scripting host is not part of the build, and in tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullRuntime;

impl PluginRuntime for NullRuntime {
    fn load(&self, _plugin: &LoadedPlugin) -> Result<(), PluginError> {
        Ok(())
    }

    fn unload(&self, _plugin_id: &str) -> Result<(), PluginError> {
        Ok(())
    }
}

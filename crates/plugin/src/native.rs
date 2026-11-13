//! First-party plugins that ship with the application.
//!
//! The right rail's panels are native plugins: Rust views compiled into the
//! app, versioned with it, and discovered through the same catalog as script
//! plugins. They cannot be installed, updated or removed; they can be enabled,
//! disabled and reordered, and they contribute the right-rail activities the
//! rail is built from.

use vibex_core::{
    InstalledPlugin, InstalledPluginStatus, PluginCategory, PluginContribution, PluginKind,
    PluginListingSource, PluginMarketEntry, PluginPermission, PluginPublisher, PluginSource,
    PluginTrust,
};

/// The contribution kind a right-rail activity is registered under.
pub const RIGHT_RAIL_ACTIVITY: &str = "right_rail_activity";
/// The contribution kind a workspace panel is registered under.
pub const PANEL: &str = "panel";
/// The contribution kind a command is registered under.
pub const COMMAND: &str = "command";
/// The contribution kind a settings page is registered under.
pub const SETTINGS: &str = "settings";

/// One contribution a native plugin makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeContribution {
    pub kind: &'static str,
    pub contribution_id: &'static str,
    pub title: &'static str,
    /// Ids earlier releases stored for this contribution.
    pub legacy_ids: &'static [&'static str],
}

/// One first-party plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePluginDescriptor {
    pub plugin_id: &'static str,
    pub builtin_key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub categories: &'static [PluginCategory],
    pub contributions: &'static [NativeContribution],
    pub enabled_by_default: bool,
}

impl NativePluginDescriptor {
    /// The right-rail activity this descriptor provides, if any.
    pub fn right_rail_activity(&self) -> Option<&'static NativeContribution> {
        self.contributions
            .iter()
            .find(|contribution| contribution.kind == RIGHT_RAIL_ACTIVITY)
    }

    /// The durable record a first-party plugin is seeded as.
    pub fn to_record(&self, app_version: &str, now_ms: i64) -> vibex_db::PluginRecord {
        let mut record =
            vibex_db::PluginRecord::new(self.plugin_id, self.name, app_version, now_ms);
        record.kind = PluginKind::Native;
        record.source = PluginSource::Bundled;
        record.bundled = true;
        record.uninstallable = false;
        record.builtin_key = Some(self.builtin_key.to_string());
        record.publisher_id = Some("vibex".to_string());
        record.publisher_name = Some("Vibex".to_string());
        record.publisher_verified = true;
        record.trust = PluginTrust::Official;
        record.enabled = self.enabled_by_default;
        record.status = if self.enabled_by_default {
            InstalledPluginStatus::Enabled
        } else {
            InstalledPluginStatus::Disabled
        };
        record.permissions = self.permissions();
        record
    }

    /// The permissions a native plugin holds.
    ///
    /// Native plugins are first-party code with the host's own access; the
    /// detail page states that rather than listing granular grants.
    pub fn permissions(&self) -> Vec<PluginPermission> {
        Vec::new()
    }

    /// The listing projection for the market.
    pub fn to_entry(&self, app_version: &str) -> PluginMarketEntry {
        PluginMarketEntry {
            id: self.plugin_id.to_string(),
            name: self.name.to_string(),
            kind: PluginKind::Native,
            bundled: true,
            listing_source: PluginListingSource::Bundled,
            summary: Some(self.description.to_string()),
            icon_url: None,
            publisher: PluginPublisher::vibex(),
            categories: self.categories.to_vec(),
            tags: Vec::new(),
            latest_version: Some(app_version.to_string()),
            installs: 0,
            rating: None,
            updated_at: None,
            license: Some("AGPL-3.0-or-later".to_string()),
            homepage: Some("https://github.com/vibex-ai/vibex".to_string()),
            repository: Some("https://github.com/vibex-ai/vibex".to_string()),
            trust: PluginTrust::Official,
            permissions: self.permissions(),
            compatibility: Default::default(),
            size_bytes: None,
            featured: false,
            i18n: Default::default(),
        }
    }

    /// The display projection for an installed native plugin.
    pub fn to_installed(&self, app_version: &str, enabled: bool) -> InstalledPlugin {
        InstalledPlugin {
            plugin_id: self.plugin_id.to_string(),
            name: self.name.to_string(),
            version: app_version.to_string(),
            kind: PluginKind::Native,
            source: PluginSource::Bundled,
            bundled: true,
            uninstallable: false,
            builtin_key: Some(self.builtin_key.to_string()),
            publisher: PluginPublisher::vibex(),
            trust: PluginTrust::Official,
            categories: self.categories.to_vec(),
            enabled,
            auto_update: false,
            pinned_version: None,
            rollback_version: None,
            status: if enabled {
                InstalledPluginStatus::Enabled
            } else {
                InstalledPluginStatus::Disabled
            },
            install_path: None,
            data_path: None,
            permissions: self.permissions(),
            granted_permissions: Vec::new(),
            consent_fingerprint: None,
            installed_at: None,
            updated_at: None,
            last_checked_at: None,
            last_error: None,
            size_bytes: None,
            update: None,
        }
    }

    /// The contribution rows this plugin owns.
    pub fn to_contributions(&self) -> Vec<PluginContribution> {
        self.contributions
            .iter()
            .map(|contribution| PluginContribution {
                plugin_id: self.plugin_id.to_string(),
                kind: contribution.kind.to_string(),
                contribution_id: contribution.contribution_id.to_string(),
                title: Some(contribution.title.to_string()),
                enabled: self.enabled_by_default,
            })
            .collect()
    }
}

/// The first-party plugins every build ships with.
pub const BUILTIN_NATIVE_PLUGINS: &[NativePluginDescriptor] = &[
    NativePluginDescriptor {
        plugin_id: "vibex.system.editor",
        builtin_key: "vibex.system.editor",
        name: "Editor",
        description: "The workspace editor, opened beside a session.",
        categories: &[PluginCategory::Panels, PluginCategory::DeveloperTools],
        contributions: &[NativeContribution {
            kind: RIGHT_RAIL_ACTIVITY,
            contribution_id: "rail_activity_editor",
            title: "Editor",
            legacy_ids: &["rail_activity_editor"],
        }],
        enabled_by_default: true,
    },
    NativePluginDescriptor {
        plugin_id: "vibex.system.files",
        builtin_key: "vibex.system.files",
        name: "Files",
        description: "Browse and open the workspace's files.",
        categories: &[PluginCategory::Panels, PluginCategory::Viewers],
        contributions: &[NativeContribution {
            kind: RIGHT_RAIL_ACTIVITY,
            contribution_id: "rail_plugin_system_files",
            title: "Files",
            legacy_ids: &["rail_plugin_system_files"],
        }],
        enabled_by_default: true,
    },
    NativePluginDescriptor {
        plugin_id: "vibex.system.git",
        builtin_key: "vibex.system.git",
        name: "Git",
        description: "Review changes, stage them and commit.",
        categories: &[PluginCategory::Panels, PluginCategory::DeveloperTools],
        contributions: &[NativeContribution {
            kind: RIGHT_RAIL_ACTIVITY,
            contribution_id: "rail_plugin_system_git",
            title: "Git",
            legacy_ids: &["rail_plugin_system_git"],
        }],
        enabled_by_default: true,
    },
    NativePluginDescriptor {
        plugin_id: "vibex.system.child-agents",
        builtin_key: "vibex.system.child-agents",
        name: "Child Agents",
        description: "Follow the agents a session delegated work to.",
        categories: &[PluginCategory::Panels, PluginCategory::Agents],
        contributions: &[NativeContribution {
            kind: RIGHT_RAIL_ACTIVITY,
            contribution_id: "rail_activity_child_agents",
            title: "Child Agents",
            legacy_ids: &["rail_activity_child_agents"],
        }],
        enabled_by_default: true,
    },
    NativePluginDescriptor {
        plugin_id: "vibex.system.web-panel",
        builtin_key: "vibex.system.web-panel",
        name: "Web Panel",
        description: "A configurable web view in the right rail.",
        categories: &[PluginCategory::Panels, PluginCategory::Integrations],
        contributions: &[NativeContribution {
            kind: RIGHT_RAIL_ACTIVITY,
            contribution_id: "rail_plugin_custom_web",
            title: "Web Panel",
            legacy_ids: &["rail_plugin_custom_web"],
        }],
        enabled_by_default: false,
    },
];

/// The first-party plugin registry.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativePluginRegistry;

impl NativePluginRegistry {
    /// The registry every build uses.
    pub fn builtin() -> Self {
        Self
    }

    pub fn descriptors(&self) -> &'static [NativePluginDescriptor] {
        BUILTIN_NATIVE_PLUGINS
    }

    pub fn get(&self, plugin_id: &str) -> Option<&'static NativePluginDescriptor> {
        BUILTIN_NATIVE_PLUGINS
            .iter()
            .find(|descriptor| descriptor.plugin_id == plugin_id)
    }

    /// The registry entry that owns an id earlier releases stored.
    pub fn from_legacy_activity_id(
        &self,
        activity_id: &str,
    ) -> Option<(&'static NativePluginDescriptor, &'static NativeContribution)> {
        for descriptor in BUILTIN_NATIVE_PLUGINS {
            for contribution in descriptor.contributions {
                if contribution.legacy_ids.contains(&activity_id)
                    || contribution.contribution_id == activity_id
                {
                    return Some((descriptor, contribution));
                }
            }
        }
        None
    }

    /// The stable id a stored activity id resolves to.
    pub fn canonical_activity_id(&self, activity_id: &str) -> Option<&'static str> {
        self.from_legacy_activity_id(activity_id)
            .map(|(_, contribution)| contribution.contribution_id)
    }

    /// Every first-party plugin's market listing.
    pub fn market_entries(&self, app_version: &str) -> Vec<PluginMarketEntry> {
        BUILTIN_NATIVE_PLUGINS
            .iter()
            .map(|descriptor| descriptor.to_entry(app_version))
            .collect()
    }
}

/// The default registry.
pub fn default_native_plugins() -> NativePluginRegistry {
    NativePluginRegistry::builtin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_native_plugin_has_a_right_rail_activity() {
        for descriptor in BUILTIN_NATIVE_PLUGINS {
            assert!(
                descriptor.right_rail_activity().is_some(),
                "{} has no right-rail activity",
                descriptor.plugin_id
            );
        }
    }

    #[test]
    fn builtin_keys_and_ids_are_unique() {
        let mut ids = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for descriptor in BUILTIN_NATIVE_PLUGINS {
            assert!(ids.insert(descriptor.plugin_id));
            assert!(keys.insert(descriptor.builtin_key));
        }
    }

    #[test]
    fn legacy_activity_ids_resolve() {
        let registry = NativePluginRegistry::builtin();
        assert_eq!(
            registry.canonical_activity_id("rail_plugin_system_git"),
            Some("rail_plugin_system_git")
        );
        // The canonical id itself resolves too.
        assert_eq!(
            registry.canonical_activity_id("rail_activity_child_agents"),
            Some("rail_activity_child_agents")
        );
        assert!(registry.canonical_activity_id("nope").is_none());
    }

    #[test]
    fn a_native_plugin_is_bundled_and_not_uninstallable() {
        let descriptor = NativePluginRegistry::builtin()
            .get("vibex.system.files")
            .unwrap();
        let installed = descriptor.to_installed("0.1.2", true);
        assert!(installed.bundled);
        assert!(!installed.can_uninstall());
        assert_eq!(installed.trust, PluginTrust::Official);
        assert_eq!(installed.kind, PluginKind::Native);

        let record = descriptor.to_record("0.1.2", 1);
        assert_eq!(record.source, PluginSource::Bundled);
        assert!(!record.uninstallable);
        assert_eq!(record.builtin_key.as_deref(), Some("vibex.system.files"));
        assert_eq!(record.to_installed().status, InstalledPluginStatus::Enabled);
    }
}

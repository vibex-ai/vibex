//! The permission catalog a plugin's capabilities are read as.
//!
//! Every permission has a stable key, a risk tier the consent sheet groups by,
//! and a scope kind. The catalog is the single place that maps a manifest's
//! declared capabilities onto the permissions the user is asked to grant; a key
//! the catalog does not know is never granted.

use vibex_core::{PluginPermission, PluginPermissionRisk};

use crate::manifest::PluginCapabilities;

/// What a permission's scope names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionScopeKind {
    /// The permission has no scope.
    None,
    /// The scope is a set of network hosts.
    Host,
    /// The scope is a set of filesystem roots.
    Path,
}

/// One permission the catalog knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionSpec {
    pub key: &'static str,
    pub risk: PluginPermissionRisk,
    pub scope_kind: PermissionScopeKind,
    pub summary: &'static str,
}

/// The permissions a build of Vibex recognizes.
#[derive(Debug, Clone)]
pub struct PluginPermissionCatalog {
    specs: Vec<PermissionSpec>,
}

impl Default for PluginPermissionCatalog {
    fn default() -> Self {
        Self::builtin()
    }
}

impl PluginPermissionCatalog {
    /// The catalog every build ships with.
    pub fn builtin() -> Self {
        Self {
            specs: vec![
                PermissionSpec {
                    key: "ui.panel",
                    risk: PluginPermissionRisk::Low,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Add a panel to the workspace",
                },
                PermissionSpec {
                    key: "ui.command",
                    risk: PluginPermissionRisk::Low,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Add commands to the command palette",
                },
                PermissionSpec {
                    key: "storage",
                    risk: PluginPermissionRisk::Low,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Keep its own data between runs",
                },
                PermissionSpec {
                    key: "clipboard",
                    risk: PluginPermissionRisk::Medium,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Read and write the system clipboard",
                },
                PermissionSpec {
                    key: "net.fetch",
                    risk: PluginPermissionRisk::Medium,
                    scope_kind: PermissionScopeKind::Host,
                    summary: "Make network requests",
                },
                PermissionSpec {
                    key: "agent.read",
                    risk: PluginPermissionRisk::Medium,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Read agent sessions",
                },
                PermissionSpec {
                    key: "agent.send",
                    risk: PluginPermissionRisk::High,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Send messages to agents",
                },
                PermissionSpec {
                    key: "agent.tool",
                    risk: PluginPermissionRisk::High,
                    scope_kind: PermissionScopeKind::None,
                    summary: "Expose tools to agents",
                },
                PermissionSpec {
                    key: "fs.read",
                    risk: PluginPermissionRisk::High,
                    scope_kind: PermissionScopeKind::Path,
                    summary: "Read files",
                },
                PermissionSpec {
                    key: "fs.write",
                    risk: PluginPermissionRisk::High,
                    scope_kind: PermissionScopeKind::Path,
                    summary: "Write files",
                },
                PermissionSpec {
                    key: "process.execute",
                    risk: PluginPermissionRisk::High,
                    scope_kind: PermissionScopeKind::Path,
                    summary: "Run programs",
                },
            ],
        }
    }

    pub fn specs(&self) -> &[PermissionSpec] {
        &self.specs
    }

    pub fn get(&self, key: &str) -> Option<&PermissionSpec> {
        self.specs.iter().find(|spec| spec.key == key)
    }

    /// Whether the catalog knows a key at all.
    pub fn knows(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The risk a key carries; an unknown key is treated as the highest.
    pub fn risk(&self, key: &str) -> PluginPermissionRisk {
        self.get(key)
            .map(|spec| spec.risk)
            .unwrap_or(PluginPermissionRisk::High)
    }

    /// The permissions a capability declaration asks for.
    pub fn permissions_for(&self, capabilities: &PluginCapabilities) -> Vec<PluginPermission> {
        let mut permissions = Vec::new();
        let mut push = |key: &str, scope: Option<String>| {
            let spec = self.get(key).copied().unwrap_or(PermissionSpec {
                key: "unknown",
                risk: PluginPermissionRisk::High,
                scope_kind: PermissionScopeKind::None,
                summary: "Unknown permission",
            });
            permissions.push(PluginPermission {
                key: spec.key.to_string(),
                scope,
                risk: spec.risk,
                reason: None,
            });
        };

        if capabilities.panel {
            push("ui.panel", None);
        }
        if capabilities.command {
            push("ui.command", None);
        }
        if capabilities.storage {
            push("storage", None);
        }
        if capabilities.clipboard {
            push("clipboard", None);
        }
        if let Some(network) = &capabilities.network {
            push("net.fetch", Some(network.hosts.join(", ")));
        }
        if let Some(fs) = &capabilities.fs {
            if !fs.read.is_empty() {
                push("fs.read", Some(fs.read.join(", ")));
            }
            if !fs.write.is_empty() {
                push("fs.write", Some(fs.write.join(", ")));
            }
        }
        if let Some(agent) = &capabilities.agent {
            if agent.read {
                push("agent.read", None);
            }
            if agent.send {
                push("agent.send", None);
            }
            if agent.tool {
                push("agent.tool", None);
            }
        }

        permissions.sort_by(|left, right| left.key.cmp(&right.key));
        permissions.dedup_by(|left, right| left.key == right.key);
        permissions
    }
}

/// The catalog every caller shares.
pub fn default_permission_catalog() -> PluginPermissionCatalog {
    PluginPermissionCatalog::builtin()
}

/// Reads a capability declaration as permissions.
pub fn permissions_for(capabilities: &PluginCapabilities) -> Vec<PluginPermission> {
    default_permission_catalog().permissions_for(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{PluginFsCapability, PluginNetworkCapability};

    #[test]
    fn capabilities_become_sorted_permissions() {
        let capabilities = PluginCapabilities {
            panel: true,
            storage: true,
            clipboard: true,
            network: Some(PluginNetworkCapability {
                hosts: vec!["api.example.com".to_string()],
                http: Vec::new(),
            }),
            fs: Some(PluginFsCapability {
                read: vec!["${workspace}".to_string()],
                write: Vec::new(),
            }),
            ..Default::default()
        };
        let permissions = permissions_for(&capabilities);
        let keys: Vec<&str> = permissions.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["clipboard", "fs.read", "net.fetch", "storage", "ui.panel"]
        );
        let fs = permissions.iter().find(|p| p.key == "fs.read").unwrap();
        assert_eq!(fs.scope.as_deref(), Some("${workspace}"));
        assert_eq!(fs.risk, PluginPermissionRisk::High);
    }

    #[test]
    fn an_empty_declaration_asks_for_nothing() {
        assert!(permissions_for(&PluginCapabilities::default()).is_empty());
    }

    #[test]
    fn unknown_keys_are_highest_risk() {
        let catalog = PluginPermissionCatalog::builtin();
        assert_eq!(catalog.risk("not.a.key"), PluginPermissionRisk::High);
        assert!(!catalog.knows("process.execute.hidden"));
    }
}

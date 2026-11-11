//! The scripting host's manifest, generated from the author's manifest.
//!
//! The host's format is fixed and rejects fields it does not know, so authors
//! never write it: Vibex translates `vibex-plugin.json` into `gpui-shell.json`
//! at development and install time. The host resolves `${pluginDir}` and
//! `${dataDir}` itself; host variables such as `${workspace}` are resolved here
//! before the manifest is written.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::manifest::PluginManifest;

/// The manifest file the scripting host reads.
pub const SHELL_MANIFEST_FILE: &str = "gpui-shell.json";

/// The host version a plugin gets when its manifest names none.
pub const DEFAULT_SHELL_VERSION: &str = "0.9.0";

/// The host's manifest format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ShellManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub shell_version: String,
    pub entry: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ShellCapabilities>,
}

/// The host capability document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<ShellFsCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<ShellNetworkCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<ShellClipboardCapability>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellFsCapability {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellNetworkCapability {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellClipboardCapability {
    #[serde(default)]
    pub write: bool,
}

/// Where the host variables a manifest may name resolve to.
#[derive(Debug, Clone, Default)]
pub struct ShellManifestContext {
    /// The directory the package is installed in (`${pluginDir}`).
    pub plugin_dir: Option<std::path::PathBuf>,
    /// The directory the plugin may write (`${dataDir}`).
    pub data_dir: Option<std::path::PathBuf>,
    /// The active workspace root (`${workspace}`).
    pub workspace_root: Option<std::path::PathBuf>,
    /// The user's home directory (`${homeDir}`).
    pub home_dir: Option<std::path::PathBuf>,
}

/// The result of translating an author manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltShellManifest {
    pub manifest: ShellManifest,
    /// Host scopes that could not be resolved and were left out.
    pub dropped_scopes: Vec<String>,
}

/// Translates an author manifest into the host's manifest.
///
/// The mapping is deliberately one-way and closed: a plugin can never ask for
/// process execution or host exit, and its own resources are always readable.
pub fn build_shell_manifest(
    manifest: &PluginManifest,
    context: &ShellManifestContext,
) -> Result<BuiltShellManifest, ManifestError> {
    let mut dropped_scopes = Vec::new();
    let mut fs = ShellFsCapability::default();
    if context.plugin_dir.is_some() {
        fs.read.push("${pluginDir}".to_string());
    }
    if context.data_dir.is_some() {
        fs.write.push("${dataDir}".to_string());
    }

    let mut storage = None;
    let mut clipboard = None;

    if manifest.capabilities.storage {
        storage = Some(true);
    }
    if manifest.capabilities.clipboard {
        clipboard = Some(ShellClipboardCapability { write: true });
    }
    if let Some(declared) = &manifest.capabilities.fs {
        for root in &declared.read {
            match resolve_host_variable(root, context) {
                Some(resolved) => push_unique(&mut fs.read, resolved),
                None => dropped_scopes.push(root.clone()),
            }
        }
        for root in &declared.write {
            match resolve_host_variable(root, context) {
                Some(resolved) => push_unique(&mut fs.write, resolved),
                None => dropped_scopes.push(root.clone()),
            }
        }
    }

    let network = manifest
        .capabilities
        .network
        .as_ref()
        .filter(|network| !network.hosts.is_empty())
        .map(|network| ShellNetworkCapability {
            hosts: network.hosts.clone(),
        });

    let capabilities = if fs.read.is_empty()
        && fs.write.is_empty()
        && network.is_none()
        && storage.is_none()
        && clipboard.is_none()
    {
        None
    } else {
        Some(ShellCapabilities {
            fs: (!fs.read.is_empty() || !fs.write.is_empty()).then_some(fs),
            network,
            storage,
            clipboard,
        })
    };

    let shell_version = manifest
        .shell_version
        .clone()
        .unwrap_or_else(|| DEFAULT_SHELL_VERSION.to_string());

    Ok(BuiltShellManifest {
        manifest: ShellManifest {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            shell_version,
            entry: manifest.entry.clone(),
            dependencies: BTreeMap::new(),
            capabilities,
        },
        dropped_scopes,
    })
}

/// Writes the host manifest into a plugin directory.
pub fn write_shell_manifest(
    directory: &Path,
    manifest: &PluginManifest,
    context: &ShellManifestContext,
) -> std::io::Result<BuiltShellManifest> {
    let built = build_shell_manifest(manifest, context)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
    let bytes = serde_json::to_vec_pretty(&built.manifest).map_err(std::io::Error::other)?;
    std::fs::write(directory.join(SHELL_MANIFEST_FILE), bytes)?;
    Ok(built)
}

fn resolve_host_variable(value: &str, context: &ShellManifestContext) -> Option<String> {
    if value.contains("${pluginDir}") {
        return Some(value.to_string());
    }
    if value.contains("${dataDir}") {
        return Some(value.to_string());
    }
    let mut resolved = value.to_string();
    let mut replaced = false;
    if resolved.contains("${workspace}") {
        let root = context.workspace_root.as_ref()?;
        resolved = resolved.replace("${workspace}", &root.to_string_lossy());
        replaced = true;
    }
    if resolved.contains("${homeDir}") {
        let home = context.home_dir.as_ref()?;
        resolved = resolved.replace("${homeDir}", &home.to_string_lossy());
        replaced = true;
    }
    if resolved.contains("${") {
        return None;
    }
    if !replaced {
        return Some(value.to_string());
    }
    Some(resolved)
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(capabilities: serde_json::Value) -> PluginManifest {
        let value = serde_json::json!({
            "id": "acme.widgets",
            "name": "Widgets",
            "version": "1.0.0",
            "license": "MIT",
            "entry": "main.js",
            "capabilities": capabilities
        });
        PluginManifest::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn context() -> ShellManifestContext {
        ShellManifestContext {
            plugin_dir: Some("/plugins/acme.widgets".into()),
            data_dir: Some("/plugin-data/acme.widgets".into()),
            workspace_root: Some("/work".into()),
            home_dir: Some("/home/user".into()),
        }
    }

    #[test]
    fn a_plugin_can_always_read_itself_and_write_its_data() {
        let manifest = manifest(serde_json::json!({}));
        let built = build_shell_manifest(&manifest, &context()).unwrap();
        let fs = built.manifest.capabilities.unwrap().fs.unwrap();
        assert_eq!(fs.read, vec!["${pluginDir}"]);
        assert_eq!(fs.write, vec!["${dataDir}"]);
    }

    #[test]
    fn host_variables_are_resolved_and_unknown_ones_dropped() {
        let manifest = manifest(serde_json::json!({
            "fs": { "read": ["${workspace}/src", "${unknown}/x"] }
        }));
        let built = build_shell_manifest(&manifest, &context()).unwrap();
        let fs = built.manifest.capabilities.unwrap().fs.unwrap();
        assert!(fs.read.contains(&"/work/src".to_string()));
        assert!(built.dropped_scopes.contains(&"${unknown}/x".to_string()));
    }

    #[test]
    fn process_execution_is_never_granted() {
        let manifest = manifest(serde_json::json!({ "panel": true }));
        let built = build_shell_manifest(&manifest, &context()).unwrap();
        let json = serde_json::to_string(&built.manifest).unwrap();
        assert!(!json.contains("execute"));
        assert!(!json.contains("exit"));
    }

    #[test]
    fn the_shell_manifest_uses_the_hosts_field_names() {
        let manifest = manifest(serde_json::json!({ "storage": true, "clipboard": true }));
        let built = build_shell_manifest(&manifest, &context()).unwrap();
        let json = serde_json::to_value(&built.manifest).unwrap();
        assert!(json.get("shell-version").is_some());
        assert!(json.get("shell_version").is_none());
        assert_eq!(json["capabilities"]["storage"], true);
        assert_eq!(json["capabilities"]["clipboard"]["write"], true);
    }

    #[test]
    fn network_hosts_pass_through() {
        let manifest = manifest(serde_json::json!({
            "network": { "hosts": ["api.example.com"] }
        }));
        let built = build_shell_manifest(&manifest, &context()).unwrap();
        let network = built.manifest.capabilities.unwrap().network.unwrap();
        assert_eq!(network.hosts, vec!["api.example.com"]);
    }
}

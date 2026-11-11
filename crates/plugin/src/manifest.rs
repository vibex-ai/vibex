//! The author's manifest (`vibex-plugin.json`) and its validation.
//!
//! A plugin author writes one manifest. The scripting host's own manifest is
//! generated from it (see [`crate::shell`]) so the host's seven-field,
//! unknown-field-rejecting format never has to be the author's problem.

use std::collections::BTreeSet;
use std::path::Path;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use vibex_core::{PluginCategory, PluginPermission};

use crate::error::ManifestError;

/// The file every plugin package carries.
pub const AUTHOR_MANIFEST_FILE: &str = "vibex-plugin.json";

/// The manifest schema this build understands.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// The fields the top-level manifest may carry.
const KNOWN_FIELDS: &[&str] = &[
    "schemaVersion",
    "id",
    "name",
    "version",
    "description",
    "license",
    "vibex",
    "shellVersion",
    "entry",
    "icon",
    "categories",
    "capabilities",
    "contributes",
    "repository",
    "homepage",
];

/// A plugin's declaration of what it may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCapabilities {
    /// Add a panel to the workspace.
    #[serde(default)]
    pub panel: bool,
    /// Add commands to the command palette.
    #[serde(default)]
    pub command: bool,
    /// Keep data between runs.
    #[serde(default)]
    pub storage: bool,
    /// Read and write the system clipboard.
    #[serde(default)]
    pub clipboard: bool,
    /// Network access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<PluginNetworkCapability>,
    /// Filesystem access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<PluginFsCapability>,
    /// Agent access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<PluginAgentCapability>,
}

/// The hosts and HTTP rules a plugin may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginNetworkCapability {
    /// Host patterns: `*`, `*.example.com`, or a host.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Narrower HTTP rules.
    #[serde(default)]
    pub http: Vec<PluginHttpGrant>,
}

/// One narrowed HTTP rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHttpGrant {
    pub host: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub schemes: Vec<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub path_prefixes: Vec<String>,
}

/// The filesystem roots a plugin may read and write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFsCapability {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

/// The agent capabilities a plugin may use.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginAgentCapability {
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub send: bool,
    #[serde(default)]
    pub tool: bool,
}

/// What a plugin adds to the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginContributions {
    #[serde(default)]
    pub commands: Vec<PluginCommandContribution>,
    #[serde(default)]
    pub settings: Vec<PluginSettingContribution>,
}

/// One command a plugin registers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCommandContribution {
    pub id: String,
    pub title: String,
}

/// One setting a plugin declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSettingContribution {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The manifest a plugin author writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub license: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vibex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_version: Option<String>,
    pub entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub categories: Vec<PluginCategory>,
    #[serde(default)]
    pub capabilities: PluginCapabilities,
    #[serde(default)]
    pub contributes: PluginContributions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

const fn default_schema_version() -> u32 {
    MANIFEST_SCHEMA_VERSION
}

impl PluginManifest {
    /// Parses and validates a manifest with the built-in license policy.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        Self::parse_with_policy(bytes, &LicensePolicy::builtin())
    }

    /// Parses and validates a manifest under a caller's license policy.
    pub fn parse_with_policy(bytes: &[u8], policy: &LicensePolicy) -> Result<Self, ManifestError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        let object = value
            .as_object()
            .ok_or_else(|| ManifestError::Json("the manifest must be a JSON object".to_string()))?;
        for key in object.keys() {
            if !KNOWN_FIELDS.contains(&key.as_str()) {
                return Err(ManifestError::UnknownField(key.clone()));
            }
        }
        let manifest: Self = serde_json::from_value(value)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        manifest.validate(policy)?;
        Ok(manifest)
    }

    /// Reads a manifest from disk.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(path).map_err(|error| ManifestError::Json(error.to_string()))?;
        Self::parse(&bytes)
    }

    /// Checks every field the market relies on.
    pub fn validate(&self, policy: &LicensePolicy) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema(self.schema_version));
        }
        validate_plugin_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(ManifestError::EmptyName);
        }
        if self.name.chars().count() > 128 {
            return Err(ManifestError::NameTooLong);
        }
        Version::parse(&self.version)
            .map_err(|_| ManifestError::InvalidVersion(self.version.clone()))?;
        if !is_safe_relative_path(&self.entry) {
            return Err(ManifestError::InvalidEntry(self.entry.clone()));
        }
        if let Some(icon) = &self.icon
            && !is_safe_relative_path(icon)
        {
            return Err(ManifestError::InvalidIcon(icon.clone()));
        }
        if !policy.allows(&self.license) {
            return Err(ManifestError::LicenseNotAllowed(self.license.clone()));
        }
        if let Some(requirement) = &self.vibex {
            VersionReq::parse(requirement)
                .map_err(|_| ManifestError::InvalidRequirement(requirement.clone()))?;
        }
        if let Some(requirement) = &self.shell_version {
            VersionReq::parse(requirement)
                .map_err(|_| ManifestError::InvalidRequirement(requirement.clone()))?;
        }
        if self.categories.len() > PluginCategory::ALL.len() {
            return Err(ManifestError::TooManyCategories);
        }
        let mut seen = BTreeSet::new();
        for category in &self.categories {
            seen.insert(category.key());
        }
        for command in &self.contributes.commands {
            if command.id.trim().is_empty() || command.title.trim().is_empty() {
                return Err(ManifestError::Json(
                    "a contributed command needs an id and a title".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// The permissions this manifest asks for.
    pub fn permissions(&self) -> Vec<PluginPermission> {
        crate::permission::permissions_for(&self.capabilities)
    }

    /// Whether the manifest's `vibex` requirement admits a version.
    pub fn supports_vibex(&self, version: &Version) -> bool {
        match &self.vibex {
            Some(requirement) => VersionReq::parse(requirement)
                .map(|requirement| requirement.matches(version))
                .unwrap_or(false),
            None => true,
        }
    }

    /// The categories, de-duplicated, in the order the catalog lists them.
    pub fn normalized_categories(&self) -> Vec<PluginCategory> {
        let mut seen = BTreeSet::new();
        let mut categories = Vec::new();
        for category in &self.categories {
            if seen.insert(category.key()) {
                categories.push(*category);
            }
        }
        categories
    }
}

/// Validates a market plugin id.
///
/// The id is `<publisher>.<name>`: lowercase letters, digits, `.`, `-`, `_`,
/// never starting or ending with a separator and never containing `..`.
pub fn validate_plugin_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() || id.len() > 128 || !id.contains('.') || id.contains("..") {
        return Err(ManifestError::InvalidId(id.to_string()));
    }
    let first = id.chars().next().unwrap_or(' ');
    let last = id.chars().last().unwrap_or(' ');
    for separator in ['.', '-', '_'] {
        if first == separator || last == separator {
            return Err(ManifestError::InvalidId(id.to_string()));
        }
    }
    if !id.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '.' | '-' | '_')
    }) {
        return Err(ManifestError::InvalidId(id.to_string()));
    }
    Ok(())
}

/// Whether a path is a plain relative path inside the plugin directory.
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 512 {
        return false;
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    if path.contains('\\') || path.contains('\0') {
        return false;
    }
    let mut depth = 0i32;
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    // Windows drive letters and UNC prefixes must not sneak in.
    !path.contains(':')
}

/// Which licenses the desktop may install.
#[derive(Debug, Clone)]
pub struct LicensePolicy {
    allowed: BTreeSet<String>,
}

impl Default for LicensePolicy {
    fn default() -> Self {
        Self::builtin()
    }
}

impl LicensePolicy {
    /// The SPDX identifiers the desktop ships with.
    pub fn builtin() -> Self {
        let allowed = [
            "MIT",
            "Apache-2.0",
            "BSD-2-Clause",
            "BSD-3-Clause",
            "ISC",
            "MPL-2.0",
            "0BSD",
            "Unlicense",
            "Zlib",
            "CC0-1.0",
            "LGPL-2.1-only",
            "LGPL-2.1-or-later",
            "LGPL-3.0-only",
            "LGPL-3.0-or-later",
            "GPL-2.0-only",
            "GPL-2.0-or-later",
            "GPL-3.0-only",
            "GPL-3.0-or-later",
            "AGPL-3.0-only",
            "AGPL-3.0-or-later",
        ];
        Self {
            allowed: allowed.into_iter().map(str::to_string).collect(),
        }
    }

    /// A policy that allows any non-empty identifier.
    pub fn allow_all() -> Self {
        Self {
            allowed: BTreeSet::new(),
        }
    }

    pub fn allows(&self, license: &str) -> bool {
        if self.allowed.is_empty() {
            return !license.trim().is_empty();
        }
        self.allowed.contains(license.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "id": "acme.widgets",
            "name": "Widgets",
            "version": "1.2.0",
            "license": "MIT",
            "entry": "main.js"
        })
    }

    fn parse(value: serde_json::Value) -> Result<PluginManifest, ManifestError> {
        PluginManifest::parse(&serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn a_minimal_manifest_parses() {
        let manifest = parse(minimal()).unwrap();
        assert_eq!(manifest.id, "acme.widgets");
        assert_eq!(manifest.schema_version, 1);
        assert!(manifest.categories.is_empty());
        assert!(manifest.permissions().is_empty());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = minimal();
        value["surprise"] = serde_json::json!(true);
        assert_eq!(
            parse(value).unwrap_err(),
            ManifestError::UnknownField("surprise".to_string())
        );
    }

    #[test]
    fn bad_ids_are_rejected() {
        for id in [
            "Widgets",
            "acme",
            "acme..widgets",
            ".widgets",
            "acme-widgets-",
        ] {
            let mut value = minimal();
            value["id"] = serde_json::json!(id);
            assert!(
                matches!(parse(value), Err(ManifestError::InvalidId(_))),
                "{id} should be invalid"
            );
        }
        assert!(validate_plugin_id("acme.widgets").is_ok());
        assert!(validate_plugin_id("acme.my-widget_2").is_ok());
    }

    #[test]
    fn entry_must_stay_inside_the_package() {
        for entry in ["../../etc/passwd", "/etc/passwd", "a/../../b", "C:evil.js"] {
            let mut value = minimal();
            value["entry"] = serde_json::json!(entry);
            assert!(
                matches!(parse(value), Err(ManifestError::InvalidEntry(_))),
                "{entry} should be invalid"
            );
        }
        let mut value = minimal();
        value["entry"] = serde_json::json!("ui/main.js");
        assert!(parse(value).is_ok());
    }

    #[test]
    fn the_license_policy_is_enforced() {
        let mut value = minimal();
        value["license"] = serde_json::json!("WTFPL");
        assert!(matches!(
            parse(value),
            Err(ManifestError::LicenseNotAllowed(_))
        ));
    }

    #[test]
    fn a_bad_requirement_is_rejected() {
        let mut value = minimal();
        value["vibex"] = serde_json::json!("not a requirement");
        assert!(matches!(
            parse(value),
            Err(ManifestError::InvalidRequirement(_))
        ));
    }

    #[test]
    fn capabilities_become_permissions() {
        let mut value = minimal();
        value["capabilities"] = serde_json::json!({
            "panel": true,
            "network": { "hosts": ["api.example.com"] },
            "fs": { "read": ["${workspace}"] }
        });
        let manifest = parse(value).unwrap();
        let permissions = manifest.permissions();
        let keys: Vec<&str> = permissions
            .iter()
            .map(|permission| permission.key.as_str())
            .collect();
        assert_eq!(keys, vec!["fs.read", "net.fetch", "ui.panel"]);
    }

    #[test]
    fn categories_are_normalized() {
        let mut value = minimal();
        value["categories"] = serde_json::json!(["panels", "panels", "themes"]);
        let manifest = parse(value).unwrap();
        assert_eq!(
            manifest.normalized_categories(),
            vec![PluginCategory::Panels, PluginCategory::Themes]
        );
    }

    #[test]
    fn version_requirements_admit_versions() {
        let mut value = minimal();
        value["vibex"] = serde_json::json!(">=0.1.0");
        let manifest = parse(value).unwrap();
        assert!(manifest.supports_vibex(&Version::parse("0.1.2").unwrap()));
        assert!(!manifest.supports_vibex(&Version::parse("0.0.9").unwrap()));
    }
}

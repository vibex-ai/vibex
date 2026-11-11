//! The consent fingerprint.
//!
//! A fingerprint summarizes everything the user agreed to: the plugin identity,
//! its version, the permissions it was granted and how it executes. Any change
//! to any of them produces a different fingerprint, which is what makes an
//! update that adds a permission ask again instead of silently widening what a
//! plugin may do.

use serde::Serialize;
use sha2::{Digest, Sha256};
use vibex_core::PluginPermission;

use crate::manifest::PluginManifest;

/// The execution model a script plugin runs under.
pub const SCRIPT_EXECUTION_MODEL: &str = "script-v1";

/// The execution model a native plugin runs under.
pub const NATIVE_EXECUTION_MODEL: &str = "native-v1";

#[derive(Serialize)]
struct FingerprintInput<'a> {
    id: &'a str,
    version: &'a str,
    #[serde(rename = "executionModel")]
    execution_model: &'a str,
    permissions: Vec<FingerprintPermission<'a>>,
}

#[derive(Serialize)]
struct FingerprintPermission<'a> {
    key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
}

/// The fingerprint of what installing this manifest would ask for.
pub fn consent_fingerprint(manifest: &PluginManifest) -> String {
    consent_fingerprint_for(
        &manifest.id,
        &manifest.version,
        SCRIPT_EXECUTION_MODEL,
        &manifest.permissions(),
    )
}

/// The fingerprint of an arbitrary identity, version and permission set.
pub fn consent_fingerprint_for(
    id: &str,
    version: &str,
    execution_model: &str,
    permissions: &[PluginPermission],
) -> String {
    let mut permissions: Vec<FingerprintPermission<'_>> = permissions
        .iter()
        .map(|permission| FingerprintPermission {
            key: permission.key.as_str(),
            scope: permission.scope.as_deref(),
        })
        .collect();
    permissions.sort_by(|left, right| {
        left.key
            .cmp(right.key)
            .then_with(|| left.scope.cmp(&right.scope))
    });
    permissions.dedup_by(|left, right| left.key == right.key && left.scope == right.scope);

    let input = FingerprintInput {
        id,
        version,
        execution_model,
        permissions,
    };
    let bytes = serde_json::to_vec(&input).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    hex_lower(&digest)
}

/// Lowercase hexadecimal.
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::PluginPermissionRisk;

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

    #[test]
    fn the_fingerprint_is_stable_and_sensitive() {
        let base = manifest(serde_json::json!({ "panel": true }));
        let same = manifest(serde_json::json!({ "panel": true }));
        assert_eq!(consent_fingerprint(&base), consent_fingerprint(&same));

        let more = manifest(serde_json::json!({ "panel": true, "clipboard": true }));
        assert_ne!(consent_fingerprint(&base), consent_fingerprint(&more));
    }

    #[test]
    fn permission_order_does_not_change_the_fingerprint() {
        let a = vec![
            PluginPermission::new("ui.panel", PluginPermissionRisk::Low),
            PluginPermission::new("storage", PluginPermissionRisk::Low),
        ];
        let b = vec![
            PluginPermission::new("storage", PluginPermissionRisk::Low),
            PluginPermission::new("ui.panel", PluginPermissionRisk::Low),
        ];
        assert_eq!(
            consent_fingerprint_for("acme.widgets", "1.0.0", SCRIPT_EXECUTION_MODEL, &a),
            consent_fingerprint_for("acme.widgets", "1.0.0", SCRIPT_EXECUTION_MODEL, &b),
        );
    }

    #[test]
    fn the_version_is_part_of_the_fingerprint() {
        let a = consent_fingerprint_for("acme.widgets", "1.0.0", SCRIPT_EXECUTION_MODEL, &[]);
        let b = consent_fingerprint_for("acme.widgets", "1.0.1", SCRIPT_EXECUTION_MODEL, &[]);
        assert_ne!(a, b);
    }
}

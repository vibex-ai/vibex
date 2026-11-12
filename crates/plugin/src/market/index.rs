//! The signed static index and its revocation list.
//!
//! The index is the install authority: every reviewed plugin, its versions and
//! their package hashes are covered by one detached Ed25519 signature made with
//! the key embedded in the application. Search results are display data; an
//! install reads the version it needs from the verified index.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use url::Url;
use vibex_core::{PluginMarketEntry, PluginMarketVersion};

use crate::error::PluginError;
use crate::manifest::validate_plugin_id;

/// The index schema this build understands.
pub const INDEX_SCHEMA: u32 = 1;
/// The revocations schema this build understands.
pub const REVOCATIONS_SCHEMA: u32 = 1;
/// The largest index the market will read.
pub const MAX_INDEX_BYTES: u64 = vibex_core::MAX_PLUGIN_MARKET_RESPONSE_BYTES;
/// The largest revocation list the market will read.
pub const MAX_REVOCATIONS_BYTES: u64 = 1024 * 1024;

/// One publisher key the index vouches for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublisherKey {
    pub publisher_id: String,
    pub key_id: String,
    pub public_key: String,
}

/// One plugin entry in the index: the listing plus its versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginIndexEntry {
    pub entry: PluginMarketEntry,
    #[serde(default)]
    pub versions: Vec<PluginMarketVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_key_id: Option<String>,
}

/// The signed index document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginIndexFile {
    pub schema: u32,
    pub generated_at: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub publisher_keys: Vec<PublisherKey>,
    #[serde(default)]
    pub plugins: Vec<PluginIndexEntry>,
    #[serde(default)]
    pub revocations_revision: u64,
}

/// One blocked plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokedPlugin {
    pub id: String,
    #[serde(default)]
    pub versions: Vec<String>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_url: Option<String>,
}

impl RevokedPlugin {
    /// Whether a version is blocked. An empty version list blocks every version.
    pub fn blocks(&self, version: &str) -> bool {
        self.versions.is_empty() || self.versions.iter().any(|blocked| blocked == version)
    }
}

/// The signed revocation list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginRevocations {
    pub schema: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub blocked_plugins: Vec<RevokedPlugin>,
}

impl PluginRevocations {
    /// The revocation blocking this plugin version, if any.
    pub fn blocking(&self, id: &str, version: &str) -> Option<&RevokedPlugin> {
        self.blocked_plugins
            .iter()
            .find(|entry| entry.id == id && entry.blocks(version))
    }
}

impl PluginIndexFile {
    /// Parses and validates an index.
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginError> {
        let index: Self = serde_json::from_slice(bytes)?;
        index.validate()?;
        Ok(index)
    }

    /// Checks the fields the installer relies on.
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.schema != INDEX_SCHEMA {
            return Err(PluginError::Network(format!(
                "the index schema {} is not supported",
                self.schema
            )));
        }
        if self.generated_at.trim().is_empty() {
            return Err(PluginError::Network(
                "the index has no generation time".to_string(),
            ));
        }
        for plugin in &self.plugins {
            validate_plugin_id(&plugin.entry.id)?;
            for version in &plugin.versions {
                validate_version(version)?;
            }
        }
        Ok(())
    }

    /// The index entries keyed by plugin id.
    pub fn entry(&self, id: &str) -> Option<&PluginIndexEntry> {
        self.plugins.iter().find(|plugin| plugin.entry.id == id)
    }

    pub fn entries(&self) -> &[PluginIndexEntry] {
        &self.plugins
    }

    /// The publisher key the index vouches for, by key id.
    pub fn publisher_key(&self, key_id: &str) -> Option<&PublisherKey> {
        self.publisher_keys.iter().find(|key| key.key_id == key_id)
    }
}

impl PluginRevocations {
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginError> {
        let revocations: Self = serde_json::from_slice(bytes)?;
        if revocations.schema != REVOCATIONS_SCHEMA {
            return Err(PluginError::Network(format!(
                "the revocation schema {} is not supported",
                revocations.schema
            )));
        }
        Ok(revocations)
    }
}

/// Validates one version entry.
pub fn validate_version(version: &PluginMarketVersion) -> Result<(), PluginError> {
    semver::Version::parse(&version.version).map_err(|_| {
        PluginError::Manifest(crate::error::ManifestError::InvalidVersion(
            version.version.clone(),
        ))
    })?;
    if !is_sha256(&version.sha256) {
        return Err(PluginError::Network(format!(
            "the version `{}` has no valid sha256",
            version.version
        )));
    }
    if let Some(sha512) = &version.sha512
        && !is_hex(sha512, 128)
    {
        return Err(PluginError::Network(format!(
            "the version `{}` has a malformed sha512",
            version.version
        )));
    }
    validate_version_url(&version.url)
}

fn validate_version_url(url: &str) -> Result<(), PluginError> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("github:") {
        let repo = &url["github:".len()..];
        if repo.is_empty() || repo.contains("..") || repo.matches('/').count() != 1 {
            return Err(PluginError::Network(format!(
                "the package address `{url}` is not a repository spec"
            )));
        }
        return Ok(());
    }
    let parsed = Url::parse(url)
        .map_err(|_| PluginError::Network(format!("the package address `{url}` is not a URL")))?;
    if parsed.scheme() != "https" {
        return Err(PluginError::Network(format!(
            "the package address `{url}` is not https"
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    is_hex(value, 64)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.chars().all(|character| character.is_ascii_hexdigit())
}

/// Verifies a detached signature over a market document.
pub fn verify_signature(
    raw: &[u8],
    signature_base64: &str,
    public_key_base64: &str,
) -> Result<(), PluginError> {
    let key = decode_verifying_key(public_key_base64)?;
    let signature = decode_signature(signature_base64)?;
    key.verify_strict(raw, &signature)
        .map_err(|_| PluginError::IndexSignatureInvalid)
}

/// Verifies a publisher's detached signature over a package.
///
/// Unlike the index signature, a publisher signature is about who built the
/// package, not about the catalog's integrity; a failure is reported as a
/// package-signature failure so the two are never confused.
pub fn verify_package_signature(
    public_key_base64: &str,
    signature_base64: &str,
    package: &[u8],
) -> Result<(), PluginError> {
    let key = decode_verifying_key(public_key_base64).map_err(|_| PluginError::SignatureInvalid)?;
    let signature =
        decode_signature(signature_base64).map_err(|_| PluginError::SignatureInvalid)?;
    key.verify_strict(package, &signature)
        .map_err(|_| PluginError::SignatureInvalid)
}

/// Signs a package with a publisher's release key.
pub fn sign_package(package: &[u8], signing_key: &SigningKey) -> String {
    sign_document(package, signing_key)
}

/// Decodes a base64 Ed25519 public key.
pub fn decode_verifying_key(public_key_base64: &str) -> Result<VerifyingKey, PluginError> {
    let bytes = BASE64
        .decode(public_key_base64.trim())
        .map_err(|_| PluginError::IndexSignatureInvalid)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| PluginError::IndexSignatureInvalid)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| PluginError::IndexSignatureInvalid)
}

fn decode_signature(signature_base64: &str) -> Result<Signature, PluginError> {
    let bytes = BASE64
        .decode(signature_base64.trim())
        .map_err(|_| PluginError::IndexSignatureInvalid)?;
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| PluginError::IndexSignatureInvalid)?;
    Ok(Signature::from_bytes(&bytes))
}

/// Signs a market document with a release key (used by the index tooling).
pub fn sign_document(raw: &[u8], signing_key: &SigningKey) -> String {
    let signature = signing_key.sign(raw);
    BASE64.encode(signature.to_bytes())
}

/// Encodes a verifying key as the base64 form the client embeds.
pub fn encode_verifying_key(key: &VerifyingKey) -> String {
    BASE64.encode(key.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{PluginListingSource, PluginPublisher, PluginTrust};

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn entry(id: &str) -> PluginIndexEntry {
        PluginIndexEntry {
            entry: PluginMarketEntry {
                id: id.to_string(),
                name: "Widgets".to_string(),
                kind: Default::default(),
                bundled: false,
                listing_source: PluginListingSource::Index,
                summary: None,
                icon_url: None,
                publisher: PluginPublisher::from_login("acme"),
                categories: Vec::new(),
                tags: Vec::new(),
                latest_version: Some("1.0.0".to_string()),
                installs: 0,
                rating: None,
                updated_at: None,
                license: Some("MIT".to_string()),
                homepage: None,
                repository: None,
                trust: PluginTrust::Community,
                permissions: Vec::new(),
                compatibility: Default::default(),
                size_bytes: None,
                featured: false,
                i18n: Default::default(),
            },
            versions: vec![PluginMarketVersion {
                version: "1.0.0".to_string(),
                published_at: None,
                size_bytes: Some(10),
                sha256: "a".repeat(64),
                sha512: None,
                url: "https://example.com/widgets.vibexplug".to_string(),
                changelog: None,
                min_vibex: None,
                min_shell: None,
                permissions: Vec::new(),
                platforms: Vec::new(),
                yanked: false,
                yanked_reason: None,
                signature: None,
                provenance: None,
                review: None,
            }],
            publisher_key_id: None,
        }
    }

    fn index() -> PluginIndexFile {
        PluginIndexFile {
            schema: INDEX_SCHEMA,
            generated_at: "2026-11-11T00:00:00Z".to_string(),
            revision: 3,
            publisher_keys: Vec::new(),
            plugins: vec![entry("acme.widgets")],
            revocations_revision: 1,
        }
    }

    #[test]
    fn an_index_round_trips_and_validates() {
        let bytes = serde_json::to_vec(&index()).unwrap();
        let parsed = PluginIndexFile::parse(&bytes).unwrap();
        assert_eq!(parsed.revision, 3);
        assert!(parsed.entry("acme.widgets").is_some());
        assert!(parsed.entry("missing.plugin").is_none());
    }

    #[test]
    fn an_unsupported_schema_is_rejected() {
        let mut index = index();
        index.schema = 99;
        let bytes = serde_json::to_vec(&index).unwrap();
        assert!(PluginIndexFile::parse(&bytes).is_err());
    }

    #[test]
    fn a_bad_hash_is_rejected() {
        let mut index = index();
        index.plugins[0].versions[0].sha256 = "nope".to_string();
        let bytes = serde_json::to_vec(&index).unwrap();
        assert!(PluginIndexFile::parse(&bytes).is_err());
    }

    #[test]
    fn a_non_https_package_address_is_rejected() {
        let mut index = index();
        index.plugins[0].versions[0].url = "http://example.com/x".to_string();
        let bytes = serde_json::to_vec(&index).unwrap();
        assert!(PluginIndexFile::parse(&bytes).is_err());
    }

    #[test]
    fn a_github_spec_is_accepted() {
        let mut index = index();
        index.plugins[0].versions[0].url = "github:acme/widgets".to_string();
        let bytes = serde_json::to_vec(&index).unwrap();
        assert!(PluginIndexFile::parse(&bytes).is_ok());
    }

    #[test]
    fn a_signature_verifies_and_tampering_fails() {
        let key = signing_key();
        let bytes = serde_json::to_vec(&index()).unwrap();
        let signature = sign_document(&bytes, &key);
        let public = encode_verifying_key(&key.verifying_key());
        assert!(verify_signature(&bytes, &signature, &public).is_ok());

        let mut tampered = bytes.clone();
        tampered.push(b' ');
        assert!(verify_signature(&tampered, &signature, &public).is_err());
    }

    #[test]
    fn revocations_block_ids_and_versions() {
        let revocations = PluginRevocations {
            schema: REVOCATIONS_SCHEMA,
            revision: 2,
            blocked_plugins: vec![
                RevokedPlugin {
                    id: "acme.widgets".to_string(),
                    versions: vec!["1.0.0".to_string()],
                    reason: "unsafe".to_string(),
                    advisory_url: None,
                },
                RevokedPlugin {
                    id: "acme.broken".to_string(),
                    versions: Vec::new(),
                    reason: "abandoned".to_string(),
                    advisory_url: None,
                },
            ],
        };
        assert!(revocations.blocking("acme.widgets", "1.0.0").is_some());
        assert!(revocations.blocking("acme.widgets", "1.0.1").is_none());
        assert!(revocations.blocking("acme.broken", "9.9.9").is_some());
    }
}

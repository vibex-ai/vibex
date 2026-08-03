//! Immutable process launch identity and stale classification.
//!
//! The process fingerprint is intentionally assembled field-by-field.  Do not
//! replace this with a serialized API/config DTO hash: session-scoped values
//! and secret material must never become process identity by accident.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest, Sha256};
use vibex_core::{
    AcpAdapterId, AgentId, NativeStateHomeId, ProviderProfileId, VibexError, VibexResult,
};

const FINGERPRINT_DOMAIN: &[u8] = b"vibex/acp/process-spawn-fingerprint/v1";

/// Immutable configuration required before an ACP process can be spawned.
///
/// Model, mode, reasoning effort and other session-scoped values are
/// deliberately absent. `secret_reference_versions` contains only opaque
/// reference/version material supplied by the config owner, never resolved
/// secret values.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessSpawnConfigSnapshot {
    pub agent_id: AgentId,
    pub adapter_id: AcpAdapterId,
    pub adapter_version: String,
    pub adapter_binary_identity: String,
    pub provider_profile_id: ProviderProfileId,
    /// A deterministic revision of the process-scoped projection. It is not a
    /// wall-clock save timestamp, so an equivalent profile can recover the
    /// launch fingerprint after a save/revert cycle.
    pub profile_revision: i64,
    pub command: String,
    pub args: Vec<String>,
    pub cwd_policy: String,
    pub base_url: Option<String>,
    pub model_provider_id: Option<String>,
    pub non_secret_env: BTreeMap<String, String>,
    pub secret_reference_versions: BTreeMap<String, String>,
    pub mcp_revision: Option<String>,
    pub skills_revision: Option<String>,
    pub native_state_home_id: NativeStateHomeId,
}

impl fmt::Debug for ProcessSpawnConfigSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let non_secret_env_keys = self.non_secret_env.keys().collect::<Vec<_>>();
        let secret_reference_keys = self.secret_reference_versions.keys().collect::<Vec<_>>();
        formatter
            .debug_struct("ProcessSpawnConfigSnapshot")
            .field("agent_id", &self.agent_id)
            .field("adapter_id", &self.adapter_id)
            .field("adapter_version", &self.adapter_version)
            .field("adapter_binary_identity", &self.adapter_binary_identity)
            .field("provider_profile_id", &self.provider_profile_id)
            .field("profile_revision", &self.profile_revision)
            .field("command", &redact_text(&self.command))
            .field("args", &format_args!("{} args", self.args.len()))
            .field("cwd_policy", &redact_text(&self.cwd_policy))
            .field("base_url", &self.base_url.as_ref().map(|_| "[redacted]"))
            .field("model_provider_id", &self.model_provider_id)
            .field("non_secret_env_keys", &non_secret_env_keys)
            .field("secret_reference_keys", &secret_reference_keys)
            .field("mcp_revision", &self.mcp_revision)
            .field("skills_revision", &self.skills_revision)
            .field("native_state_home_id", &self.native_state_home_id)
            .finish()
    }
}

impl ProcessSpawnConfigSnapshot {
    /// Sets the deterministic process-scoped content revision.
    pub fn with_content_revision(mut self) -> Self {
        self.profile_revision = self.content_revision();
        self
    }

    /// Returns the fingerprint used by `ProcessAcquireKey` and stale checks.
    pub fn process_spawn_fingerprint(&self) -> String {
        let bytes = self.canonical_bytes(true);
        format!("sha256:{}", hex_digest(Sha256::digest(bytes).as_slice()))
    }

    /// Computes a stable revision from the process-effective projection,
    /// excluding the revision field itself.
    pub fn content_revision(&self) -> i64 {
        let digest = Sha256::digest(self.canonical_bytes(false));
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        // Keep the value positive for persistence/UI consumers. The full
        // fingerprint remains the collision-resistant identity.
        let revision = i64::from_be_bytes(bytes) & i64::MAX;
        if revision == 0 { 1 } else { revision }
    }

    /// Computes which process-effective fields differ.
    pub fn diff(&self, current: &Self) -> BTreeSet<ProcessConfigField> {
        let mut changed = BTreeSet::new();
        if self.agent_id != current.agent_id {
            changed.insert(ProcessConfigField::Agent);
        }
        if self.adapter_id != current.adapter_id {
            changed.insert(ProcessConfigField::Adapter);
        }
        if self.adapter_version != current.adapter_version {
            changed.insert(ProcessConfigField::AdapterVersion);
        }
        if self.adapter_binary_identity != current.adapter_binary_identity {
            changed.insert(ProcessConfigField::AdapterBinaryIdentity);
        }
        if self.provider_profile_id != current.provider_profile_id {
            changed.insert(ProcessConfigField::ProviderProfile);
        }
        if self.command != current.command {
            changed.insert(ProcessConfigField::Command);
        }
        if self.args != current.args {
            changed.insert(ProcessConfigField::Args);
        }
        if self.cwd_policy != current.cwd_policy {
            changed.insert(ProcessConfigField::CwdPolicy);
        }
        if self.base_url != current.base_url {
            changed.insert(ProcessConfigField::BaseUrl);
        }
        if self.model_provider_id != current.model_provider_id {
            changed.insert(ProcessConfigField::ModelProvider);
        }
        if self.non_secret_env != current.non_secret_env {
            changed.insert(ProcessConfigField::NonSecretEnv);
        }
        if self.secret_reference_versions != current.secret_reference_versions {
            changed.insert(ProcessConfigField::SecretReferences);
        }
        if self.mcp_revision != current.mcp_revision {
            changed.insert(ProcessConfigField::McpRevision);
        }
        if self.skills_revision != current.skills_revision {
            changed.insert(ProcessConfigField::SkillsRevision);
        }
        if self.native_state_home_id != current.native_state_home_id {
            changed.insert(ProcessConfigField::NativeStateHome);
        }
        changed
    }

    fn canonical_bytes(&self, include_revision: bool) -> Vec<u8> {
        let mut output = Vec::new();
        write_component(&mut output, FINGERPRINT_DOMAIN);
        write_required_field(&mut output, b"agent_id", self.agent_id.as_str().as_bytes());
        write_required_field(
            &mut output,
            b"adapter_id",
            self.adapter_id.as_str().as_bytes(),
        );
        write_required_field(
            &mut output,
            b"adapter_version",
            self.adapter_version.as_bytes(),
        );
        write_required_field(
            &mut output,
            b"adapter_binary_identity",
            self.adapter_binary_identity.as_bytes(),
        );
        write_required_field(
            &mut output,
            b"provider_profile_id",
            self.provider_profile_id.as_str().as_bytes(),
        );
        if include_revision {
            write_required_field(
                &mut output,
                b"profile_revision",
                &self.profile_revision.to_be_bytes(),
            );
        }
        write_required_field(&mut output, b"command", self.command.as_bytes());
        write_string_vec_field(&mut output, b"args", &self.args);
        write_required_field(&mut output, b"cwd_policy", self.cwd_policy.as_bytes());
        write_optional_field(&mut output, b"base_url", self.base_url.as_deref());
        write_optional_field(
            &mut output,
            b"model_provider_id",
            self.model_provider_id.as_deref(),
        );
        write_map_field(&mut output, b"non_secret_env", &self.non_secret_env);
        write_map_field(
            &mut output,
            b"secret_reference_versions",
            &self.secret_reference_versions,
        );
        write_optional_field(&mut output, b"mcp_revision", self.mcp_revision.as_deref());
        write_optional_field(
            &mut output,
            b"skills_revision",
            self.skills_revision.as_deref(),
        );
        write_required_field(
            &mut output,
            b"native_state_home_id",
            self.native_state_home_id.as_str().as_bytes(),
        );
        output
    }
}

/// Fields that can make a running process stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessConfigField {
    Agent,
    Adapter,
    AdapterVersion,
    AdapterBinaryIdentity,
    ProviderProfile,
    Command,
    Args,
    CwdPolicy,
    BaseUrl,
    ModelProvider,
    NonSecretEnv,
    SecretReferences,
    McpRevision,
    SkillsRevision,
    NativeStateHome,
}

impl ProcessConfigField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Adapter => "adapter",
            Self::AdapterVersion => "adapter_version",
            Self::AdapterBinaryIdentity => "adapter_binary_identity",
            Self::ProviderProfile => "provider_profile",
            Self::Command => "command",
            Self::Args => "args",
            Self::CwdPolicy => "cwd_policy",
            Self::BaseUrl => "base_url",
            Self::ModelProvider => "model_provider",
            Self::NonSecretEnv => "non_secret_env",
            Self::SecretReferences => "secret_references",
            Self::McpRevision => "mcp_revision",
            Self::SkillsRevision => "skills_revision",
            Self::NativeStateHome => "native_state_home",
        }
    }
}

/// Process-level stale status. Live mutation is represented here but is only
/// selected when a later negotiated capability explicitly permits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessConfigStatus {
    Current,
    StaleLiveMutationAvailable,
    StaleRestartRequired,
    PreparingReplacement,
    ReplacementFailed,
}

impl ProcessConfigStatus {
    pub fn from_diff(
        changed: &BTreeSet<ProcessConfigField>,
        live_mutation_fields: &BTreeSet<ProcessConfigField>,
    ) -> Self {
        if changed.is_empty() {
            return Self::Current;
        }
        if changed
            .iter()
            .all(|field| live_mutation_fields.contains(field))
        {
            Self::StaleLiveMutationAvailable
        } else {
            Self::StaleRestartRequired
        }
    }
}

/// Bounded event emitted when a process status changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessConfigStatusEvent {
    pub process_instance_id: String,
    pub provider_profile_id: ProviderProfileId,
    pub previous_status: ProcessConfigStatus,
    pub status: ProcessConfigStatus,
    pub previous_fingerprint: String,
    pub current_fingerprint: String,
    pub changed_fields: Vec<String>,
}

fn write_component(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn write_required_field(output: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    write_component(output, name);
    output.push(1);
    write_component(output, value);
}

fn write_optional_field(output: &mut Vec<u8>, name: &[u8], value: Option<&str>) {
    write_component(output, name);
    match value {
        Some(value) => {
            output.push(1);
            write_component(output, value.as_bytes());
        }
        None => output.push(0),
    }
}

fn write_string_vec_field(output: &mut Vec<u8>, name: &[u8], values: &[String]) {
    write_component(output, name);
    output.push(1);
    output.extend_from_slice(&(values.len() as u64).to_be_bytes());
    for value in values {
        write_component(output, value.as_bytes());
    }
}

fn write_map_field(output: &mut Vec<u8>, name: &[u8], values: &BTreeMap<String, String>) {
    write_component(output, name);
    output.push(1);
    output.extend_from_slice(&(values.len() as u64).to_be_bytes());
    for (key, value) in values {
        write_component(output, key.as_bytes());
        write_component(output, value.as_bytes());
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn redact_text(value: &str) -> String {
    let Some((end, _)) = value.char_indices().nth(80) else {
        return value.to_string();
    };
    format!("{}...", &value[..end])
}

/// Builds a safe version token for a secret reference without resolving it.
pub fn secret_reference_version(
    lookup_key: &str,
    backend: &str,
    updated_at_ms: i64,
) -> VibexResult<String> {
    if lookup_key.trim().is_empty() || backend.trim().is_empty() || updated_at_ms < 0 {
        return Err(VibexError::validation(
            "acp_secret_reference_version_invalid",
            "ACP secret reference version metadata is invalid",
        ));
    }
    let mut canonical = Vec::new();
    write_component(&mut canonical, b"vibex/acp/secret-reference/v1");
    write_required_field(&mut canonical, b"lookup_key", lookup_key.as_bytes());
    write_required_field(&mut canonical, b"backend", backend.as_bytes());
    write_required_field(
        &mut canonical,
        b"updated_at_ms",
        &updated_at_ms.to_be_bytes(),
    );
    Ok(format!(
        "ref:sha256:{}",
        hex_digest(Sha256::digest(canonical).as_slice())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AcpAdapterId, AgentId, NativeStateHomeId, ProviderProfileId};

    fn snapshot() -> ProcessSpawnConfigSnapshot {
        ProcessSpawnConfigSnapshot {
            agent_id: AgentId::parse("codex").unwrap(),
            adapter_id: AcpAdapterId::parse("codex-acp").unwrap(),
            adapter_version: "1.1.9".to_string(),
            adapter_binary_identity: "sha256:adapter".to_string(),
            provider_profile_id: ProviderProfileId::parse("provider_codex").unwrap(),
            profile_revision: 0,
            command: "node".to_string(),
            args: vec!["adapter.js".to_string(), "--stdio".to_string()],
            cwd_policy: "{workspaceRoot}".to_string(),
            base_url: Some("https://api.example.test".to_string()),
            model_provider_id: Some("openai".to_string()),
            non_secret_env: BTreeMap::from([(String::from("LANG"), String::from("C"))]),
            secret_reference_versions: BTreeMap::from([(
                String::from("OPENAI_API_KEY"),
                String::from("ref:sha256:abc"),
            )]),
            mcp_revision: Some("mcp:1".to_string()),
            skills_revision: Some("skills:1".to_string()),
            native_state_home_id: NativeStateHomeId::parse("statehome_codex").unwrap(),
        }
        .with_content_revision()
    }

    #[test]
    fn fingerprint_is_deterministic_and_excludes_revision_time() {
        let first = snapshot();
        let mut equivalent = first.clone();
        equivalent.profile_revision = 42;
        equivalent = equivalent.with_content_revision();
        assert_eq!(
            first.process_spawn_fingerprint(),
            equivalent.process_spawn_fingerprint()
        );
        assert_eq!(first.profile_revision, equivalent.profile_revision);
    }

    #[test]
    fn map_order_does_not_change_fingerprint() {
        let first = snapshot();
        let mut second = first.clone();
        second.non_secret_env = BTreeMap::from([
            (String::from("LANG"), String::from("C")),
            (String::from("TERM"), String::from("xterm")),
        ]);
        let mut third = first.clone();
        third.non_secret_env = BTreeMap::from([
            (String::from("TERM"), String::from("xterm")),
            (String::from("LANG"), String::from("C")),
        ]);
        assert_eq!(
            second.process_spawn_fingerprint(),
            third.process_spawn_fingerprint()
        );
        assert_ne!(
            first.process_spawn_fingerprint(),
            second.process_spawn_fingerprint()
        );
    }

    #[test]
    fn every_process_scoped_field_changes_the_fingerprint() {
        let first = snapshot();
        let mut variants = Vec::new();

        let mut value = first.clone();
        value.agent_id = AgentId::parse("claude").unwrap();
        variants.push(value);
        let mut value = first.clone();
        value.adapter_id = AcpAdapterId::parse("other-acp").unwrap();
        variants.push(value);
        let mut value = first.clone();
        value.adapter_version.push_str("-next");
        variants.push(value);
        let mut value = first.clone();
        value.adapter_binary_identity.push_str("-next");
        variants.push(value);
        let mut value = first.clone();
        value.provider_profile_id = ProviderProfileId::parse("provider_other").unwrap();
        variants.push(value);
        let mut value = first.clone();
        value.command.push_str("-next");
        variants.push(value);
        let mut value = first.clone();
        value.args.reverse();
        variants.push(value);
        let mut value = first.clone();
        value.cwd_policy.push_str("/nested");
        variants.push(value);
        let mut value = first.clone();
        value.base_url = None;
        variants.push(value);
        let mut value = first.clone();
        value.model_provider_id = None;
        variants.push(value);
        let mut value = first.clone();
        value
            .non_secret_env
            .insert("TERM".to_string(), "xterm".to_string());
        variants.push(value);
        let mut value = first.clone();
        value
            .secret_reference_versions
            .insert("TOKEN".to_string(), "ref:sha256:next".to_string());
        variants.push(value);
        let mut value = first.clone();
        value.mcp_revision = None;
        variants.push(value);
        let mut value = first.clone();
        value.skills_revision = None;
        variants.push(value);
        let mut value = first.clone();
        value.native_state_home_id = NativeStateHomeId::parse("statehome_other").unwrap();
        variants.push(value);

        for variant in variants {
            assert_ne!(
                first.process_spawn_fingerprint(),
                variant.with_content_revision().process_spawn_fingerprint()
            );
        }
    }

    #[test]
    fn vector_order_and_optional_presence_are_unambiguous() {
        let first = snapshot();
        let mut reordered = first.clone();
        reordered.args.swap(0, 1);
        assert_ne!(
            first.process_spawn_fingerprint(),
            reordered
                .with_content_revision()
                .process_spawn_fingerprint()
        );

        let mut absent = first.clone();
        absent.base_url = None;
        let mut empty = first;
        empty.base_url = Some(String::new());
        assert_ne!(
            absent.with_content_revision().process_spawn_fingerprint(),
            empty.with_content_revision().process_spawn_fingerprint()
        );
    }

    #[test]
    fn debug_output_contains_only_secret_reference_metadata() {
        let mut value = snapshot();
        value.secret_reference_versions.insert(
            "API_KEY".to_string(),
            "ref:sha256:opaque-secret-version".to_string(),
        );
        let rendered = format!("{value:?}");
        assert!(!rendered.contains("opaque-secret-version"));
        assert!(!rendered.contains("super-secret"));
        assert!(rendered.contains("secret_reference_keys"));
    }

    #[test]
    fn session_only_values_have_no_representation_in_diff() {
        let first = snapshot();
        let second = first.clone();
        assert!(first.diff(&second).is_empty());
        assert_eq!(
            ProcessConfigStatus::from_diff(&first.diff(&second), &BTreeSet::new()),
            ProcessConfigStatus::Current
        );
    }

    #[test]
    fn restart_fields_are_conservative_and_revertible() {
        let first = snapshot();
        let mut changed = first.clone();
        changed.base_url = Some("https://other.example.test".to_string());
        changed = changed.with_content_revision();
        let diff = first.diff(&changed);
        assert_eq!(
            ProcessConfigStatus::from_diff(&diff, &BTreeSet::new()),
            ProcessConfigStatus::StaleRestartRequired
        );
        assert_eq!(
            ProcessConfigStatus::from_diff(&first.diff(&first), &BTreeSet::new()),
            ProcessConfigStatus::Current
        );
    }

    #[test]
    fn explicitly_negotiated_live_fields_use_live_status() {
        let first = snapshot();
        let mut changed = first.clone();
        changed.base_url = Some("https://other.example.test".to_string());
        let diff = first.diff(&changed);
        let live_fields = BTreeSet::from([ProcessConfigField::BaseUrl]);
        assert_eq!(
            ProcessConfigStatus::from_diff(&diff, &live_fields),
            ProcessConfigStatus::StaleLiveMutationAvailable
        );
    }

    #[test]
    fn secret_reference_version_never_contains_secret_material() {
        let version = secret_reference_version("lookup-key", "os_keychain", 42).unwrap();
        assert!(!version.contains("super-secret"));
        assert!(version.starts_with("ref:sha256:"));
    }

    #[test]
    fn debug_redaction_truncates_on_utf8_character_boundaries() {
        let value = "配".repeat(81);
        assert_eq!(redact_text(&value), format!("{}...", "配".repeat(80)));
    }
}

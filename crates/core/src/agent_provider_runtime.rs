//! Provider-neutral runtime verification contracts and the checked Agent
//! rollout manifest.
//!
//! The manifest is deliberately Rust-owned.  Catalog metadata tells Vibex how
//! to start an ACP process, while this module records the stronger claim (if
//! any) Vibex is allowed to make about provider projection and switching.  A
//! catalog entry can therefore exist without silently becoming a Secret
//! projector.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    AcpAdapterId, AgentCredentialControl, AgentCredentialKind, AgentId, AgentModelControl,
    AgentModelInterfaceDescriptor, AgentProviderControl, AgentProviderProjectionDescriptor,
    AgentProviderProjectionDescriptorId, AgentRuntimeHomeStrategy, AgentVersionCompatibility,
    ProjectionDescriptorMatch, ProjectionEvidenceReference, ProjectionEvidenceState,
    ProviderSwitchBehavior, VibexError, VibexResult, acp_agent_catalog_entries,
};
use crate::{AgentModelProviderBindingId, AgentRuntimeProbeId, AgentRuntimeProfileId};

pub const AGENT_PROVIDER_ROLLOUT_SCHEMA_VERSION: u32 = 1;
pub const AGENT_RUNTIME_PROBE_SCHEMA_VERSION: u32 = 1;
pub const MAX_PROBE_TIMEOUT_MS: u64 = 300_000;
pub const MIN_PROBE_TIMEOUT_MS: u64 = 1_000;

/// A descriptor's product boundary.  This is intentionally separate from
/// `ProjectionEvidenceState`: a documented AgentManaged capability is useful
/// even when no operator account is available for a runtime smoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderCapabilityMode {
    ReplaceableProvider,
    AgentManaged,
    CloudCredential,
    LocalModel,
    ServiceMarketplace,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "version", rename_all = "snake_case")]
pub enum AgentVersionPolicy {
    Exact,
    DetectedManual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderRolloutManifestEntry {
    pub agent_id: AgentId,
    pub catalog_version: String,
    pub adapter_id: AcpAdapterId,
    pub descriptor_id: AgentProviderProjectionDescriptorId,
    pub descriptor_version: String,
    pub version_policy: AgentVersionPolicy,
    pub capability_mode: AgentProviderCapabilityMode,
    pub runtime_home_strategy: AgentRuntimeHomeStrategy,
    pub switch_behavior: ProviderSwitchBehavior,
    pub credential_kinds: Vec<AgentCredentialKind>,
    pub model_interfaces: Vec<AgentModelInterfaceDescriptor>,
    pub evidence_state: ProjectionEvidenceState,
    pub source_evidence_reference: String,
    pub smoke_id: String,
}

impl AgentProviderRolloutManifestEntry {
    pub fn validate(&self) -> VibexResult<()> {
        if self.catalog_version.trim().is_empty()
            || self.descriptor_version.trim().is_empty()
            || self.source_evidence_reference.trim().is_empty()
            || self.smoke_id.trim().is_empty()
        {
            return Err(VibexError::validation(
                "agent_rollout_manifest_field_missing",
                "rollout manifest identity fields must be non-empty",
            ));
        }
        if self.descriptor_id.as_str().contains(['/', '\\']) || self.smoke_id.contains(['/', '\\'])
        {
            return Err(VibexError::validation(
                "agent_rollout_manifest_identity_unsafe",
                "rollout manifest identities must not contain path separators",
            ));
        }
        if !is_safe_identity(&self.catalog_version)
            || !is_safe_identity(&self.descriptor_version)
            || !is_safe_identity(&self.smoke_id)
        {
            return Err(VibexError::validation(
                "agent_rollout_manifest_identity_invalid",
                "rollout manifest versions and smoke ids must be bounded identities",
            ));
        }
        let mut interfaces = BTreeSet::new();
        for interface in &self.model_interfaces {
            if interface.wire_protocol_id.trim().is_empty()
                || !interfaces.insert((
                    interface.wire_protocol_id.as_str(),
                    interface.sdk_adapter_id.as_deref(),
                ))
            {
                return Err(VibexError::validation(
                    "agent_rollout_manifest_model_duplicate",
                    "rollout manifest model interfaces must be unique and non-empty",
                ));
            }
        }
        if matches!(
            self.capability_mode,
            AgentProviderCapabilityMode::AgentManaged
                | AgentProviderCapabilityMode::LocalModel
                | AgentProviderCapabilityMode::ServiceMarketplace
                | AgentProviderCapabilityMode::Unsupported
        ) && self.switch_behavior == ProviderSwitchBehavior::LiveSessionConfig
        {
            return Err(VibexError::validation(
                "agent_rollout_live_strategy_invalid",
                "managed, local, marketplace and unsupported Agents cannot promise live provider switching",
            ));
        }
        Ok(())
    }

    pub fn is_runtime_verified(&self) -> bool {
        self.evidence_state == ProjectionEvidenceState::Verified
    }
}

/// Runtime route derivation is shared by the ACP manager, the compatibility
/// backfill and the projection registry.  Names which already identify an ACP
/// adapter must not acquire a second `-acp` suffix.
pub fn default_acp_adapter_id(agent_id: &AgentId) -> AcpAdapterId {
    let value = agent_id.as_str();
    let adapter = if value == "claude" {
        "claude-agent-acp".to_string()
    } else if value == "codex" {
        "codex-acp".to_string()
    } else if value == "opencode" {
        "opencode-acp".to_string()
    } else if value.ends_with("-acp") || value.ends_with("-acp-agent") {
        value.to_string()
    } else {
        format!("{value}-acp")
    };
    AcpAdapterId::parse(adapter).expect("catalog Agent ids produce valid ACP adapter ids")
}

fn descriptor_id(agent_id: &AgentId) -> AgentProviderProjectionDescriptorId {
    AgentProviderProjectionDescriptorId::parse(format!(
        "projection_{}_runtime_v1",
        agent_id.as_str().replace('-', "_")
    ))
    .expect("sanitized catalog Agent ids produce valid descriptor ids")
}

fn exact_or_manual(version: &str) -> (AgentVersionPolicy, AgentVersionCompatibility) {
    if version.eq_ignore_ascii_case("manual") {
        (
            AgentVersionPolicy::DetectedManual,
            AgentVersionCompatibility::ManualVersionUnverified,
        )
    } else {
        (
            AgentVersionPolicy::Exact,
            AgentVersionCompatibility::Exact {
                adapter_version: None,
                agent_version: Some(version.to_string()),
                runtime_dependencies: BTreeMap::new(),
            },
        )
    }
}

fn mode_for(agent_id: &str) -> AgentProviderCapabilityMode {
    match agent_id {
        "agoragentic-acp" => AgentProviderCapabilityMode::ServiceMarketplace,
        "sigit" => AgentProviderCapabilityMode::LocalModel,
        "amp-acp" | "auggie" | "cursor" | "devin" | "junie" | "qoder" => {
            AgentProviderCapabilityMode::AgentManaged
        }
        "cortex-code" | "gemini" | "kiro" => AgentProviderCapabilityMode::CloudCredential,
        _ => AgentProviderCapabilityMode::ReplaceableProvider,
    }
}

fn documented_source(agent_id: &str, version: &str) -> String {
    format!("research/acp-agent/{agent_id}@{version}")
}

fn generic_projection_shape(
    agent_id: &str,
    mode: AgentProviderCapabilityMode,
) -> (
    AgentProviderControl,
    AgentCredentialControl,
    AgentModelControl,
    Vec<AgentCredentialKind>,
    Vec<AgentModelInterfaceDescriptor>,
    AgentRuntimeHomeStrategy,
    ProviderSwitchBehavior,
    ProjectionEvidenceState,
) {
    match mode {
        AgentProviderCapabilityMode::ServiceMarketplace => (
            AgentProviderControl::ServiceMarketplace,
            AgentCredentialControl::ServiceMarketplace,
            AgentModelControl::ServiceMarketplace,
            vec![AgentCredentialKind::ManagedSubscription],
            Vec::new(),
            AgentRuntimeHomeStrategy::AgentManaged,
            ProviderSwitchBehavior::AgentManaged,
            ProjectionEvidenceState::ServiceMarketplace,
        ),
        AgentProviderCapabilityMode::LocalModel => (
            AgentProviderControl::LocalModel,
            AgentCredentialControl::Local,
            AgentModelControl::LocalModel,
            vec![AgentCredentialKind::Local],
            Vec::new(),
            AgentRuntimeHomeStrategy::VibexPrivate,
            ProviderSwitchBehavior::RestartFreshAndBridge,
            ProjectionEvidenceState::Local,
        ),
        AgentProviderCapabilityMode::AgentManaged => (
            AgentProviderControl::AgentManaged,
            AgentCredentialControl::OAuthAgentManaged,
            AgentModelControl::AgentManaged,
            vec![
                AgentCredentialKind::OAuth,
                AgentCredentialKind::ManagedSubscription,
            ],
            Vec::new(),
            AgentRuntimeHomeStrategy::AgentManaged,
            ProviderSwitchBehavior::AgentManaged,
            ProjectionEvidenceState::AgentManaged,
        ),
        AgentProviderCapabilityMode::CloudCredential => {
            let (credential, kinds) = match agent_id {
                "cortex-code" => (
                    AgentCredentialControl::AdvertisedAuthMethod {
                        method_ids: vec!["snowflake_connection".to_string(), "oauth".to_string()],
                    },
                    vec![AgentCredentialKind::Snowflake, AgentCredentialKind::OAuth],
                ),
                "gemini" => (
                    AgentCredentialControl::AdvertisedAuthMethod {
                        method_ids: vec![
                            "google_oauth".to_string(),
                            "api_key".to_string(),
                            "vertex_adc".to_string(),
                        ],
                    },
                    vec![
                        AgentCredentialKind::OAuth,
                        AgentCredentialKind::ApiKey,
                        AgentCredentialKind::Gcp,
                    ],
                ),
                _ => (
                    AgentCredentialControl::AdvertisedAuthMethod {
                        method_ids: vec!["agent_login".to_string(), "aws_chain".to_string()],
                    },
                    vec![AgentCredentialKind::OAuth, AgentCredentialKind::Aws],
                ),
            };
            (
                AgentProviderControl::AdvertisedSessionOption {
                    option_ids: vec!["provider".to_string(), "model".to_string()],
                },
                credential,
                AgentModelControl::AcpConfigOption {
                    aliases: vec!["model".to_string(), "deployment".to_string()],
                },
                kinds,
                Vec::new(),
                AgentRuntimeHomeStrategy::AgentManaged,
                ProviderSwitchBehavior::RestartAndResume,
                ProjectionEvidenceState::Documented,
            )
        }
        AgentProviderCapabilityMode::ReplaceableProvider => {
            (
                // The upstream matrix documents provider entry points, but it
                // does not prove a Vibex-owned isolated projector. Exact
                // catalog identity therefore resolves to an explicit
                // conservative descriptor until a typed projector and runtime
                // smoke are added for that version.
                AgentProviderControl::Unverified,
                AgentCredentialControl::Unverified,
                AgentModelControl::Unverified,
                Vec::new(),
                Vec::new(),
                AgentRuntimeHomeStrategy::VibexPrivate,
                ProviderSwitchBehavior::Unverified,
                ProjectionEvidenceState::Documented,
            )
        }
        AgentProviderCapabilityMode::Unsupported => (
            AgentProviderControl::Unsupported,
            AgentCredentialControl::Unsupported,
            AgentModelControl::Unsupported,
            Vec::new(),
            Vec::new(),
            AgentRuntimeHomeStrategy::None,
            ProviderSwitchBehavior::Unsupported,
            ProjectionEvidenceState::Unsupported,
        ),
    }
}

fn catalog_manifest_entry(
    id: &str,
    version: &str,
) -> VibexResult<AgentProviderRolloutManifestEntry> {
    let agent_id = AgentId::parse(id)?;
    let mode = mode_for(id);
    let (version_policy, _) = exact_or_manual(version);
    let (_, _, _, credential_kinds, model_interfaces, home, switch_behavior, evidence_state) =
        generic_projection_shape(id, mode);
    let entry = AgentProviderRolloutManifestEntry {
        agent_id: agent_id.clone(),
        catalog_version: version.to_string(),
        adapter_id: default_acp_adapter_id(&agent_id),
        descriptor_id: descriptor_id(&agent_id),
        descriptor_version: "1".to_string(),
        version_policy,
        capability_mode: mode,
        runtime_home_strategy: home,
        switch_behavior,
        credential_kinds,
        model_interfaces,
        evidence_state,
        source_evidence_reference: documented_source(id, version),
        smoke_id: format!("agent-provider-{id}"),
    };
    entry.validate()?;
    Ok(entry)
}

/// Returns the complete checked matrix: three builtins plus every catalog id.
pub fn agent_provider_rollout_manifest() -> VibexResult<Vec<AgentProviderRolloutManifestEntry>> {
    let mut entries = Vec::with_capacity(acp_agent_catalog_entries().len() + 3);
    for (id, version) in [
        ("claude", "0.64.2"),
        ("codex", "0.146.0"),
        ("opencode", "1.17.9"),
    ] {
        let agent_id = AgentId::parse(id)?;
        entries.push(AgentProviderRolloutManifestEntry {
            agent_id: agent_id.clone(),
            catalog_version: version.to_string(),
            adapter_id: default_acp_adapter_id(&agent_id),
            descriptor_id: AgentProviderProjectionDescriptorId::parse(match id {
                "claude" => "projection_claude_environment_v1",
                "codex" => "projection_codex_stable_home_v1",
                _ => "projection_opencode_inline_provider_v1",
            })?,
            descriptor_version: "1".to_string(),
            version_policy: AgentVersionPolicy::Exact,
            capability_mode: AgentProviderCapabilityMode::ReplaceableProvider,
            runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            credential_kinds: vec![AgentCredentialKind::ApiKey],
            model_interfaces: Vec::new(),
            // The platform descriptor is known, but a runtime probe is the
            // only authority allowed to promote a concrete profile to
            // Verified. The rollout manifest therefore starts conservatively.
            evidence_state: ProjectionEvidenceState::Documented,
            source_evidence_reference: format!("provider-config/{id}-environment-v1"),
            smoke_id: format!("builtin-provider-{id}"),
        });
    }
    for entry in acp_agent_catalog_entries() {
        entries.push(catalog_manifest_entry(entry.id, entry.version)?);
    }
    validate_rollout_manifest(&entries)?;
    Ok(entries)
}

pub fn validate_rollout_manifest(entries: &[AgentProviderRolloutManifestEntry]) -> VibexResult<()> {
    let expected_catalog = acp_agent_catalog_entries()
        .iter()
        .map(|entry| entry.id)
        .collect::<BTreeSet<_>>();
    let actual = entries
        .iter()
        .map(|entry| entry.agent_id.as_str())
        .collect::<BTreeSet<_>>();
    if entries.len() != 38 || actual.len() != entries.len() {
        return Err(VibexError::conflict(
            "agent_rollout_manifest_coverage_invalid",
            "rollout manifest must contain exactly 38 unique Agent ids",
        ));
    }
    let catalog_actual = actual
        .iter()
        .copied()
        .filter(|id| !matches!(*id, "claude" | "codex" | "opencode"))
        .collect::<BTreeSet<_>>();
    if catalog_actual != expected_catalog {
        return Err(VibexError::conflict(
            "agent_rollout_manifest_catalog_drift",
            "rollout manifest and ACP catalog contain different Agent ids",
        ));
    }
    let mut descriptors = BTreeSet::new();
    let mut adapters = BTreeSet::new();
    for entry in entries {
        entry.validate()?;
        if !descriptors.insert(entry.descriptor_id.as_str())
            || !adapters.insert(entry.adapter_id.as_str())
        {
            return Err(VibexError::conflict(
                "agent_rollout_manifest_identity_duplicate",
                "rollout manifest descriptor and adapter identities must be unique",
            ));
        }
    }
    Ok(())
}

/// Build catalog descriptors.  Builtin descriptors remain in
/// `provider_projection.rs` because they carry their established, stronger
/// evidence contracts.
pub fn catalog_projection_descriptors() -> VibexResult<Vec<AgentProviderProjectionDescriptor>> {
    let mut result = Vec::with_capacity(acp_agent_catalog_entries().len());
    for entry in acp_agent_catalog_entries() {
        let agent_id = AgentId::parse(entry.id)?;
        let mode = mode_for(entry.id);
        let (
            provider_control,
            credential_control,
            model_control,
            credential_kinds,
            model_interfaces,
            runtime_home_strategy,
            switch_behavior,
            evidence_state,
        ) = generic_projection_shape(entry.id, mode);
        let (version_policy, compatibility) = exact_or_manual(entry.version);
        let _ = version_policy;
        result.push(AgentProviderProjectionDescriptor {
            id: descriptor_id(&agent_id),
            descriptor_version: "1".to_string(),
            route: crate::AgentRuntimeRouteKey {
                agent_id: agent_id.clone(),
                transport_kind: crate::TransportKind::Acp,
                adapter_id: default_acp_adapter_id(&agent_id),
            },
            compatibility,
            provider_control,
            credential_control,
            model_control,
            credential_kinds,
            model_interfaces,
            runtime_home_strategy,
            switch_behavior,
            evidence: ProjectionEvidenceReference {
                state: evidence_state,
                source_reference: Some(documented_source(entry.id, entry.version)),
                runtime_reference: None,
            },
        });
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeProbeStage {
    Requested,
    ResolvingIdentity,
    PlanningProjection,
    StartingProcess,
    InitializingAcp,
    Authenticating,
    CreatingOrLoadingSession,
    DiscoveringModels,
    ApplyingModelAndConfig,
    OptionalMinimalPrompt,
    ConfirmingEffectiveProvider,
    CleaningUp,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeProbeStatus {
    Requested,
    Running,
    Passed,
    Failed,
    Cancelled,
    TimedOut,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeProbeCapability {
    BinaryIdentity,
    AcpHandshake,
    Authentication,
    Session,
    ModelCatalog,
    ModelSelection,
    ProviderProjection,
    SessionResume,
    SwitchCompatibility,
    Redaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeProbeFactStatus {
    Passed,
    Failed,
    Unsupported,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeFact {
    pub capability: AgentRuntimeProbeCapability,
    pub status: AgentRuntimeProbeFactStatus,
    pub diagnostic_code: Option<String>,
}

impl AgentRuntimeProbeFact {
    pub fn passed(capability: AgentRuntimeProbeCapability) -> Self {
        Self {
            capability,
            status: AgentRuntimeProbeFactStatus::Passed,
            diagnostic_code: None,
        }
    }

    pub fn blocked(capability: AgentRuntimeProbeCapability, code: &str) -> Self {
        Self {
            capability,
            status: AgentRuntimeProbeFactStatus::Blocked,
            diagnostic_code: Some(code.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeEvidence {
    pub schema_version: u32,
    pub agent_id: AgentId,
    pub agent_version: Option<String>,
    pub adapter_id: AcpAdapterId,
    pub adapter_version: Option<String>,
    pub descriptor_id: AgentProviderProjectionDescriptorId,
    pub descriptor_version: String,
    #[serde(default = "default_probe_descriptor_match")]
    pub descriptor_match: ProjectionDescriptorMatch,
    /// A content fingerprint only; the resolved overlay and endpoint values
    /// never enter durable evidence.
    #[serde(default)]
    pub projection_fingerprint: Option<String>,
    pub source_revision: String,
    pub platform_os: String,
    pub platform_arch: String,
    pub facts: Vec<AgentRuntimeProbeFact>,
    pub switch_behavior: ProviderSwitchBehavior,
    pub source_survived_prepare_failure: bool,
    pub redaction_passed: bool,
    pub recorded_at_ms: i64,
}

impl AgentRuntimeProbeEvidence {
    pub fn provider_projection_verified(&self) -> bool {
        self.descriptor_match == ProjectionDescriptorMatch::Exact
            && self
                .projection_fingerprint
                .as_deref()
                .is_some_and(is_safe_fingerprint)
            && self.redaction_passed
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::BinaryIdentity
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::AcpHandshake
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::Authentication
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::Session
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::ProviderProjection
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::ModelSelection
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
    }

    /// Live provider mutation is a stronger claim than an effective provider
    /// probe. It additionally requires an exercised switch compatibility fact
    /// and proof that a failed target prepare left the source usable.
    pub fn live_switch_verified(&self) -> bool {
        self.provider_projection_verified()
            && self.switch_behavior == ProviderSwitchBehavior::LiveSessionConfig
            && self.source_survived_prepare_failure
            && self.facts.iter().any(|fact| {
                fact.capability == AgentRuntimeProbeCapability::SwitchCompatibility
                    && fact.status == AgentRuntimeProbeFactStatus::Passed
            })
    }

    pub fn validate(&self) -> VibexResult<()> {
        if self.schema_version != AGENT_RUNTIME_PROBE_SCHEMA_VERSION
            || self.source_revision.trim().is_empty()
            || self.platform_os.trim().is_empty()
            || self.platform_arch.trim().is_empty()
            || self.descriptor_version.trim().is_empty()
            || !is_safe_identity(&self.descriptor_version)
            || !is_safe_identity(&self.source_revision)
            || !is_safe_identity(&self.platform_os)
            || !is_safe_identity(&self.platform_arch)
        {
            return Err(VibexError::validation(
                "agent_probe_evidence_identity_invalid",
                "runtime probe evidence identity is incomplete",
            ));
        }
        if self
            .agent_version
            .as_deref()
            .is_some_and(|value| !is_safe_identity(value))
            || self
                .adapter_version
                .as_deref()
                .is_some_and(|value| !is_safe_identity(value))
        {
            return Err(VibexError::validation(
                "agent_probe_evidence_version_invalid",
                "runtime probe versions must be bounded identities",
            ));
        }
        if self
            .projection_fingerprint
            .as_deref()
            .is_some_and(|value| !is_safe_fingerprint(value))
        {
            return Err(VibexError::validation(
                "agent_probe_fingerprint_invalid",
                "runtime probe projection fingerprints must be bounded hashes",
            ));
        }
        let mut seen = BTreeSet::new();
        for fact in &self.facts {
            if !seen.insert(fact.capability) {
                return Err(VibexError::validation(
                    "agent_probe_fact_duplicate",
                    "runtime probe evidence cannot contain duplicate capability facts",
                ));
            }
            if let Some(code) = fact.diagnostic_code.as_deref()
                && (code.len() > 96
                    || code.is_empty()
                    || !code
                        .chars()
                        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'))
            {
                return Err(VibexError::validation(
                    "agent_probe_diagnostic_code_invalid",
                    "runtime probe diagnostics must be bounded stable codes",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeRequest {
    pub runtime_profile_id: AgentRuntimeProfileId,
    pub binding_id: Option<AgentModelProviderBindingId>,
    pub workspace_key: String,
    pub timeout_ms: u64,
    pub minimal_prompt: bool,
}

impl AgentRuntimeProbeRequest {
    pub fn validate(&self) -> VibexResult<()> {
        if self.workspace_key.trim().is_empty()
            || self.workspace_key.len() > 192
            || !self
                .workspace_key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(VibexError::validation(
                "agent_probe_workspace_key_invalid",
                "runtime probe workspace key must be a bounded non-path identity",
            ));
        }
        if !(MIN_PROBE_TIMEOUT_MS..=MAX_PROBE_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(VibexError::validation(
                "agent_probe_timeout_invalid",
                "runtime probe timeout is outside the supported bounds",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeRecord {
    pub id: AgentRuntimeProbeId,
    pub request: AgentRuntimeProbeRequest,
    pub agent_id: AgentId,
    pub adapter_id: AcpAdapterId,
    pub descriptor_id: AgentProviderProjectionDescriptorId,
    pub descriptor_version: String,
    pub status: AgentRuntimeProbeStatus,
    pub stage: AgentRuntimeProbeStage,
    pub facts: Vec<AgentRuntimeProbeFact>,
    pub evidence: Option<AgentRuntimeProbeEvidence>,
    pub diagnostic_code: Option<String>,
    pub cancel_requested: bool,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

impl AgentRuntimeProbeRecord {
    pub fn requested(
        id: AgentRuntimeProbeId,
        request: AgentRuntimeProbeRequest,
        agent_id: AgentId,
        adapter_id: AcpAdapterId,
        descriptor_id: AgentProviderProjectionDescriptorId,
        descriptor_version: String,
        now_ms: i64,
    ) -> VibexResult<Self> {
        request.validate()?;
        Ok(Self {
            id,
            request,
            agent_id,
            adapter_id,
            descriptor_id,
            descriptor_version,
            status: AgentRuntimeProbeStatus::Requested,
            stage: AgentRuntimeProbeStage::Requested,
            facts: Vec::new(),
            evidence: None,
            diagnostic_code: None,
            cancel_requested: false,
            revision: 1,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            finished_at_ms: None,
        })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            AgentRuntimeProbeStatus::Passed
                | AgentRuntimeProbeStatus::Failed
                | AgentRuntimeProbeStatus::Cancelled
                | AgentRuntimeProbeStatus::TimedOut
                | AgentRuntimeProbeStatus::Blocked
        )
    }

    pub fn set_cancel_requested(&mut self, now_ms: i64) {
        self.cancel_requested = true;
        self.updated_at_ms = now_ms.max(self.updated_at_ms);
        self.revision = self.revision.saturating_add(1).max(1);
    }

    pub fn validate(&self) -> VibexResult<()> {
        self.request.validate()?;
        if self.revision <= 0
            || self.descriptor_version.trim().is_empty()
            || !is_safe_identity(&self.descriptor_version)
            || self
                .diagnostic_code
                .as_deref()
                .is_some_and(|code| !is_diagnostic_code(code))
        {
            return Err(VibexError::validation(
                "agent_probe_record_invalid",
                "runtime probe record identity is invalid",
            ));
        }
        if let Some(evidence) = &self.evidence {
            evidence.validate()?;
            if evidence.agent_id != self.agent_id
                || evidence.adapter_id != self.adapter_id
                || evidence.descriptor_id != self.descriptor_id
                || evidence.descriptor_version != self.descriptor_version
            {
                return Err(VibexError::conflict(
                    "agent_probe_evidence_identity_mismatch",
                    "runtime probe evidence does not match its durable record",
                ));
            }
        }
        let mut seen = BTreeSet::new();
        for fact in &self.facts {
            if !seen.insert(fact.capability) {
                return Err(VibexError::validation(
                    "agent_probe_record_fact_duplicate",
                    "runtime probe record cannot contain duplicate facts",
                ));
            }
        }
        Ok(())
    }
}

/// Provider-neutral request used by Backend/Remote callers to create a
/// durable probe. The execution worker is intentionally separate from this
/// DTO so a caller can observe/cancel a probe after a process crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeStartRequest {
    pub runtime_profile_id: AgentRuntimeProfileId,
    pub binding_id: Option<AgentModelProviderBindingId>,
    pub workspace_key: String,
    pub timeout_ms: u64,
    #[serde(default)]
    pub minimal_prompt: bool,
}

impl AgentRuntimeProbeStartRequest {
    pub fn validate(&self) -> VibexResult<()> {
        AgentRuntimeProbeRequest {
            runtime_profile_id: self.runtime_profile_id.clone(),
            binding_id: self.binding_id.clone(),
            workspace_key: self.workspace_key.clone(),
            timeout_ms: self.timeout_ms,
            minimal_prompt: self.minimal_prompt,
        }
        .validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeListRequest {
    pub runtime_profile_id: Option<AgentRuntimeProfileId>,
    pub limit: Option<usize>,
}

impl Default for AgentRuntimeProbeListRequest {
    fn default() -> Self {
        Self {
            runtime_profile_id: None,
            limit: Some(100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeCancelRequest {
    pub probe_id: AgentRuntimeProbeId,
    pub expected_revision: i64,
}

fn is_safe_identity(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 192
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@' | '+' | '/')
        })
        && !value.contains("..")
}

fn is_diagnostic_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn default_probe_descriptor_match() -> ProjectionDescriptorMatch {
    ProjectionDescriptorMatch::Conservative
}

fn is_safe_fingerprint(value: &str) -> bool {
    let value = value.trim();
    value.len() <= 192
        && value.starts_with("sha256:")
        && value[7..].len() >= 16
        && value[7..].chars().all(|ch| ch.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_exact_catalog_coverage_and_unique_routes() {
        let manifest = agent_provider_rollout_manifest().unwrap();
        validate_rollout_manifest(&manifest).unwrap();
        assert_eq!(manifest.len(), 38);
        assert!(
            manifest
                .iter()
                .any(|entry| entry.agent_id.as_str() == "agoragentic-acp")
        );
        assert!(
            manifest
                .iter()
                .any(|entry| entry.agent_id.as_str() == "sigit")
        );
        assert!(
            manifest
                .iter()
                .all(|entry| !entry.source_evidence_reference.is_empty())
        );
    }

    #[test]
    fn adapter_derivation_does_not_double_suffix_catalog_adapters() {
        assert_eq!(
            default_acp_adapter_id(&AgentId::parse("amp-acp").unwrap()).as_str(),
            "amp-acp"
        );
        assert_eq!(
            default_acp_adapter_id(&AgentId::parse("glm-acp-agent").unwrap()).as_str(),
            "glm-acp-agent"
        );
        assert_eq!(
            default_acp_adapter_id(&AgentId::parse("qwen-code").unwrap()).as_str(),
            "qwen-code-acp"
        );
    }

    #[test]
    fn evidence_never_upgrades_without_provider_and_model_facts() {
        let mut evidence = AgentRuntimeProbeEvidence {
            schema_version: AGENT_RUNTIME_PROBE_SCHEMA_VERSION,
            agent_id: AgentId::parse("fixture").unwrap(),
            agent_version: Some("1.0.0".to_string()),
            adapter_id: AcpAdapterId::parse("fixture-acp").unwrap(),
            adapter_version: None,
            descriptor_id: AgentProviderProjectionDescriptorId::parse("projection_fixture_v1")
                .unwrap(),
            descriptor_version: "1".to_string(),
            descriptor_match: ProjectionDescriptorMatch::Exact,
            projection_fingerprint: Some("sha256:0123456789abcdef".to_string()),
            source_revision: "fixture".to_string(),
            platform_os: "linux".to_string(),
            platform_arch: "x86_64".to_string(),
            facts: vec![
                AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::BinaryIdentity),
                AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::AcpHandshake),
                AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::Authentication),
                AgentRuntimeProbeFact::passed(AgentRuntimeProbeCapability::Session),
            ],
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            source_survived_prepare_failure: true,
            redaction_passed: true,
            recorded_at_ms: 1,
        };
        assert!(!evidence.provider_projection_verified());
        evidence.facts.push(AgentRuntimeProbeFact::passed(
            AgentRuntimeProbeCapability::ProviderProjection,
        ));
        assert!(!evidence.provider_projection_verified());
        evidence.facts.push(AgentRuntimeProbeFact::passed(
            AgentRuntimeProbeCapability::ModelSelection,
        ));
        assert!(evidence.provider_projection_verified());
        assert!(!evidence.live_switch_verified());
        evidence.switch_behavior = ProviderSwitchBehavior::LiveSessionConfig;
        evidence.facts.push(AgentRuntimeProbeFact::passed(
            AgentRuntimeProbeCapability::SwitchCompatibility,
        ));
        assert!(evidence.live_switch_verified());
    }

    #[test]
    fn descriptor_or_fingerprint_drift_closes_runtime_verification() {
        let mut evidence = AgentRuntimeProbeEvidence {
            schema_version: AGENT_RUNTIME_PROBE_SCHEMA_VERSION,
            agent_id: AgentId::parse("fixture").unwrap(),
            agent_version: Some("1.0.0".to_string()),
            adapter_id: AcpAdapterId::parse("fixture-acp").unwrap(),
            adapter_version: None,
            descriptor_id: AgentProviderProjectionDescriptorId::parse("projection_fixture_v1")
                .unwrap(),
            descriptor_version: "1".to_string(),
            descriptor_match: ProjectionDescriptorMatch::Exact,
            projection_fingerprint: Some("sha256:0123456789abcdef".to_string()),
            source_revision: "fixture".to_string(),
            platform_os: "linux".to_string(),
            platform_arch: "x86_64".to_string(),
            facts: [
                AgentRuntimeProbeCapability::BinaryIdentity,
                AgentRuntimeProbeCapability::AcpHandshake,
                AgentRuntimeProbeCapability::Authentication,
                AgentRuntimeProbeCapability::Session,
                AgentRuntimeProbeCapability::ModelSelection,
                AgentRuntimeProbeCapability::ProviderProjection,
            ]
            .into_iter()
            .map(AgentRuntimeProbeFact::passed)
            .collect(),
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            source_survived_prepare_failure: false,
            redaction_passed: true,
            recorded_at_ms: 1,
        };
        assert!(evidence.provider_projection_verified());
        evidence.descriptor_match = ProjectionDescriptorMatch::Conservative;
        assert!(!evidence.provider_projection_verified());
        evidence.descriptor_match = ProjectionDescriptorMatch::Exact;
        evidence.projection_fingerprint = Some("not-a-fingerprint".to_string());
        assert!(!evidence.provider_projection_verified());
    }

    #[test]
    fn catalog_replaceable_agents_do_not_claim_unimplemented_generic_projection() {
        for descriptor in catalog_projection_descriptors().unwrap() {
            let mode = mode_for(descriptor.route.agent_id.as_str());
            if mode != AgentProviderCapabilityMode::ReplaceableProvider {
                continue;
            }
            assert_eq!(
                descriptor.provider_control,
                AgentProviderControl::Unverified
            );
            assert_eq!(
                descriptor.credential_control,
                AgentCredentialControl::Unverified
            );
            assert_eq!(descriptor.model_control, AgentModelControl::Unverified);
            assert!(descriptor.credential_kinds.is_empty());
            assert!(descriptor.model_interfaces.is_empty());
            assert_eq!(
                descriptor.switch_behavior,
                ProviderSwitchBehavior::Unverified
            );
        }
    }
}

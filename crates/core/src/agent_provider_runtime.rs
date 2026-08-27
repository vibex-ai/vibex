//! Provider-neutral runtime verification contracts and the checked Agent
//! rollout manifest.
//!
//! The manifest is deliberately Rust-owned.  Catalog metadata tells Vibex how
//! to start an ACP process, while this module records the stronger claim (if
//! any) Vibex is allowed to make about provider projection and switching.  A
//! catalog entry can therefore exist without silently becoming a Secret
//! projector.

use std::collections::{BTreeMap, BTreeSet};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::provider_projection::{
    ConfigOverlayStrategy, OPENCODE_COMPATIBLE_VERSION_REQUIREMENT, OPENCODE_LAST_VERIFIED_VERSION,
};
use crate::{
    AcpAdapterId, AgentCredentialControl, AgentCredentialKind, AgentId, AgentModelControl,
    AgentModelInterfaceDescriptor, AgentModelInterfaceIntegrationKind, AgentProviderControl,
    AgentProviderProjectionDescriptor, AgentProviderProjectionDescriptorId,
    AgentRuntimeHomeStrategy, AgentVersionCompatibility, ProjectionDescriptorMatch,
    ProjectionEvidenceReference, ProjectionEvidenceState, ProviderSecretKind,
    ProviderSwitchBehavior, VibexError, VibexResult, WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
    WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE, WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, WIRE_PROTOCOL_OPENAI_RESPONSES,
    acp_agent_catalog_entries,
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentVersionPolicy {
    Exact,
    DetectedSemver { requirement: String },
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
    pub capability_diagnostic_code: Option<String>,
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
        if self
            .capability_diagnostic_code
            .as_deref()
            .is_some_and(|code| !is_diagnostic_code(code))
        {
            return Err(VibexError::validation(
                "agent_rollout_manifest_diagnostic_invalid",
                "rollout manifest capability diagnostics must be bounded stable codes",
            ));
        }
        let conservative = self.evidence_state == ProjectionEvidenceState::Unverified
            || self.switch_behavior == ProviderSwitchBehavior::Unverified;
        if conservative
            && (self.evidence_state != ProjectionEvidenceState::Unverified
                || self.switch_behavior != ProviderSwitchBehavior::Unverified
                || self.runtime_home_strategy != AgentRuntimeHomeStrategy::None
                || !self.credential_kinds.is_empty()
                || !self.model_interfaces.is_empty()
                || self.capability_diagnostic_code.is_none())
        {
            return Err(VibexError::validation(
                "agent_rollout_manifest_conservative_shape_invalid",
                "conservative rollout entries must disable projection and carry an explicit diagnostic",
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
        match &self.version_policy {
            AgentVersionPolicy::Exact if self.catalog_version == "manual" => {
                return Err(VibexError::validation(
                    "agent_rollout_manifest_version_policy_invalid",
                    "exact rollout entries cannot use the manual catalog version",
                ));
            }
            AgentVersionPolicy::DetectedManual if self.catalog_version != "manual" => {
                return Err(VibexError::validation(
                    "agent_rollout_manifest_version_policy_invalid",
                    "manual rollout entries must use the manual catalog version",
                ));
            }
            AgentVersionPolicy::DetectedSemver { requirement }
                if requirement.trim().is_empty()
                    || requirement.len() > 192
                    || VersionReq::parse(requirement).is_err()
                    || self.catalog_version == "manual" =>
            {
                return Err(VibexError::validation(
                    "agent_rollout_manifest_version_policy_invalid",
                    "detected semantic-version rollout entries require a valid non-manual range",
                ));
            }
            _ => {}
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

    pub fn supports_model_provider_configuration(&self) -> bool {
        self.capability_mode == AgentProviderCapabilityMode::ReplaceableProvider
            && self.evidence_state != ProjectionEvidenceState::Unverified
            && self.switch_behavior != ProviderSwitchBehavior::Unverified
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
    } else if value == "zcode" {
        "zcode-acp-server".to_string()
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

fn at_least_or_manual(version: &str) -> (AgentVersionPolicy, AgentVersionCompatibility) {
    if version.eq_ignore_ascii_case("manual") {
        return exact_or_manual(version);
    }
    let requirement = format!(">={version}");
    (
        AgentVersionPolicy::DetectedSemver {
            requirement: requirement.clone(),
        },
        AgentVersionCompatibility::SemverRange {
            adapter_range: None,
            agent_range: Some(requirement),
            runtime_dependency_ranges: BTreeMap::new(),
        },
    )
}

fn mode_for(agent_id: &str) -> AgentProviderCapabilityMode {
    match agent_id {
        "amp-acp" | "auggie" | "cursor" | "devin" | "junie" | "qoder" => {
            AgentProviderCapabilityMode::AgentManaged
        }
        "kiro" => AgentProviderCapabilityMode::CloudCredential,
        _ => AgentProviderCapabilityMode::ReplaceableProvider,
    }
}

fn documented_source(agent_id: &str, version: &str) -> String {
    format!("research/acp-agent/{agent_id}@{version}")
}

struct CatalogProjectionShape {
    provider_control: AgentProviderControl,
    credential_control: AgentCredentialControl,
    model_control: AgentModelControl,
    credential_kinds: Vec<AgentCredentialKind>,
    model_interfaces: Vec<AgentModelInterfaceDescriptor>,
    runtime_home_strategy: AgentRuntimeHomeStrategy,
    switch_behavior: ProviderSwitchBehavior,
    evidence_state: ProjectionEvidenceState,
    capability_diagnostic_code: Option<&'static str>,
}

impl CatalogProjectionShape {
    fn supports_vibex_model_provider_projection(&self) -> bool {
        matches!(
            self.provider_control,
            AgentProviderControl::Environment { .. }
                | AgentProviderControl::ManagedConfigOverlay { .. }
                | AgentProviderControl::AdvertisedSessionOption { .. }
        )
    }
}

fn catalog_version_compatibility(
    version: &str,
    mode: AgentProviderCapabilityMode,
    shape: &CatalogProjectionShape,
) -> (AgentVersionPolicy, AgentVersionCompatibility) {
    // Every explicit ReplaceableProvider projector uses its researched
    // catalog version as a minimum supported version. Managed/cloud/local
    // Agents and conservative entries retain their stricter policies.
    if mode == AgentProviderCapabilityMode::ReplaceableProvider
        && shape.supports_vibex_model_provider_projection()
    {
        at_least_or_manual(version)
    } else {
        exact_or_manual(version)
    }
}

fn catalog_projection_shape(
    agent_id: &str,
    mode: AgentProviderCapabilityMode,
) -> VibexResult<CatalogProjectionShape> {
    if mode == AgentProviderCapabilityMode::ReplaceableProvider {
        return match agent_id {
            "antigravity" => Ok(antigravity_projection_shape()),
            "codebuddy-code" => Ok(environment_projection_shape(
                "CODEBUDDY_BASE_URL",
                "CODEBUDDY_API_KEY",
                "CODEBUDDY_MODEL",
            )),
            "glm-acp-agent" => Ok(environment_projection_shape(
                "ACP_GLM_BASE_URL",
                "Z_AI_API_KEY",
                "ACP_GLM_MODEL",
            )),
            "gemini" => Ok(environment_projection_shape_with_interfaces(
                "GOOGLE_GEMINI_BASE_URL",
                "GEMINI_API_KEY",
                "GEMINI_MODEL",
                vec![catalog_interface(
                    WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
                    false,
                    true,
                )],
            )),
            "copilot" => Ok(environment_projection_shape(
                "COPILOT_PROVIDER_BASE_URL",
                "COPILOT_PROVIDER_API_KEY",
                "COPILOT_MODEL",
            )),
            "codewhale" => Ok(environment_projection_shape(
                "CODEWHALE_BASE_URL",
                "OPENAI_API_KEY",
                "CODEWHALE_MODEL",
            )),
            "kimi" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::KimiToml,
                "VIBEX_KIMI_API_KEY",
                vec![
                    catalog_interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true, true),
                    catalog_interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, true, true),
                ],
            )),
            "poolside" => Ok(environment_projection_shape(
                "POOLSIDE_STANDALONE_BASE_URL",
                "POOLSIDE_API_KEY",
                "POOLSIDE_STANDALONE_MODEL",
            )),
            "crow-cli" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::CrowCliYaml,
                "VIBEX_CROW_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            "dirac" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::DiracToml,
                "OPENAI_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            "factory-droid" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::FactoryDroidJson,
                "VIBEX_FACTORY_DROID_API_KEY",
                vec![
                    catalog_interface(WIRE_PROTOCOL_OPENAI_RESPONSES, true, true),
                    catalog_interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true, true),
                    catalog_interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, true, true),
                ],
            )),
            "goose" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::GooseJson,
                "VIBEX_GOOSE_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            "grok" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::GrokToml,
                "VIBEX_GROK_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_RESPONSES,
                    false,
                    true,
                )],
            )),
            "hermes" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::HermesYaml,
                "VIBEX_HERMES_API_KEY",
                vec![
                    catalog_interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true, true),
                    catalog_interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, true, true),
                    catalog_interface(WIRE_PROTOCOL_OPENAI_RESPONSES, true, true),
                    catalog_interface(WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE, true, true),
                ],
            )),
            "kilo" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::KiloInlineJson,
                "VIBEX_KILO_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            "mistral-vibe" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::MistralVibeToml,
                "VIBEX_MISTRAL_VIBE_API_KEY",
                vec![
                    catalog_interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true, true),
                    catalog_interface(WIRE_PROTOCOL_OPENAI_RESPONSES, true, true),
                    catalog_interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, true, true),
                ],
            )),
            "pi" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::PiModelsJson,
                "VIBEX_PI_API_KEY",
                vec![
                    catalog_interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true, true),
                    catalog_interface(WIRE_PROTOCOL_OPENAI_RESPONSES, true, true),
                    catalog_interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, true, true),
                ],
            )),
            "qwen-code" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::QwenCodeJson,
                "VIBEX_QWEN_CODE_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            "stakpak" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::StakpakToml,
                "OPENAI_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            "vtcode" => Ok(overlay_projection_shape(
                ConfigOverlayStrategy::VtcodeToml,
                "VIBEX_VTCODE_API_KEY",
                vec![catalog_interface(
                    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                    false,
                    true,
                )],
            )),
            _ => conservative_replaceable_shape(agent_id),
        };
    }

    Ok(match mode {
        AgentProviderCapabilityMode::AgentManaged => CatalogProjectionShape {
            provider_control: AgentProviderControl::AgentManaged,
            credential_control: AgentCredentialControl::OAuthAgentManaged,
            model_control: AgentModelControl::AgentManaged,
            credential_kinds: vec![
                AgentCredentialKind::OAuth,
                AgentCredentialKind::ManagedSubscription,
            ],
            model_interfaces: Vec::new(),
            runtime_home_strategy: AgentRuntimeHomeStrategy::AgentManaged,
            switch_behavior: ProviderSwitchBehavior::AgentManaged,
            evidence_state: ProjectionEvidenceState::AgentManaged,
            capability_diagnostic_code: None,
        },
        AgentProviderCapabilityMode::CloudCredential => {
            let (credential_control, credential_kinds) = match agent_id {
                "kiro" => (
                    AgentCredentialControl::AdvertisedAuthMethod {
                        method_ids: vec!["agent_login".to_string(), "aws_chain".to_string()],
                    },
                    vec![AgentCredentialKind::OAuth, AgentCredentialKind::Aws],
                ),
                _ => {
                    return Err(VibexError::validation(
                        "agent_rollout_cloud_contract_missing",
                        "cloud credential Agent has no explicit catalog contract",
                    ));
                }
            };
            CatalogProjectionShape {
                provider_control: AgentProviderControl::AdvertisedSessionOption {
                    option_ids: vec!["provider".to_string(), "model".to_string()],
                },
                credential_control,
                model_control: AgentModelControl::AcpConfigOption {
                    aliases: vec!["model".to_string(), "deployment".to_string()],
                },
                credential_kinds,
                model_interfaces: Vec::new(),
                runtime_home_strategy: AgentRuntimeHomeStrategy::AgentManaged,
                switch_behavior: ProviderSwitchBehavior::RestartAndResume,
                evidence_state: ProjectionEvidenceState::Documented,
                capability_diagnostic_code: Some("agent_projection_runtime_verification_required"),
            }
        }
        AgentProviderCapabilityMode::LocalModel
        | AgentProviderCapabilityMode::ServiceMarketplace => {
            return Err(VibexError::validation(
                "agent_rollout_legacy_capability_mode",
                "legacy Agent capability modes are no longer supported",
            ));
        }
        AgentProviderCapabilityMode::Unsupported => CatalogProjectionShape {
            provider_control: AgentProviderControl::Unsupported,
            credential_control: AgentCredentialControl::Unsupported,
            model_control: AgentModelControl::Unsupported,
            credential_kinds: Vec::new(),
            model_interfaces: Vec::new(),
            runtime_home_strategy: AgentRuntimeHomeStrategy::None,
            switch_behavior: ProviderSwitchBehavior::Unsupported,
            evidence_state: ProjectionEvidenceState::Unsupported,
            capability_diagnostic_code: Some("agent_projection_unsupported"),
        },
        AgentProviderCapabilityMode::ReplaceableProvider => unreachable!(),
    })
}

fn antigravity_projection_shape() -> CatalogProjectionShape {
    CatalogProjectionShape {
        provider_control: AgentProviderControl::Environment {
            base_url_key: Some("GOOGLE_GEMINI_BASE_URL".to_string()),
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: "GEMINI_API_KEY".to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
        },
        // Antigravity 1.0.0 ignores GEMINI_MODEL at process startup but
        // advertises and applies the standard ACP `model` config option.
        model_control: AgentModelControl::AcpConfigOption {
            aliases: vec!["model".to_string()],
        },
        credential_kinds: vec![AgentCredentialKind::ApiKey],
        model_interfaces: vec![catalog_interface(
            WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
            false,
            true,
        )],
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence_state: ProjectionEvidenceState::Documented,
        capability_diagnostic_code: Some("agent_projection_runtime_verification_required"),
    }
}

fn environment_projection_shape(
    base_url_key: &str,
    secret_env_key: &str,
    model_env_key: &str,
) -> CatalogProjectionShape {
    environment_projection_shape_with_interfaces(
        base_url_key,
        secret_env_key,
        model_env_key,
        vec![catalog_interface(
            WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            false,
            true,
        )],
    )
}

fn environment_projection_shape_with_interfaces(
    base_url_key: &str,
    secret_env_key: &str,
    model_env_key: &str,
    model_interfaces: Vec<AgentModelInterfaceDescriptor>,
) -> CatalogProjectionShape {
    CatalogProjectionShape {
        provider_control: AgentProviderControl::Environment {
            base_url_key: Some(base_url_key.to_string()),
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: secret_env_key.to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
        },
        model_control: AgentModelControl::ProcessEnvironment {
            key: model_env_key.to_string(),
        },
        credential_kinds: vec![AgentCredentialKind::ApiKey],
        model_interfaces,
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence_state: ProjectionEvidenceState::Documented,
        capability_diagnostic_code: Some("agent_projection_runtime_verification_required"),
    }
}

fn overlay_projection_shape(
    strategy: ConfigOverlayStrategy,
    secret_env_key: &str,
    model_interfaces: Vec<AgentModelInterfaceDescriptor>,
) -> CatalogProjectionShape {
    CatalogProjectionShape {
        provider_control: AgentProviderControl::ManagedConfigOverlay {
            strategy: strategy.clone(),
        },
        credential_control: AgentCredentialControl::Environment {
            secret_env_key: secret_env_key.to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
        },
        model_control: AgentModelControl::ManagedConfigOverlay { strategy },
        credential_kinds: vec![AgentCredentialKind::ApiKey],
        model_interfaces,
        runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
        switch_behavior: ProviderSwitchBehavior::RestartAndResume,
        evidence_state: ProjectionEvidenceState::Documented,
        capability_diagnostic_code: Some("agent_projection_runtime_verification_required"),
    }
}

fn catalog_interface(
    wire_protocol_id: &str,
    user_selectable: bool,
    process_scoped: bool,
) -> AgentModelInterfaceDescriptor {
    AgentModelInterfaceDescriptor {
        wire_protocol_id: wire_protocol_id.to_string(),
        sdk_adapter_id: None,
        transport: "https".to_string(),
        integration_kind: AgentModelInterfaceIntegrationKind::Direct,
        user_selectable,
        process_scoped,
    }
}

fn conservative_replaceable_shape(agent_id: &str) -> VibexResult<CatalogProjectionShape> {
    let diagnostic = match agent_id {
        "cline" | "deepseek-harness" | "dimcode" | "minion-code" | "nova" => {
            "agent_projection_auth_boundary_not_runtime_verified"
        }
        "deepagents" => "agent_projection_environment_contract_not_runtime_verified",
        _ => {
            return Err(VibexError::validation(
                "agent_rollout_conservative_contract_missing",
                "replaceable-provider Agent has no explicit projector or conservative contract",
            ));
        }
    };
    Ok(CatalogProjectionShape {
        provider_control: AgentProviderControl::Unverified,
        credential_control: AgentCredentialControl::Unverified,
        model_control: AgentModelControl::Unverified,
        credential_kinds: Vec::new(),
        model_interfaces: Vec::new(),
        runtime_home_strategy: AgentRuntimeHomeStrategy::None,
        switch_behavior: ProviderSwitchBehavior::Unverified,
        evidence_state: ProjectionEvidenceState::Unverified,
        capability_diagnostic_code: Some(diagnostic),
    })
}

fn catalog_manifest_entry(
    id: &str,
    version: &str,
) -> VibexResult<AgentProviderRolloutManifestEntry> {
    let agent_id = AgentId::parse(id)?;
    let mode = mode_for(id);
    let shape = catalog_projection_shape(id, mode)?;
    let (version_policy, _) = catalog_version_compatibility(version, mode, &shape);
    let entry = AgentProviderRolloutManifestEntry {
        agent_id: agent_id.clone(),
        catalog_version: version.to_string(),
        adapter_id: default_acp_adapter_id(&agent_id),
        descriptor_id: descriptor_id(&agent_id),
        descriptor_version: "1".to_string(),
        version_policy,
        capability_mode: mode,
        runtime_home_strategy: shape.runtime_home_strategy,
        switch_behavior: shape.switch_behavior,
        credential_kinds: shape.credential_kinds,
        model_interfaces: shape.model_interfaces,
        evidence_state: shape.evidence_state,
        source_evidence_reference: documented_source(id, version),
        smoke_id: format!("agent-provider-{id}"),
        capability_diagnostic_code: shape.capability_diagnostic_code.map(str::to_string),
    };
    entry.validate()?;
    Ok(entry)
}

/// Returns the complete checked matrix: four builtins plus every catalog id.
pub fn agent_provider_rollout_manifest() -> VibexResult<Vec<AgentProviderRolloutManifestEntry>> {
    let mut entries = Vec::with_capacity(acp_agent_catalog_entries().len() + 4);
    for (id, version) in [
        ("claude", "0.64.2"),
        ("codex", "0.146.0"),
        ("opencode", OPENCODE_LAST_VERIFIED_VERSION),
        ("zcode", crate::provider_projection::ZCODE_ADAPTER_VERSION),
    ] {
        let agent_id = AgentId::parse(id)?;
        entries.push(AgentProviderRolloutManifestEntry {
            agent_id: agent_id.clone(),
            catalog_version: version.to_string(),
            adapter_id: default_acp_adapter_id(&agent_id),
            descriptor_id: AgentProviderProjectionDescriptorId::parse(match id {
                "claude" => "projection_claude_environment_v1",
                "codex" => "projection_codex_stable_home_v1",
                "opencode" => "projection_opencode_inline_provider_v1",
                _ => crate::provider_projection::ZCODE_PROJECTION_DESCRIPTOR_ID,
            })?,
            descriptor_version: "1".to_string(),
            version_policy: if id == "opencode" {
                AgentVersionPolicy::DetectedSemver {
                    requirement: OPENCODE_COMPATIBLE_VERSION_REQUIREMENT.to_string(),
                }
            } else {
                AgentVersionPolicy::Exact
            },
            capability_mode: AgentProviderCapabilityMode::ReplaceableProvider,
            runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            credential_kinds: vec![AgentCredentialKind::ApiKey],
            model_interfaces: if id == "zcode" {
                vec![
                    catalog_interface(WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true, true),
                    catalog_interface(WIRE_PROTOCOL_ANTHROPIC_MESSAGES, true, true),
                ]
            } else {
                Vec::new()
            },
            // The platform descriptor is known, but a runtime probe is the
            // only authority allowed to promote a concrete profile to
            // Verified. The rollout manifest therefore starts conservatively.
            evidence_state: ProjectionEvidenceState::Documented,
            source_evidence_reference: if id == "opencode" {
                "provider-config/opencode-inline-provider-v1".to_string()
            } else {
                format!("provider-config/{id}-environment-v1")
            },
            smoke_id: format!("builtin-provider-{id}"),
            capability_diagnostic_code: Some(
                "agent_projection_runtime_verification_required".to_string(),
            ),
        });
    }
    for entry in acp_agent_catalog_entries() {
        entries.push(catalog_manifest_entry(entry.id, entry.version)?);
    }
    validate_rollout_manifest(&entries)?;
    Ok(entries)
}

pub fn model_provider_configurable_agent_ids() -> VibexResult<BTreeSet<AgentId>> {
    Ok(agent_provider_rollout_manifest()?
        .into_iter()
        .filter(AgentProviderRolloutManifestEntry::supports_model_provider_configuration)
        .map(|entry| entry.agent_id)
        .collect())
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
    let expected_count = acp_agent_catalog_entries().len() + 4;
    if entries.len() != expected_count || actual.len() != entries.len() {
        return Err(VibexError::conflict(
            "agent_rollout_manifest_coverage_invalid",
            format!("rollout manifest must contain exactly {expected_count} unique Agent ids"),
        ));
    }
    let catalog_actual = actual
        .iter()
        .copied()
        .filter(|id| !matches!(*id, "claude" | "codex" | "opencode" | "zcode"))
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
        let shape = catalog_projection_shape(entry.id, mode)?;
        let (_, compatibility) = catalog_version_compatibility(entry.version, mode, &shape);
        result.push(AgentProviderProjectionDescriptor {
            id: descriptor_id(&agent_id),
            descriptor_version: "1".to_string(),
            route: crate::AgentRuntimeRouteKey {
                agent_id: agent_id.clone(),
                transport_kind: crate::TransportKind::Acp,
                adapter_id: default_acp_adapter_id(&agent_id),
            },
            compatibility,
            provider_control: shape.provider_control,
            credential_control: shape.credential_control,
            model_control: shape.model_control,
            credential_kinds: shape.credential_kinds,
            model_interfaces: shape.model_interfaces,
            runtime_home_strategy: shape.runtime_home_strategy,
            switch_behavior: shape.switch_behavior,
            evidence: ProjectionEvidenceReference {
                state: shape.evidence_state,
                source_reference: Some(documented_source(entry.id, entry.version)),
                runtime_reference: None,
                diagnostic_code: shape.capability_diagnostic_code.map(str::to_string),
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
        let version_identity_verified = match self.descriptor_match {
            ProjectionDescriptorMatch::Exact => true,
            ProjectionDescriptorMatch::SemverRange => [
                self.agent_version.as_deref(),
                self.adapter_version.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|version| Version::parse(version).is_ok()),
            ProjectionDescriptorMatch::Conservative => false,
        };
        version_identity_verified
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
        assert_eq!(manifest.len(), 37);
        assert!(manifest.iter().any(|entry| entry.agent_id.as_str() == "pi"));
        let antigravity = manifest
            .iter()
            .find(|entry| entry.agent_id.as_str() == "antigravity")
            .unwrap();
        assert_eq!(
            antigravity.capability_mode,
            AgentProviderCapabilityMode::ReplaceableProvider
        );
        assert_eq!(
            antigravity.runtime_home_strategy,
            AgentRuntimeHomeStrategy::VibexPrivate
        );
        assert_eq!(
            antigravity.switch_behavior,
            ProviderSwitchBehavior::RestartAndResume
        );
        assert!(
            manifest
                .iter()
                .all(|entry| !entry.source_evidence_reference.is_empty())
        );
        let opencode = manifest
            .iter()
            .find(|entry| entry.agent_id.as_str() == "opencode")
            .unwrap();
        assert_eq!(opencode.catalog_version, OPENCODE_LAST_VERIFIED_VERSION);
        assert_eq!(
            opencode.version_policy,
            AgentVersionPolicy::DetectedSemver {
                requirement: OPENCODE_COMPATIBLE_VERSION_REQUIREMENT.to_string()
            }
        );
        for (agent_id, requirement) in [
            ("codebuddy-code", ">=2.109.0"),
            ("glm-acp-agent", ">=1.1.4"),
        ] {
            assert_eq!(
                manifest
                    .iter()
                    .find(|entry| entry.agent_id.as_str() == agent_id)
                    .unwrap()
                    .version_policy,
                AgentVersionPolicy::DetectedSemver {
                    requirement: requirement.to_string(),
                }
            );
        }

        let mut unsafe_conservative = manifest
            .into_iter()
            .find(|entry| entry.evidence_state == ProjectionEvidenceState::Unverified)
            .unwrap();
        unsafe_conservative.runtime_home_strategy = AgentRuntimeHomeStrategy::VibexPrivate;
        assert_eq!(
            unsafe_conservative.validate().unwrap_err().code,
            "agent_rollout_manifest_conservative_shape_invalid"
        );
    }

    #[test]
    fn version_policy_keeps_legacy_unit_json_and_adds_range_payload() {
        assert_eq!(
            serde_json::from_value::<AgentVersionPolicy>(serde_json::json!({
                "kind": "exact"
            }))
            .unwrap(),
            AgentVersionPolicy::Exact
        );
        assert_eq!(
            serde_json::from_value::<AgentVersionPolicy>(serde_json::json!({
                "kind": "detected_manual"
            }))
            .unwrap(),
            AgentVersionPolicy::DetectedManual
        );
        let range = AgentVersionPolicy::DetectedSemver {
            requirement: OPENCODE_COMPATIBLE_VERSION_REQUIREMENT.to_string(),
        };
        assert_eq!(
            serde_json::to_value(range).unwrap(),
            serde_json::json!({
                "kind": "detected_semver",
                "requirement": OPENCODE_COMPATIBLE_VERSION_REQUIREMENT,
            })
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
    fn semver_range_evidence_requires_a_detected_version_identity() {
        let mut evidence = AgentRuntimeProbeEvidence {
            schema_version: AGENT_RUNTIME_PROBE_SCHEMA_VERSION,
            agent_id: AgentId::parse("fixture").unwrap(),
            agent_version: None,
            adapter_id: AcpAdapterId::parse("fixture-acp").unwrap(),
            adapter_version: None,
            descriptor_id: AgentProviderProjectionDescriptorId::parse("projection_fixture_v1")
                .unwrap(),
            descriptor_version: "1".to_string(),
            descriptor_match: ProjectionDescriptorMatch::SemverRange,
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
        assert!(!evidence.provider_projection_verified());
        evidence.agent_version = Some("1.18.11".to_string());
        assert!(evidence.provider_projection_verified());
        evidence.agent_version = None;
        evidence.adapter_version = Some("1.1.13".to_string());
        assert!(evidence.provider_projection_verified());
        evidence.adapter_version = Some("not-semver".to_string());
        assert!(!evidence.provider_projection_verified());
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
    fn catalog_projection_contracts_are_explicit_and_conservative() {
        let descriptors = catalog_projection_descriptors().unwrap();
        let codebuddy = descriptors
            .iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "codebuddy-code")
            .unwrap();
        assert_eq!(
            codebuddy.provider_control,
            AgentProviderControl::Environment {
                base_url_key: Some("CODEBUDDY_BASE_URL".to_string())
            }
        );
        assert_eq!(
            codebuddy.credential_control,
            AgentCredentialControl::Environment {
                secret_env_key: "CODEBUDDY_API_KEY".to_string(),
                accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
            }
        );
        assert_eq!(
            codebuddy.model_control,
            AgentModelControl::ProcessEnvironment {
                key: "CODEBUDDY_MODEL".to_string()
            }
        );
        assert_eq!(
            codebuddy.compatibility,
            AgentVersionCompatibility::SemverRange {
                adapter_range: None,
                agent_range: Some(">=2.109.0".to_string()),
                runtime_dependency_ranges: BTreeMap::new(),
            }
        );
        let glm = descriptors
            .iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "glm-acp-agent")
            .unwrap();
        assert_eq!(
            glm.provider_control,
            AgentProviderControl::Environment {
                base_url_key: Some("ACP_GLM_BASE_URL".to_string())
            }
        );
        assert_eq!(
            glm.credential_control,
            AgentCredentialControl::Environment {
                secret_env_key: "Z_AI_API_KEY".to_string(),
                accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
            }
        );
        assert_eq!(
            glm.model_control,
            AgentModelControl::ProcessEnvironment {
                key: "ACP_GLM_MODEL".to_string()
            }
        );
        assert_eq!(
            glm.compatibility,
            AgentVersionCompatibility::SemverRange {
                adapter_range: None,
                agent_range: Some(">=1.1.4".to_string()),
                runtime_dependency_ranges: BTreeMap::new(),
            }
        );
        let typed_projectors = [
            "antigravity",
            "copilot",
            "codewhale",
            "crow-cli",
            "dirac",
            "factory-droid",
            "gemini",
            "goose",
            "grok",
            "hermes",
            "kilo",
            "kimi",
            "mistral-vibe",
            "poolside",
            "pi",
            "qwen-code",
            "stakpak",
            "vtcode",
        ];
        let blocked_projectors = [
            "cline",
            "deepagents",
            "deepseek-harness",
            "dimcode",
            "minion-code",
            "nova",
        ];
        assert_eq!(typed_projectors.len(), 18);
        assert_eq!(blocked_projectors.len(), 6);

        for descriptor in descriptors
            .iter()
            .filter(|descriptor| typed_projectors.contains(&descriptor.route.agent_id.as_str()))
        {
            let catalog_version = acp_agent_catalog_entries()
                .iter()
                .find(|entry| entry.id == descriptor.route.agent_id.as_str())
                .expect("typed projector is present in the ACP catalog")
                .version;
            let expected_requirement = format!(">={catalog_version}");
            assert!(
                matches!(
                    descriptor.compatibility,
                    AgentVersionCompatibility::SemverRange {
                        adapter_range: None,
                        agent_range: Some(ref range),
                        runtime_dependency_ranges: ref ranges,
                    } if range == &expected_requirement && ranges.is_empty()
                ),
                "{} must accept its catalog version and newer versions",
                descriptor.route.agent_id
            );
        }

        for descriptor in descriptors {
            let mode = mode_for(descriptor.route.agent_id.as_str());
            if mode != AgentProviderCapabilityMode::ReplaceableProvider {
                continue;
            }
            if matches!(
                descriptor.route.agent_id.as_str(),
                "codebuddy-code" | "glm-acp-agent"
            ) || typed_projectors.contains(&descriptor.route.agent_id.as_str())
            {
                assert!(matches!(
                    descriptor.provider_control,
                    AgentProviderControl::Environment { .. }
                        | AgentProviderControl::ManagedConfigOverlay { .. }
                ));
                assert!(matches!(
                    descriptor.credential_control,
                    AgentCredentialControl::Environment { .. }
                ));
                match (descriptor.provider_control, descriptor.model_control) {
                    (
                        AgentProviderControl::Environment { .. },
                        AgentModelControl::AcpConfigOption { .. },
                    ) if descriptor.route.agent_id.as_str() == "antigravity" => {}
                    (
                        AgentProviderControl::Environment { .. },
                        AgentModelControl::ProcessEnvironment { .. },
                    )
                    | (
                        AgentProviderControl::ManagedConfigOverlay { .. },
                        AgentModelControl::ManagedConfigOverlay { .. },
                    ) => {}
                    (provider_control, model_control) => panic!(
                        "provider/model projection controls must share a typed boundary: provider={provider_control:?} model={model_control:?}"
                    ),
                }
                assert_eq!(
                    descriptor.evidence.state,
                    ProjectionEvidenceState::Documented
                );
                assert_eq!(
                    descriptor.runtime_home_strategy,
                    AgentRuntimeHomeStrategy::VibexPrivate
                );
                assert_eq!(
                    descriptor.switch_behavior,
                    ProviderSwitchBehavior::RestartAndResume
                );
            } else {
                assert!(blocked_projectors.contains(&descriptor.route.agent_id.as_str()));
                assert_eq!(
                    descriptor.provider_control,
                    AgentProviderControl::Unverified
                );
                assert_eq!(
                    descriptor.credential_control,
                    AgentCredentialControl::Unverified
                );
                assert_eq!(descriptor.model_control, AgentModelControl::Unverified);
                assert_eq!(
                    descriptor.evidence.state,
                    ProjectionEvidenceState::Unverified
                );
                assert!(descriptor.evidence.diagnostic_code.is_some());
                assert_eq!(
                    descriptor.switch_behavior,
                    ProviderSwitchBehavior::Unverified
                );
            }
        }
    }

    #[test]
    fn model_provider_configuration_support_follows_the_rollout_contract() {
        let supported = model_provider_configurable_agent_ids().unwrap();

        for agent_id in [
            "claude",
            "codex",
            "opencode",
            "antigravity",
            "gemini",
            "glm-acp-agent",
            "copilot",
            "qwen-code",
            "zcode",
        ] {
            assert!(
                supported.contains(&AgentId::parse(agent_id).unwrap()),
                "{agent_id} has a typed model-provider projector"
            );
        }
        let unsupported_agent_id = "cline";
        assert!(
            !supported.contains(&AgentId::parse(unsupported_agent_id).unwrap()),
            "{unsupported_agent_id} must not expose model-provider configuration"
        );
    }

    #[test]
    fn catalog_agent_protocol_contracts_are_agent_specific() {
        let descriptors = catalog_projection_descriptors().unwrap();
        let protocols = |agent_id: &str| {
            descriptors
                .iter()
                .find(|descriptor| descriptor.route.agent_id.as_str() == agent_id)
                .unwrap()
                .model_interfaces
                .iter()
                .map(|interface| interface.wire_protocol_id.as_str())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            protocols("antigravity"),
            vec![WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI]
        );
        assert_eq!(
            protocols("gemini"),
            vec![WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI]
        );
        assert_eq!(protocols("grok"), vec![WIRE_PROTOCOL_OPENAI_RESPONSES]);
        assert_eq!(
            protocols("hermes"),
            vec![
                WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
                WIRE_PROTOCOL_OPENAI_RESPONSES,
                WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE,
            ]
        );

        let antigravity = descriptors
            .iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "antigravity")
            .unwrap();
        assert_eq!(
            antigravity.provider_control,
            AgentProviderControl::Environment {
                base_url_key: Some("GOOGLE_GEMINI_BASE_URL".to_string())
            }
        );
        assert_eq!(
            antigravity.credential_control,
            AgentCredentialControl::Environment {
                secret_env_key: "GEMINI_API_KEY".to_string(),
                accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
            }
        );
        assert_eq!(
            antigravity.model_control,
            AgentModelControl::AcpConfigOption {
                aliases: vec!["model".to_string()]
            }
        );

        let gemini = descriptors
            .iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "gemini")
            .unwrap();
        assert_eq!(
            gemini.provider_control,
            AgentProviderControl::Environment {
                base_url_key: Some("GOOGLE_GEMINI_BASE_URL".to_string())
            }
        );
        assert_eq!(
            gemini.credential_control,
            AgentCredentialControl::Environment {
                secret_env_key: "GEMINI_API_KEY".to_string(),
                accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
            }
        );
        assert_eq!(
            gemini.model_control,
            AgentModelControl::ProcessEnvironment {
                key: "GEMINI_MODEL".to_string()
            }
        );
    }

    #[test]
    fn conservative_descriptor_requires_a_diagnostic_and_fail_closed_shape() {
        let mut descriptor = catalog_projection_descriptors()
            .unwrap()
            .into_iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "cline")
            .unwrap();
        descriptor.evidence.diagnostic_code = None;
        assert_eq!(
            descriptor.validate().unwrap_err().code,
            "agent_projection_unverified_contract_incomplete"
        );

        descriptor.evidence.diagnostic_code =
            Some("agent_projection_auth_boundary_not_runtime_verified".to_string());
        descriptor
            .credential_kinds
            .push(AgentCredentialKind::ApiKey);
        assert_eq!(
            descriptor.validate().unwrap_err().code,
            "agent_projection_unverified_contract_incomplete"
        );

        let mut documented_projector = catalog_projection_descriptors()
            .unwrap()
            .into_iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "codebuddy-code")
            .unwrap();
        documented_projector.evidence.state = ProjectionEvidenceState::Unverified;
        assert_eq!(
            documented_projector.validate().unwrap_err().code,
            "agent_projection_unverified_contract_incomplete"
        );
    }
}

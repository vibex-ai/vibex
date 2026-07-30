//! ACP session restore planning and failure classification.
//!
//! This module is deliberately provider-neutral at its public boundary. It
//! decides which restore operations may be probed for one exact process
//! generation; the runtime supplies the wire executor and owns attachment
//! fences.

use std::collections::BTreeMap;

use vibex_core::{
    AgentSessionRestoreAttempt, AgentSessionRestoreCompatibility,
    AgentSessionRestoreCompatibilityKey, AgentSessionRestoreMethod, AgentSessionRestoreOutcome,
    AgentSessionRestoreResult, RestoreIncompatibilityReason, VibexError,
};

use crate::protocol::{AcpOperation, AcpOperationStability, AcpWireEncoding, CapabilitySource};
use crate::registry::{CapabilitySupport, RestorePolicy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCapabilityEvidence {
    pub support: CapabilitySupport,
    pub source: CapabilitySource,
    pub encoding: AcpWireEncoding,
    pub stability: AcpOperationStability,
    pub compatibility_identity: String,
    pub activation_generation: i64,
}

impl RestoreCapabilityEvidence {
    pub fn supported_for(&self, compatibility_identity: &str, generation: i64) -> bool {
        self.support == CapabilitySupport::Supported
            && self.compatibility_identity == compatibility_identity
            && self.activation_generation == generation
            && matches!(
                self.source,
                CapabilitySource::NegotiatedRuntime | CapabilitySource::ObservedRuntime
            )
    }

    pub fn unsupported_for(&self, compatibility_identity: &str, generation: i64) -> bool {
        self.support == CapabilitySupport::Unsupported
            && self.compatibility_identity == compatibility_identity
            && self.activation_generation == generation
    }
}

pub type RestoreCapabilityMap = BTreeMap<AgentSessionRestoreMethod, RestoreCapabilityEvidence>;

fn capability_for(
    capabilities: &RestoreCapabilityMap,
    method: AgentSessionRestoreMethod,
) -> Option<&RestoreCapabilityEvidence> {
    capabilities.get(&method)
}

/// Resolves exact identities and current-generation capability evidence. Static
/// descriptors and unknown values can only produce `ProbeRequired`.
pub fn resolve_restore_compatibility(
    source: &AgentSessionRestoreCompatibilityKey,
    target: &AgentSessionRestoreCompatibilityKey,
    capabilities: &RestoreCapabilityMap,
    generation: i64,
) -> AgentSessionRestoreCompatibility {
    if source.agent_id != target.agent_id {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::AgentMismatch,
        };
    }
    if source.native_state_home_id != target.native_state_home_id {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::NativeStateHomeMismatch,
        };
    }
    if source.adapter_compatibility_identity != target.adapter_compatibility_identity {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::AdapterCompatibilityMismatch,
        };
    }
    if source.agent_state_format_identity != target.agent_state_format_identity {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::AgentStateFormatMismatch,
        };
    }
    if source.provider_resume_identity != target.provider_resume_identity {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::ProviderResumeIdentityMismatch,
        };
    }
    if source.workspace_identity != target.workspace_identity {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::WorkspaceMismatch,
        };
    }
    if source.native_session_id.is_empty() {
        return AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::MissingIdentity,
        };
    }

    let identity = target.adapter_compatibility_identity.as_str();
    let mut negotiated_supported = Vec::new();
    let mut probe_methods = Vec::new();
    let mut explicitly_unsupported = 0;
    for method in [
        AgentSessionRestoreMethod::Resume,
        AgentSessionRestoreMethod::Load,
    ] {
        match capability_for(capabilities, method) {
            Some(evidence) if evidence.supported_for(identity, generation) => {
                negotiated_supported.push(method);
            }
            Some(evidence) if evidence.unsupported_for(identity, generation) => {
                explicitly_unsupported += 1;
            }
            Some(_) | None => probe_methods.push(method),
        }
    }
    if !negotiated_supported.is_empty() {
        return AgentSessionRestoreCompatibility::Compatible;
    }
    if !probe_methods.is_empty() {
        return AgentSessionRestoreCompatibility::ProbeRequired {
            allowed_methods: probe_methods,
        };
    }
    if explicitly_unsupported == 2 {
        AgentSessionRestoreCompatibility::Incompatible {
            reason: RestoreIncompatibilityReason::CapabilityUnavailable,
        }
    } else {
        AgentSessionRestoreCompatibility::ProbeRequired {
            allowed_methods: vec![
                AgentSessionRestoreMethod::Resume,
                AgentSessionRestoreMethod::Load,
            ],
        }
    }
}

pub fn operation_for_method(method: AgentSessionRestoreMethod) -> AcpOperation {
    match method {
        AgentSessionRestoreMethod::Resume => AcpOperation::SessionResume,
        AgentSessionRestoreMethod::Load => AcpOperation::SessionLoad,
        AgentSessionRestoreMethod::New => AcpOperation::SessionNew,
    }
}

pub fn encoding_name(encoding: AcpWireEncoding) -> &'static str {
    match encoding {
        AcpWireEncoding::Typed => "typed",
        AcpWireEncoding::VersionedRaw => "versioned_raw",
        AcpWireEncoding::ExtensionCodec => "extension_codec",
    }
}

/// A deterministic candidate order. `new` is never included unless explicitly
/// enabled by the caller; a load/resume miss remains observable as NotFound.
pub fn restore_methods(policy: RestorePolicy) -> Vec<AgentSessionRestoreMethod> {
    match policy {
        RestorePolicy::LoadThenNew => vec![AgentSessionRestoreMethod::Load],
        RestorePolicy::ResumeThenLoadThenNew => vec![
            AgentSessionRestoreMethod::Resume,
            AgentSessionRestoreMethod::Load,
        ],
    }
}

pub fn classify_restore_error(error: &VibexError) -> AgentSessionRestoreOutcome {
    let code = error.code.to_ascii_lowercase();
    let diagnostics = error
        .diagnostics
        .iter()
        .map(|entry| entry.value.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!("{code} {diagnostics}");
    if text.contains("method_not_found")
        || text.contains("unsupported")
        || matches!(error.category, vibex_core::ErrorCategory::Capability)
    {
        AgentSessionRestoreOutcome::Unsupported
    } else if text.contains("not_found")
        || text.contains("not found")
        || text.contains("session_missing")
        || text.contains("unknown_session")
        || text.contains("unknown session")
    {
        AgentSessionRestoreOutcome::NotFound
    } else if text.contains("auth")
        || text.contains("permission")
        || matches!(error.category, vibex_core::ErrorCategory::Permission)
    {
        AgentSessionRestoreOutcome::AuthenticationRequired
    } else if text.contains("timeout")
        || text.contains("transport")
        || text.contains("process_exited")
        || matches!(error.category, vibex_core::ErrorCategory::Process)
    {
        AgentSessionRestoreOutcome::TransientFailure
    } else {
        AgentSessionRestoreOutcome::FatalFailure
    }
}

pub fn result_for_failure(
    compatibility: AgentSessionRestoreCompatibility,
    method: AgentSessionRestoreMethod,
    encoding: Option<AcpWireEncoding>,
    source: Option<CapabilitySource>,
    error: &VibexError,
    activation_generation: i64,
) -> AgentSessionRestoreResult {
    let outcome = classify_restore_error(error);
    AgentSessionRestoreResult {
        outcome,
        compatibility,
        attempts: vec![AgentSessionRestoreAttempt {
            method,
            encoding: encoding.map(encoding_name).map(str::to_string),
            capability_source: source.map(|value| format!("{value:?}").to_ascii_lowercase()),
            outcome,
            error_code: Some(error.code.chars().take(80).collect()),
        }],
        method: Some(method),
        encoding: encoding.map(encoding_name).map(str::to_string),
        capability_source: source.map(|value| format!("{value:?}").to_ascii_lowercase()),
        error_code: Some(error.code.chars().take(80).collect()),
        activation_generation,
        fresh_allowed: matches!(
            outcome,
            AgentSessionRestoreOutcome::NotFound | AgentSessionRestoreOutcome::Unsupported
        ),
    }
}

pub fn result_for_success(
    compatibility: AgentSessionRestoreCompatibility,
    method: AgentSessionRestoreMethod,
    evidence: &RestoreCapabilityEvidence,
    activation_generation: i64,
) -> AgentSessionRestoreResult {
    let outcome = match method {
        AgentSessionRestoreMethod::Resume => AgentSessionRestoreOutcome::Resumed,
        AgentSessionRestoreMethod::Load => AgentSessionRestoreOutcome::Loaded,
        AgentSessionRestoreMethod::New => AgentSessionRestoreOutcome::FatalFailure,
    };
    let encoding = encoding_name(evidence.encoding).to_string();
    let capability_source = format!("{:?}", evidence.source).to_ascii_lowercase();
    AgentSessionRestoreResult {
        outcome,
        compatibility,
        attempts: vec![AgentSessionRestoreAttempt {
            method,
            encoding: Some(encoding.clone()),
            capability_source: Some(capability_source.clone()),
            outcome,
            error_code: None,
        }],
        method: Some(method),
        encoding: Some(encoding),
        capability_source: Some(capability_source),
        error_code: None,
        activation_generation,
        fresh_allowed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentId, NativeStateHomeId};

    fn key(workspace: &str) -> AgentSessionRestoreCompatibilityKey {
        AgentSessionRestoreCompatibilityKey::new(
            AgentId::parse("codex").unwrap(),
            "native-1",
            NativeStateHomeId::parse("statehome_test").unwrap(),
            "codex-acp@v1",
            Some("state-v1".to_string()),
            "resume-v1",
            workspace,
        )
        .unwrap()
    }

    fn evidence(
        method: AgentSessionRestoreMethod,
        support: CapabilitySupport,
    ) -> (AgentSessionRestoreMethod, RestoreCapabilityEvidence) {
        (
            method,
            RestoreCapabilityEvidence {
                support,
                source: CapabilitySource::NegotiatedRuntime,
                encoding: if method == AgentSessionRestoreMethod::Load {
                    AcpWireEncoding::Typed
                } else {
                    AcpWireEncoding::VersionedRaw
                },
                stability: AcpOperationStability::CapabilityGated,
                compatibility_identity: "codex-acp@v1".to_string(),
                activation_generation: 3,
            },
        )
    }

    #[test]
    fn resolver_requires_exact_identity_and_generation_evidence() {
        let source = key("workspace:/repo");
        let target = key("workspace:/repo");
        let compatible = BTreeMap::from([evidence(
            AgentSessionRestoreMethod::Load,
            CapabilitySupport::Supported,
        )]);
        assert_eq!(
            resolve_restore_compatibility(&source, &target, &compatible, 3),
            AgentSessionRestoreCompatibility::Compatible
        );
        assert!(matches!(
            resolve_restore_compatibility(&source, &key("workspace:/other"), &compatible, 3),
            AgentSessionRestoreCompatibility::Incompatible {
                reason: RestoreIncompatibilityReason::WorkspaceMismatch
            }
        ));
        assert!(matches!(
            resolve_restore_compatibility(&source, &target, &compatible, 4),
            AgentSessionRestoreCompatibility::ProbeRequired { .. }
        ));
    }

    #[test]
    fn resolver_reports_each_stable_incompatibility_reason() {
        let source = key("workspace:/repo");
        let compatible = BTreeMap::from([evidence(
            AgentSessionRestoreMethod::Load,
            CapabilitySupport::Supported,
        )]);
        let cases = [
            (
                {
                    let mut target = source.clone();
                    target.agent_id = AgentId::parse("claude").unwrap();
                    target
                },
                RestoreIncompatibilityReason::AgentMismatch,
            ),
            (
                {
                    let mut target = source.clone();
                    target.native_state_home_id =
                        NativeStateHomeId::parse("statehome_other").unwrap();
                    target
                },
                RestoreIncompatibilityReason::NativeStateHomeMismatch,
            ),
            (
                {
                    let mut target = source.clone();
                    target.adapter_compatibility_identity = "codex-acp@v2".to_string();
                    target
                },
                RestoreIncompatibilityReason::AdapterCompatibilityMismatch,
            ),
            (
                {
                    let mut target = source.clone();
                    target.agent_state_format_identity = Some("state-v2".to_string());
                    target
                },
                RestoreIncompatibilityReason::AgentStateFormatMismatch,
            ),
            (
                {
                    let mut target = source.clone();
                    target.provider_resume_identity = "resume-v2".to_string();
                    target
                },
                RestoreIncompatibilityReason::ProviderResumeIdentityMismatch,
            ),
            (
                key("workspace:/other"),
                RestoreIncompatibilityReason::WorkspaceMismatch,
            ),
        ];
        for (target, reason) in cases {
            assert_eq!(
                resolve_restore_compatibility(&source, &target, &compatible, 3),
                AgentSessionRestoreCompatibility::Incompatible { reason }
            );
        }

        let mut missing = source.clone();
        missing.native_session_id.clear();
        assert_eq!(
            resolve_restore_compatibility(&missing, &source, &compatible, 3),
            AgentSessionRestoreCompatibility::Incompatible {
                reason: RestoreIncompatibilityReason::MissingIdentity
            }
        );

        let unsupported = BTreeMap::from([
            evidence(
                AgentSessionRestoreMethod::Resume,
                CapabilitySupport::Unsupported,
            ),
            evidence(
                AgentSessionRestoreMethod::Load,
                CapabilitySupport::Unsupported,
            ),
        ]);
        assert_eq!(
            resolve_restore_compatibility(&source, &source, &unsupported, 3),
            AgentSessionRestoreCompatibility::Incompatible {
                reason: RestoreIncompatibilityReason::CapabilityUnavailable
            }
        );
    }

    #[test]
    fn unknown_and_static_capabilities_require_probe() {
        let source = key("workspace:/repo");
        let target = key("workspace:/repo");
        let static_evidence = RestoreCapabilityEvidence {
            support: CapabilitySupport::Supported,
            source: CapabilitySource::VersionedRegistry,
            encoding: AcpWireEncoding::VersionedRaw,
            stability: AcpOperationStability::VersionedUnstable,
            compatibility_identity: "codex-acp@v1".to_string(),
            activation_generation: 3,
        };
        let capabilities = BTreeMap::from([(AgentSessionRestoreMethod::Resume, static_evidence)]);
        assert!(matches!(
            resolve_restore_compatibility(&source, &target, &capabilities, 3),
            AgentSessionRestoreCompatibility::ProbeRequired { .. }
        ));
    }

    #[test]
    fn error_classes_do_not_collapse_fatal_or_auth_into_fresh() {
        let auth = VibexError::provider("acp_auth_required", "authentication required");
        assert_eq!(
            classify_restore_error(&auth),
            AgentSessionRestoreOutcome::AuthenticationRequired
        );
        assert!(
            !result_for_failure(
                AgentSessionRestoreCompatibility::Compatible,
                AgentSessionRestoreMethod::Load,
                None,
                None,
                &auth,
                1
            )
            .fresh_allowed
        );
        let timeout = VibexError::process("acp_request_timeout", "timed out");
        assert_eq!(
            classify_restore_error(&timeout),
            AgentSessionRestoreOutcome::TransientFailure
        );
        let missing = VibexError::provider("session_not_found", "not found");
        assert!(
            result_for_failure(
                AgentSessionRestoreCompatibility::Compatible,
                AgentSessionRestoreMethod::Load,
                None,
                None,
                &missing,
                1
            )
            .fresh_allowed
        );
        let structured_missing =
            VibexError::provider("acp_rpc_error", "ACP agent returned a JSON-RPC error")
                .with_diagnostic("protocolErrorKind", "resource_not_found")
                .with_diagnostic("rpcMessage", "Internal error");
        assert_eq!(
            classify_restore_error(&structured_missing),
            AgentSessionRestoreOutcome::NotFound
        );
    }
}

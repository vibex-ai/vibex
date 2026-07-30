//! Domain model for the unified ACP agent runtime and logical-session hot switch.
//!
//! Covers plan §3 (terminology), §10.2 (session runtime config state), §17 (two-phase
//! switch state machine), §20 (data model) and §21.2 (session runtime selection).
//! These types are persistence/domain types only; no process management lives here.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agent_config::AgentId;
use crate::error::{VibexError, VibexResult};
use crate::ids::{
    EventId, NativeStateHomeId, ProviderProfileId, RuntimeBindingId, RuntimeClientId,
    RuntimeLeaseId, RuntimeProcessId, RuntimeStreamId, RuntimeSwitchId, VibexSessionId,
};
use crate::permission::PermissionRequest;
use crate::provider::ProviderSessionConfigValue;

/// §3.3 — the target architecture only keeps ACP as a transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Acp,
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acp => f.write_str("acp"),
        }
    }
}

/// Identifier of an ACP adapter (for example `claude-code-acp`). Free-form,
/// non-empty, human-assigned; unlike surrogate ids it carries no generated prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AcpAdapterId(String);

impl AcpAdapterId {
    pub fn parse(value: impl Into<String>) -> VibexResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(VibexError::validation(
                "invalid_acp_adapter_id",
                "acp adapter id must not be empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for AcpAdapterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AcpAdapterId {
    type Err = VibexError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for AcpAdapterId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AcpAdapterId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// §4.1 — route key used to resolve an agent to a runtime route.
/// Usable as a map key (`Eq + Hash + Ord`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeRouteKey {
    pub agent_id: AgentId,
    pub transport_kind: TransportKind,
    pub adapter_id: AcpAdapterId,
}

/// §20.2 — lifecycle state of a runtime binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingState {
    Preparing,
    Current,
    Inactive,
    Failed,
}

/// A generic session option keeps the user intent and the last value confirmed
/// by the Agent separate.  `ProviderSessionConfigState` is the discovery
/// snapshot returned by an Agent; this type is Vibex-owned mutable state.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeConfigValueState {
    pub preferred: Option<ProviderSessionConfigValue>,
    pub effective: Option<ProviderSessionConfigValue>,
}

// P1 rows used the discovery value shape directly (`{value,label}`).  Accept
// that shape while reading old bindings, but always write the explicit
// preferred/effective projection going forward.
impl<'de> Deserialize<'de> for SessionRuntimeConfigValueState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Dual {
            #[serde(default)]
            preferred: Option<ProviderSessionConfigValue>,
            #[serde(default)]
            effective: Option<ProviderSessionConfigValue>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Dual(Dual),
            Legacy(ProviderSessionConfigValue),
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Dual(Dual {
                preferred,
                effective,
            }) => Self {
                preferred,
                effective,
            },
            Wire::Legacy(value) => Self {
                preferred: Some(value.clone()),
                effective: Some(value),
            },
        })
    }
}

/// §10.2 — preferred vs. effective session-scoped runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeConfigState {
    #[serde(default)]
    pub preferred_model: Option<String>,
    #[serde(default)]
    pub effective_model: Option<String>,
    #[serde(default)]
    pub preferred_mode: Option<String>,
    #[serde(default)]
    pub effective_mode: Option<String>,
    #[serde(default)]
    pub preferred_reasoning_effort: Option<String>,
    #[serde(default)]
    pub effective_reasoning_effort: Option<String>,
    /// Reserved model/mode/effort keys are represented by the explicit fields
    /// above.  This map contains generic options such as approval/sandbox.
    #[serde(default)]
    pub config_values: BTreeMap<String, SessionRuntimeConfigValueState>,
    #[serde(default)]
    pub state_revision: i64,
    #[serde(default)]
    pub applied_activation_generation: Option<i64>,
}

impl SessionRuntimeConfigState {
    /// Returns whether every preferred value currently has the same effective
    /// value. Empty state is considered converged.
    pub fn is_converged(&self) -> bool {
        self.preferred_model == self.effective_model
            && self.preferred_mode == self.effective_mode
            && self.preferred_reasoning_effort == self.effective_reasoning_effort
            && self.config_values.values().all(|value| {
                value.preferred.as_ref().map(|v| &v.value)
                    == value.effective.as_ref().map(|v| &v.value)
            })
    }

    /// The generation marker is meaningful only for a fully converged state.
    pub fn mark_generation_if_converged(&mut self, generation: i64) {
        self.applied_activation_generation = self.is_converged().then_some(generation);
    }

    /// Returns whether the preferred projection is fully confirmed for one
    /// attachment generation.
    pub fn is_applied_to_generation(&self, generation: i64) -> bool {
        self.applied_activation_generation == Some(generation) && self.is_converged()
    }
}

/// A mutation patch uses explicit clear flags instead of nested `Option`s so
/// wire consumers can distinguish omission from clearing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeConfigPatch {
    pub model_id: Option<String>,
    pub clear_model: bool,
    pub reasoning_effort: Option<String>,
    pub clear_reasoning_effort: bool,
    pub mode_id: Option<String>,
    pub clear_mode: bool,
    pub config_values: BTreeMap<String, String>,
    #[serde(default)]
    pub clear_config_keys: Vec<String>,
}

/// Provider-neutral request used by the internal runtime-config API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeConfigMutationRequest {
    pub session_id: VibexSessionId,
    pub expected_revision: i64,
    pub expected_binding_id: RuntimeBindingId,
    pub expected_activation_generation: i64,
    pub patch: SessionRuntimeConfigPatch,
}

/// Stable outcome for one requested field.  The operation/encoding strings
/// are bounded, provider-neutral summaries; native option payloads are never
/// included here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeConfigApplyStatus {
    Applied,
    NoOp,
    RestartRequired,
    Unavailable,
    Busy,
    Failed,
    StaleConfirmation,
    ReconciliationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeConfigFieldOutcome {
    pub key: String,
    pub status: SessionRuntimeConfigApplyStatus,
    pub operation: Option<String>,
    pub encoding: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeConfigMutationResult {
    pub state: SessionRuntimeConfigState,
    pub outcomes: Vec<SessionRuntimeConfigFieldOutcome>,
}

const RESTORE_IDENTITY_LIMIT: usize = 512;

fn normalize_restore_identity(
    value: impl Into<String>,
    field: &'static str,
) -> VibexResult<String> {
    let value = value.into();
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(VibexError::validation(
            "restore_compatibility_identity_empty",
            format!("{field} must not be empty"),
        ));
    }
    if normalized.len() > RESTORE_IDENTITY_LIMIT {
        return Err(VibexError::validation(
            "restore_compatibility_identity_too_long",
            format!("{field} exceeds the bounded restore identity length"),
        ));
    }
    Ok(normalized.to_string())
}

/// Exact identity used to decide whether a native session may be restored.
/// Native ids are serialized for durable matching, but are deliberately hidden
/// from Debug output and diagnostics.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRestoreCompatibilityKey {
    pub agent_id: AgentId,
    pub native_session_id: String,
    pub native_state_home_id: NativeStateHomeId,
    pub adapter_compatibility_identity: String,
    pub agent_state_format_identity: Option<String>,
    pub provider_resume_identity: String,
    pub workspace_identity: String,
}

impl fmt::Debug for AgentSessionRestoreCompatibilityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentSessionRestoreCompatibilityKey")
            .field("agent_id", &self.agent_id)
            .field("native_session_id", &"<redacted>")
            .field("native_state_home_id", &self.native_state_home_id)
            .field("adapter_compatibility_identity", &"<redacted>")
            .field(
                "agent_state_format_identity",
                &self
                    .agent_state_format_identity
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field("provider_resume_identity", &"<redacted>")
            .field("workspace_identity", &"<redacted>")
            .finish()
    }
}

impl AgentSessionRestoreCompatibilityKey {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: AgentId,
        native_session_id: impl Into<String>,
        native_state_home_id: NativeStateHomeId,
        adapter_compatibility_identity: impl Into<String>,
        agent_state_format_identity: Option<String>,
        provider_resume_identity: impl Into<String>,
        workspace_identity: impl Into<String>,
    ) -> VibexResult<Self> {
        let native_session_id = normalize_restore_identity(native_session_id, "native_session_id")?;
        let adapter_compatibility_identity = normalize_restore_identity(
            adapter_compatibility_identity,
            "adapter_compatibility_identity",
        )?;
        let provider_resume_identity =
            normalize_restore_identity(provider_resume_identity, "provider_resume_identity")?;
        let workspace_identity =
            normalize_restore_identity(workspace_identity, "workspace_identity")?;
        let agent_state_format_identity = agent_state_format_identity
            .map(|value| normalize_restore_identity(value, "agent_state_format_identity"))
            .transpose()?;
        Ok(Self {
            agent_id,
            native_session_id,
            native_state_home_id,
            adapter_compatibility_identity,
            agent_state_format_identity,
            provider_resume_identity,
            workspace_identity,
        })
    }
}

impl<'de> Deserialize<'de> for AgentSessionRestoreCompatibilityKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            agent_id: AgentId,
            native_session_id: String,
            native_state_home_id: NativeStateHomeId,
            adapter_compatibility_identity: String,
            agent_state_format_identity: Option<String>,
            provider_resume_identity: String,
            workspace_identity: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.agent_id,
            wire.native_session_id,
            wire.native_state_home_id,
            wire.adapter_compatibility_identity,
            wire.agent_state_format_identity,
            wire.provider_resume_identity,
            wire.workspace_identity,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Provider-neutral restore operation. ACP-specific encoding is recorded
/// separately so the core contract does not depend on the ACP crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRestoreMethod {
    Resume,
    Load,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreIncompatibilityReason {
    AgentMismatch,
    NativeStateHomeMismatch,
    AdapterCompatibilityMismatch,
    AgentStateFormatMismatch,
    ProviderResumeIdentityMismatch,
    WorkspaceMismatch,
    MissingIdentity,
    CapabilityUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSessionRestoreCompatibility {
    Compatible,
    ProbeRequired {
        #[serde(rename = "allowedMethods")]
        allowed_methods: Vec<AgentSessionRestoreMethod>,
    },
    Incompatible {
        reason: RestoreIncompatibilityReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRestoreOutcome {
    Resumed,
    Loaded,
    NotFound,
    AuthenticationRequired,
    Unsupported,
    TransientFailure,
    FatalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRestoreAttempt {
    pub method: AgentSessionRestoreMethod,
    pub encoding: Option<String>,
    pub capability_source: Option<String>,
    pub outcome: AgentSessionRestoreOutcome,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRestoreResult {
    pub outcome: AgentSessionRestoreOutcome,
    pub compatibility: AgentSessionRestoreCompatibility,
    pub attempts: Vec<AgentSessionRestoreAttempt>,
    pub method: Option<AgentSessionRestoreMethod>,
    pub encoding: Option<String>,
    pub capability_source: Option<String>,
    pub error_code: Option<String>,
    pub activation_generation: i64,
    pub fresh_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRestoreStrategy {
    pub methods: Vec<AgentSessionRestoreMethod>,
    pub allow_fresh: bool,
}

/// §3.4 / §20.2 — durable binding between a logical session and a native
/// agent session created through a specific agent/profile/adapter route.
///
/// `binding_id` is an independently generated surrogate key: the same
/// (session, agent, profile, adapter compatibility identity) combination may
/// legitimately own several bindings (multiple `ForceFreshSession` results).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBinding {
    pub binding_id: RuntimeBindingId,
    pub session_id: VibexSessionId,
    pub agent_id: AgentId,
    pub transport_kind: TransportKind,
    pub provider_profile_id: ProviderProfileId,
    pub adapter_id: AcpAdapterId,
    pub adapter_version: String,
    pub adapter_compatibility_identity: String,
    pub native_session_id: Option<String>,
    pub native_state_home_id: NativeStateHomeId,
    pub provider_resume_identity: Option<String>,
    pub process_spawn_fingerprint: String,
    pub session_runtime_config_state: SessionRuntimeConfigState,
    pub capability_snapshot: Option<serde_json::Value>,
    pub restore_compatibility_key: Option<AgentSessionRestoreCompatibilityKey>,
    pub profile_revision: i64,
    pub last_context_sequence: i64,
    pub last_summary_sequence: i64,
    pub context_bridge_version: i64,
    pub activation_generation: i64,
    pub binding_state: BindingState,
    pub created_by_switch_id: Option<RuntimeSwitchId>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl fmt::Debug for RuntimeBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeBinding")
            .field("binding_id", &self.binding_id)
            .field("session_id", &self.session_id)
            .field("agent_id", &self.agent_id)
            .field("transport_kind", &self.transport_kind)
            .field("provider_profile_id", &self.provider_profile_id)
            .field("adapter_id", &self.adapter_id)
            .field("adapter_version", &self.adapter_version)
            .field("adapter_compatibility_identity", &"<redacted>")
            .field("has_native_session_id", &self.native_session_id.is_some())
            .field("native_state_home_id", &self.native_state_home_id)
            .field(
                "has_provider_resume_identity",
                &self.provider_resume_identity.is_some(),
            )
            .field("process_spawn_fingerprint", &"<redacted>")
            .field(
                "has_capability_snapshot",
                &self.capability_snapshot.is_some(),
            )
            .field(
                "has_restore_compatibility_key",
                &self.restore_compatibility_key.is_some(),
            )
            .field("profile_revision", &self.profile_revision)
            .field("last_context_sequence", &self.last_context_sequence)
            .field("last_summary_sequence", &self.last_summary_sequence)
            .field("context_bridge_version", &self.context_bridge_version)
            .field("activation_generation", &self.activation_generation)
            .field("binding_state", &self.binding_state)
            .field("created_by_switch_id", &self.created_by_switch_id)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

/// §21.2 — product-level runtime selection submitted by ordinary sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeSelection {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub mode_id: Option<String>,
    /// Explicit values for provider-neutral session features advertised by
    /// the selected Agent through ACP `configOptions`.
    #[serde(default)]
    pub config_values: BTreeMap<String, String>,
}

impl SessionRuntimeSelection {
    /// Optional fields represent explicit overrides. When they are absent, the
    /// Adapter's converged session default remains authoritative.
    pub fn matches_effective_config(&self, config: &SessionRuntimeConfigState) -> bool {
        config.effective_model.as_deref() == Some(self.model_id.as_str())
            && self
                .reasoning_effort
                .as_ref()
                .is_none_or(|effort| config.effective_reasoning_effort.as_ref() == Some(effort))
            && self
                .mode_id
                .as_ref()
                .is_none_or(|mode| config.effective_mode.as_ref() == Some(mode))
            && self.config_values.iter().all(|(key, value)| {
                config
                    .config_values
                    .get(key)
                    .and_then(|state| state.effective.as_ref())
                    .is_some_and(|effective| effective.value == *value)
            })
    }
}

/// A bounded, provider-neutral value exposed by the Runtime Option Catalog.
/// Labels are display metadata only; the value is the only part sent back in
/// a product-level selection request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigValue {
    pub value: String,
    pub label: Option<String>,
}

/// UI shape of a generic ACP session configuration option. The option id and
/// values remain provider-defined; Vibex only normalizes their bounded wire
/// representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeFeatureKind {
    Toggle,
    Select,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeFeature {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub kind: SessionRuntimeFeatureKind,
    pub current_value: Option<SessionConfigValue>,
    pub default_value: Option<SessionConfigValue>,
    #[serde(default)]
    pub values: Vec<SessionConfigValue>,
}

impl SessionRuntimeFeature {
    pub fn accepts_value(&self, value: &str) -> bool {
        match self.kind {
            SessionRuntimeFeatureKind::Toggle => matches!(value, "true" | "false"),
            SessionRuntimeFeatureKind::Select => {
                self.values.iter().any(|candidate| candidate.value == value)
                    || self
                        .current_value
                        .as_ref()
                        .is_some_and(|candidate| candidate.value == value)
                    || self
                        .default_value
                        .as_ref()
                        .is_some_and(|candidate| candidate.value == value)
            }
            SessionRuntimeFeatureKind::String => {
                !value.trim().is_empty() && value.trim().len() <= 256
            }
        }
    }

    pub fn value_for(
        &self,
        config_values: &BTreeMap<String, String>,
    ) -> Option<SessionConfigValue> {
        if let Some(value) = config_values
            .get(&self.id)
            .filter(|value| self.accepts_value(value))
        {
            let label = self
                .values
                .iter()
                .find(|candidate| candidate.value == *value)
                .and_then(|candidate| candidate.label.clone());
            return Some(SessionConfigValue {
                value: value.clone(),
                label,
            });
        }
        self.current_value
            .clone()
            .or_else(|| self.default_value.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOptionAvailability {
    Available,
    TemporarilyUnavailable,
    RequiresConfiguration,
}

/// One selectable Agent/Profile/Model combination. Adapter and native runtime
/// details deliberately do not cross this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeOption {
    pub selection: SessionRuntimeSelection,
    pub agent_label: String,
    pub provider_profile_label: String,
    pub model_label: String,
    pub reasoning_efforts: Vec<SessionConfigValue>,
    pub modes: Vec<SessionConfigValue>,
    #[serde(default)]
    pub features: Vec<SessionRuntimeFeature>,
    pub availability: RuntimeOptionAvailability,
}

/// Published snapshot consumed by ordinary session selectors. The revision is
/// deterministic for the ordered option projection and can be used for stale
/// confirmation checks after a profile/capability change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuntimeOptionCatalog {
    pub revision: i64,
    pub options: Vec<SessionRuntimeOption>,
}

/// Desired/effective selection state returned with a logical session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeSelectionState {
    pub desired: SessionRuntimeSelection,
    pub effective: SessionRuntimeSelection,
    pub status: SessionRuntimeSelectionStatus,
    pub session_revision: i64,
    pub selection_revision: i64,
    pub current_binding_id: Option<RuntimeBindingId>,
    pub activation_generation: i64,
    pub pending_switch_id: Option<RuntimeSwitchId>,
    pub actionable_error: Option<RuntimeSelectionActionableError>,
}

/// §21.2 — how the desired selection currently relates to the effective one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeSelectionStatus {
    Ready,
    WaitingForCurrentWork,
    Preparing,
    FailedUsingPrevious,
}

pub const MAX_RUNTIME_SELECTION_ERROR_CODE_LEN: usize = 160;
pub const MAX_RUNTIME_SELECTION_ERROR_MESSAGE_LEN: usize = 512;
pub const MAX_RUNTIME_SELECTION_RECOVERY_HINT_LEN: usize = 512;

/// Bounded product-facing failure information. Provider payloads, native ids,
/// commands and credentials never cross this projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSelectionActionableError {
    pub code: String,
    pub message: String,
    pub recovery_hint: Option<String>,
}

impl RuntimeSelectionActionableError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        recovery_hint: Option<String>,
    ) -> VibexResult<Self> {
        let code = bounded_runtime_selection_text(
            code.into(),
            MAX_RUNTIME_SELECTION_ERROR_CODE_LEN,
            "runtime_selection_error_code_invalid",
        )?;
        let message = bounded_runtime_selection_text(
            message.into(),
            MAX_RUNTIME_SELECTION_ERROR_MESSAGE_LEN,
            "runtime_selection_error_message_invalid",
        )?;
        let recovery_hint = recovery_hint
            .map(|value| {
                bounded_runtime_selection_text(
                    value,
                    MAX_RUNTIME_SELECTION_RECOVERY_HINT_LEN,
                    "runtime_selection_recovery_hint_invalid",
                )
            })
            .transpose()?;
        Ok(Self {
            code,
            message,
            recovery_hint,
        })
    }
}

fn bounded_runtime_selection_text(
    value: String,
    max_len: usize,
    code: &'static str,
) -> VibexResult<String> {
    let value = value.trim().to_string();
    if value.is_empty()
        || value.len() > max_len
        || value.chars().any(|character| character.is_control())
    {
        return Err(VibexError::validation(
            code,
            "runtime selection error text must be non-empty, bounded and contain no control characters",
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSelectionInteraction {
    Seamless,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDesiredAgentSessionRuntimeRequest {
    pub session_id: VibexSessionId,
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub expected_selection_revision: i64,
    pub desired: SessionRuntimeSelection,
    pub interaction: RuntimeSelectionInteraction,
}

/// Low-level control API used by automation and diagnostics. Ordinary session
/// selectors use [`SetDesiredAgentSessionRuntimeRequest`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchAgentSessionRuntimeRequest {
    pub session_id: VibexSessionId,
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub target: SessionRuntimeSelection,
    pub target_adapter_id: Option<AcpAdapterId>,
    pub policy: RuntimeSwitchPolicy,
    pub active_work_policy: RuntimeSwitchActiveWorkPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchAgentSessionRuntimeResponse {
    pub switch_id: RuntimeSwitchId,
    pub status: RuntimeSwitchStatus,
    pub session_revision: i64,
    pub current_binding_id: Option<RuntimeBindingId>,
    pub target_binding_id: Option<RuntimeBindingId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAgentSessionRuntimeSwitchRequest {
    pub session_id: VibexSessionId,
    pub switch_id: RuntimeSwitchId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSwitchEventVisibility {
    Internal,
    Audit,
    UserNotice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSwitchEventKind {
    RuntimeSwitchRequested,
    RuntimeSwitchPrepared,
    RuntimeSwitchCommitted,
    RuntimeSwitchFailed,
    RuntimeSwitchCancelled,
    RuntimeSwitchSuperseded,
    RuntimeConfigStale,
    RuntimeConfigApplied,
    RuntimeResumed,
    RuntimeLoaded,
    RuntimeCreatedFresh,
    ContextBridgeApplied,
    AdapterCompatibilityFallback,
}

impl RuntimeSwitchEventKind {
    pub const fn default_visibility(self) -> RuntimeSwitchEventVisibility {
        match self {
            Self::RuntimeSwitchPrepared => RuntimeSwitchEventVisibility::Internal,
            Self::RuntimeSwitchFailed => RuntimeSwitchEventVisibility::UserNotice,
            Self::RuntimeSwitchRequested
            | Self::RuntimeSwitchCommitted
            | Self::RuntimeSwitchCancelled
            | Self::RuntimeSwitchSuperseded
            | Self::RuntimeConfigStale
            | Self::RuntimeConfigApplied
            | Self::RuntimeResumed
            | Self::RuntimeLoaded
            | Self::RuntimeCreatedFresh
            | Self::ContextBridgeApplied
            | Self::AdapterCompatibilityFallback => RuntimeSwitchEventVisibility::Audit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSwitchEventProjection {
    pub event_id: EventId,
    pub switch_id: RuntimeSwitchId,
    pub session_id: VibexSessionId,
    pub kind: RuntimeSwitchEventKind,
    pub visibility: RuntimeSwitchEventVisibility,
    pub status: RuntimeSwitchStatus,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeSelectionEvent {
    pub state: AgentSessionRuntimeSelectionState,
    pub event: Option<RuntimeSwitchEventProjection>,
}

/// Roles that may keep an in-memory Runtime alive.  Only Owner and Viewer
/// cross the public attach API; the other roles are created by backend workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLeaseRole {
    Owner,
    Viewer,
    BackgroundWorker,
    SwitchPreparation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAttachmentStatus {
    Preparing,
    Ready,
    Inactive,
    Crashed,
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProcessStatus {
    Starting,
    Ready,
    Closing,
    Closed,
    Crashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMaterializationStatus {
    Available,
    NotMaterialized,
    Rebuilding,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProcessConfigStatus {
    Current,
    StaleRestartRequired,
    StaleLiveMutationAvailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventKind {
    AttachmentCreated,
    AttachmentActivated,
    AttachmentUpdated,
    AttachmentInactive,
    AttachmentCrashed,
    AttachmentRemoved,
    LeaseChanged,
    ProcessChanged,
    ResetRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventCursor {
    pub stream_id: RuntimeStreamId,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLeaseRoleCounts {
    pub owner: u32,
    pub viewer: u32,
    pub background_worker: u32,
    pub switch_preparation: u32,
}

impl Default for RuntimeLeaseRoleCounts {
    fn default() -> Self {
        Self {
            owner: 0,
            viewer: 0,
            background_worker: 0,
            switch_preparation: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLiveMessageSnapshot {
    pub text: String,
    pub next_chunk_index: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLiveToolCallSnapshot {
    pub tool_call_id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub input_summary: Option<String>,
    pub output_summary: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentTokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub thought_tokens: Option<u64>,
    pub cached_read_tokens: Option<u64>,
    pub cached_write_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub context_window_used_tokens: Option<u64>,
    pub context_window_size_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeAttachmentSnapshot {
    pub binding_id: RuntimeBindingId,
    pub process_id: RuntimeProcessId,
    pub activation_generation: i64,
    pub status: RuntimeAttachmentStatus,
    pub last_event_sequence: u64,
    pub current_model: Option<String>,
    pub current_mode: Option<String>,
    pub config_options: Vec<SessionConfigValue>,
    pub active_message: Option<RuntimeLiveMessageSnapshot>,
    pub active_tool_calls: Vec<RuntimeLiveToolCallSnapshot>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub active_terminal_count: u32,
    pub active_background_work_count: u32,
    pub lease_counts: RuntimeLeaseRoleCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AgentTokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProcessSnapshot {
    pub process_id: RuntimeProcessId,
    pub status: RuntimeProcessStatus,
    pub config_status: Option<RuntimeProcessConfigStatus>,
    pub protocol_version: Option<i64>,
    pub attached_session_count: u32,
    pub pending_request_count: u32,
    pub pending_host_callback_count: u32,
    pub lease_counts: RuntimeLeaseRoleCounts,
    pub spawn_fingerprint_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeSnapshot {
    pub session_id: VibexSessionId,
    pub cursor: RuntimeEventCursor,
    pub materialization_status: RuntimeMaterializationStatus,
    pub attachment: Option<RuntimeAttachmentSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSessionEvent {
    pub session_id: VibexSessionId,
    pub cursor: RuntimeEventCursor,
    pub kind: RuntimeEventKind,
    pub binding_id: Option<RuntimeBindingId>,
    pub process_id: Option<RuntimeProcessId>,
    pub emitted_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventBatch {
    pub session_id: VibexSessionId,
    pub events: Vec<RuntimeSessionEvent>,
    pub next_cursor: RuntimeEventCursor,
    pub reset_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetRuntimeSnapshotRequest {
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetRuntimeProcessSnapshotRequest {
    pub process_id: RuntimeProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetRuntimeEventsRequest {
    pub session_id: VibexSessionId,
    pub after: Option<RuntimeEventCursor>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachRuntimeRequest {
    pub session_id: VibexSessionId,
    pub client_id: RuntimeClientId,
    pub role: RuntimeLeaseRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachRuntimeResponse {
    pub snapshot: AgentSessionRuntimeSnapshot,
    pub lease_expires_at_ms: Option<i64>,
    pub lease_id: Option<RuntimeLeaseId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachRuntimeRequest {
    pub session_id: VibexSessionId,
    pub client_id: RuntimeClientId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachRuntimeResponse {
    pub released: bool,
}

/// §21.1 — caller preference for how a runtime switch should be executed.
/// Serialized into the durable `runtime_switches.requested_policy_json`
/// column; the JSON shape is locked by tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSwitchPolicy {
    Automatic,
    PreferLiveMutation,
    PreferResume,
    ForceFreshSession,
}

/// §16.5 — the four categories of in-flight work a switch must account for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveWorkKind {
    ActiveTurn,
    PendingPermission,
    ActiveTerminal,
    BackgroundWork,
}

/// §16.5 — disposition for one category of active work when a switch is
/// requested. `Wait` carries a bounded deadline in milliseconds; it is never
/// an unbounded wait. Serialized into durable JSON columns, so the wire shape
/// is locked by tests: unit variants are plain strings and `Wait` is an
/// externally tagged object, following the file-local payload-enum precedent
/// (`AgentSessionRestoreCompatibility`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyDisposition {
    /// Safe low-level default: refuse to switch while this category is busy.
    #[default]
    Reject,
    Wait {
        #[serde(rename = "deadlineMs")]
        deadline_ms: u64,
    },
    Cancel,
}

pub const MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS: u64 = 24 * 60 * 60 * 1000;

/// §16.5 — per-category dispositions declared by a switch request. The low
/// level safe default is `Reject` for all four categories; product policies
/// such as `SeamlessSessionSwitch` are built on top of this type, not baked
/// into it. Serialized into `runtime_switches.active_work_policy_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSwitchActiveWorkPolicy {
    pub active_turn: BusyDisposition,
    pub pending_permission: BusyDisposition,
    pub active_terminal: BusyDisposition,
    pub background_work: BusyDisposition,
}

impl RuntimeSwitchActiveWorkPolicy {
    /// Returns the disposition configured for one active-work category.
    pub fn disposition(&self, kind: ActiveWorkKind) -> BusyDisposition {
        match kind {
            ActiveWorkKind::ActiveTurn => self.active_turn,
            ActiveWorkKind::PendingPermission => self.pending_permission,
            ActiveWorkKind::ActiveTerminal => self.active_terminal,
            ActiveWorkKind::BackgroundWork => self.background_work,
        }
    }

    pub fn validate(&self) -> VibexResult<()> {
        for kind in [
            ActiveWorkKind::ActiveTurn,
            ActiveWorkKind::PendingPermission,
            ActiveWorkKind::ActiveTerminal,
            ActiveWorkKind::BackgroundWork,
        ] {
            if let BusyDisposition::Wait { deadline_ms } = self.disposition(kind)
                && !(1..=MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS).contains(&deadline_ms)
            {
                return Err(VibexError::validation(
                    "runtime_switch_wait_deadline_invalid",
                    "runtime switch wait deadline must be positive and bounded",
                ));
            }
        }
        Ok(())
    }
}

/// §17 — durable state of a two-phase runtime switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSwitchStatus {
    Requested,
    Reserved,
    WaitingForIdle,
    Preparing,
    Prepared,
    Committing,
    Committed,
    Failed,
    Cancelled,
    Superseded,
    AmbiguousExternalEffect,
}

impl RuntimeSwitchStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Committed
                | Self::Failed
                | Self::Cancelled
                | Self::Superseded
                | Self::AmbiguousExternalEffect
        )
    }

    /// §17 — whether transitioning from `self` to `next` is a legal move in the
    /// switch state machine. Terminal states accept no further transitions.
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return false;
        }
        match self {
            Self::Requested => matches!(
                next,
                Self::Reserved | Self::Failed | Self::Cancelled | Self::Superseded
            ),
            Self::Reserved => matches!(
                next,
                Self::WaitingForIdle
                    | Self::Preparing
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Superseded
                    | Self::AmbiguousExternalEffect
            ),
            Self::WaitingForIdle => matches!(
                next,
                Self::Preparing
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Superseded
                    | Self::AmbiguousExternalEffect
            ),
            Self::Preparing => matches!(
                next,
                Self::Prepared
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Superseded
                    | Self::AmbiguousExternalEffect
            ),
            Self::Prepared => matches!(
                next,
                Self::Committing
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Superseded
                    | Self::AmbiguousExternalEffect
            ),
            Self::Committing => matches!(
                next,
                Self::Committed
                    | Self::Prepared
                    | Self::Failed
                    | Self::Superseded
                    | Self::AmbiguousExternalEffect
            ),
            Self::Committed
            | Self::Failed
            | Self::Cancelled
            | Self::Superseded
            | Self::AmbiguousExternalEffect => false,
        }
    }
}

/// §17.2 / §20.3 — write-ahead journal status for an external side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchOperationStatus {
    AboutToSend,
    Succeeded,
    Failed,
    AmbiguousExternalEffect,
}

/// §20.3 — retry semantics declared per journaled external operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySemantics {
    Idempotent,
    ReconcileBeforeRetry,
    NonRetryableWhenAmbiguous,
}

/// §20.4 — durable status of a queued message submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSubmissionStatus {
    AwaitingRuntime,
    ReadyToDispatch,
    AboutToPrompt,
    Dispatched,
    Completed,
    Failed,
    Cancelled,
    AmbiguousPromptDispatch,
}

impl MessageSubmissionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::AmbiguousPromptDispatch
        )
    }

    /// §20.4 — whether transitioning from `self` to `next` is a legal move.
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return false;
        }
        match self {
            Self::AwaitingRuntime => {
                matches!(next, Self::ReadyToDispatch | Self::Failed | Self::Cancelled)
            }
            Self::ReadyToDispatch => matches!(
                next,
                Self::AboutToPrompt | Self::AwaitingRuntime | Self::Failed | Self::Cancelled
            ),
            Self::AboutToPrompt => {
                matches!(next, Self::Dispatched | Self::AmbiguousPromptDispatch)
            }
            Self::Dispatched => next == Self::Completed,
            Self::Completed | Self::Failed | Self::Cancelled | Self::AmbiguousPromptDispatch => {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn route_key(agent: &str, adapter: &str) -> AgentRuntimeRouteKey {
        AgentRuntimeRouteKey {
            agent_id: AgentId::parse(agent).unwrap(),
            transport_kind: TransportKind::Acp,
            adapter_id: AcpAdapterId::parse(adapter).unwrap(),
        }
    }

    #[test]
    fn route_key_works_as_map_key() {
        let mut map = BTreeMap::new();
        map.insert(route_key("claude-code", "claude-code-acp"), 1);
        map.insert(route_key("codex", "codex-acp"), 2);
        map.insert(route_key("claude-code", "claude-code-acp"), 3);

        assert_eq!(map.len(), 2);
        assert_eq!(map[&route_key("claude-code", "claude-code-acp")], 3);

        let mut hash_map = std::collections::HashMap::new();
        hash_map.insert(route_key("codex", "codex-acp"), "x");
        assert_eq!(hash_map.get(&route_key("codex", "codex-acp")), Some(&"x"));
    }

    #[test]
    fn agent_token_usage_uses_camel_case_and_accepts_partial_payloads() {
        let usage: AgentTokenUsage = serde_json::from_value(serde_json::json!({
            "inputTokens": 120,
            "cachedReadTokens": 80,
            "contextWindowUsedTokens": 1_500,
            "contextWindowSizeTokens": 8_000
        }))
        .unwrap();
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(usage.cached_read_tokens, Some(80));
        assert_eq!(usage.output_tokens, None);

        let encoded = serde_json::to_value(usage).unwrap();
        assert_eq!(encoded["contextWindowUsedTokens"], 1_500);
        assert_eq!(encoded["contextWindowSizeTokens"], 8_000);
    }

    #[test]
    fn acp_adapter_id_rejects_empty() {
        assert!(AcpAdapterId::parse("  ").is_err());
        assert_eq!(
            AcpAdapterId::parse("claude-code-acp").unwrap().as_str(),
            "claude-code-acp"
        );
    }

    #[test]
    fn selection_serde_round_trip() {
        let selection = SessionRuntimeSelection {
            agent_id: AgentId::parse("claude-code").unwrap(),
            provider_profile_id: crate::ids::ProviderProfileId::new(),
            model_id: "claude-sonnet-4-5".to_string(),
            reasoning_effort: Some("high".to_string()),
            mode_id: None,
            config_values: BTreeMap::from([("web_search".to_string(), "true".to_string())]),
        };
        let json = serde_json::to_string(&selection).unwrap();
        let restored: SessionRuntimeSelection = serde_json::from_str(&json).unwrap();
        assert_eq!(selection, restored);
    }

    #[test]
    fn selection_deserializes_legacy_payload_without_feature_values() {
        let selection: SessionRuntimeSelection = serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_acp_codex",
            "modelId": "gpt-5",
            "reasoningEffort": null,
            "modeId": null
        }))
        .unwrap();
        assert!(selection.config_values.is_empty());
    }

    #[test]
    fn runtime_binding_serde_round_trip() {
        let binding = RuntimeBinding {
            binding_id: RuntimeBindingId::new(),
            session_id: VibexSessionId::new(),
            agent_id: AgentId::parse("codex").unwrap(),
            transport_kind: TransportKind::Acp,
            provider_profile_id: ProviderProfileId::new(),
            adapter_id: AcpAdapterId::parse("codex-acp").unwrap(),
            adapter_version: "0.4.0".to_string(),
            adapter_compatibility_identity: "codex-acp@v1".to_string(),
            native_session_id: Some("native-123".to_string()),
            native_state_home_id: NativeStateHomeId::new(),
            provider_resume_identity: Some("resume-secret".to_string()),
            process_spawn_fingerprint: "fp_abc".to_string(),
            session_runtime_config_state: SessionRuntimeConfigState {
                preferred_model: Some("gpt-5.2-codex".to_string()),
                effective_model: None,
                ..Default::default()
            },
            capability_snapshot: Some(
                serde_json::json!({"loadSession": true, "private": "capability-secret"}),
            ),
            restore_compatibility_key: None,
            profile_revision: 3,
            last_context_sequence: 42,
            last_summary_sequence: 0,
            context_bridge_version: 1,
            activation_generation: 7,
            binding_state: BindingState::Current,
            created_by_switch_id: Some(RuntimeSwitchId::new()),
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let json = serde_json::to_string(&binding).unwrap();
        let restored: RuntimeBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(binding, restored);
        let debug = format!("{binding:?}");
        for sensitive in [
            "native-123",
            "resume-secret",
            "fp_abc",
            "codex-acp@v1",
            "capability-secret",
        ] {
            assert!(!debug.contains(sensitive));
        }
    }

    #[test]
    fn restore_compatibility_key_validates_and_redacts_debug() {
        let native = "native-secret-session";
        let workspace = "workspace:/private/repo";
        let key = AgentSessionRestoreCompatibilityKey::new(
            AgentId::parse("codex").unwrap(),
            format!("  {native}  "),
            NativeStateHomeId::new(),
            " codex-acp@v1 ",
            Some("state-v1".to_string()),
            "provider-resume-v1",
            workspace,
        )
        .unwrap();
        assert_eq!(key.native_session_id, native);
        let debug = format!("{key:?}");
        for sensitive in [
            native,
            workspace,
            "codex-acp@v1",
            "state-v1",
            "provider-resume-v1",
        ] {
            assert!(!debug.contains(sensitive));
        }
        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains(native));
        assert_eq!(
            serde_json::from_str::<AgentSessionRestoreCompatibilityKey>(&json).unwrap(),
            key
        );
        assert!(
            AgentSessionRestoreCompatibilityKey::new(
                AgentId::parse("codex").unwrap(),
                " ",
                NativeStateHomeId::new(),
                "adapter",
                None,
                "resume",
                "workspace",
            )
            .is_err()
        );
    }

    #[test]
    fn restore_outcomes_are_distinct_and_stable() {
        let values = [
            AgentSessionRestoreOutcome::Resumed,
            AgentSessionRestoreOutcome::Loaded,
            AgentSessionRestoreOutcome::NotFound,
            AgentSessionRestoreOutcome::AuthenticationRequired,
            AgentSessionRestoreOutcome::Unsupported,
            AgentSessionRestoreOutcome::TransientFailure,
            AgentSessionRestoreOutcome::FatalFailure,
        ];
        let encoded = values
            .iter()
            .map(|value| serde_json::to_string(value).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            encoded,
            [
                "\"resumed\"",
                "\"loaded\"",
                "\"not_found\"",
                "\"authentication_required\"",
                "\"unsupported\"",
                "\"transient_failure\"",
                "\"fatal_failure\"",
            ]
        );
    }

    #[test]
    fn session_runtime_config_state_tracks_convergence_by_generation() {
        let mut state = SessionRuntimeConfigState {
            preferred_model: Some("mock/model-2".to_string()),
            effective_model: Some("mock/model-1".to_string()),
            state_revision: 1,
            ..Default::default()
        };
        state.config_values.insert(
            "approval_mode".to_string(),
            SessionRuntimeConfigValueState {
                preferred: Some(ProviderSessionConfigValue {
                    value: "ask".to_string(),
                    label: Some("Ask".to_string()),
                }),
                effective: Some(ProviderSessionConfigValue {
                    value: "ask".to_string(),
                    label: None,
                }),
            },
        );
        assert!(!state.is_converged());
        state.mark_generation_if_converged(4);
        assert_eq!(state.applied_activation_generation, None);

        state.effective_model = state.preferred_model.clone();
        assert!(state.is_converged());
        state.mark_generation_if_converged(4);
        assert_eq!(state.applied_activation_generation, Some(4));

        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: SessionRuntimeConfigState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn runtime_selection_matches_adapter_defaults_for_optional_fields() {
        let selection = SessionRuntimeSelection {
            agent_id: AgentId::parse("codex").unwrap(),
            provider_profile_id: ProviderProfileId::parse("provider_acp_codex").unwrap(),
            model_id: "gpt-test".to_string(),
            reasoning_effort: None,
            mode_id: None,
            config_values: Default::default(),
        };
        let config = SessionRuntimeConfigState {
            effective_model: Some("gpt-test".to_string()),
            effective_reasoning_effort: Some("medium".to_string()),
            effective_mode: Some("build".to_string()),
            ..Default::default()
        };
        assert!(selection.matches_effective_config(&config));

        let feature_selection = SessionRuntimeSelection {
            config_values: BTreeMap::from([("web_search".to_string(), "true".to_string())]),
            ..selection.clone()
        };
        assert!(!feature_selection.matches_effective_config(&config));
        let mut matching_config = config.clone();
        matching_config.config_values.insert(
            "web_search".to_string(),
            SessionRuntimeConfigValueState {
                preferred: Some(ProviderSessionConfigValue {
                    value: "true".to_string(),
                    label: None,
                }),
                effective: Some(ProviderSessionConfigValue {
                    value: "true".to_string(),
                    label: None,
                }),
            },
        );
        assert!(feature_selection.matches_effective_config(&matching_config));

        let explicit = SessionRuntimeSelection {
            reasoning_effort: Some("high".to_string()),
            mode_id: Some("review".to_string()),
            ..selection
        };
        assert!(!explicit.matches_effective_config(&config));
    }

    #[test]
    fn session_runtime_config_patch_uses_explicit_clear_flags() {
        let patch = SessionRuntimeConfigPatch {
            clear_model: true,
            clear_config_keys: vec!["approval_mode".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_value(&patch).unwrap();
        assert_eq!(json["clearModel"], true);
        assert_eq!(json["clearConfigKeys"][0], "approval_mode");
        assert!(json["modelId"].is_null());
    }

    #[test]
    fn session_runtime_config_state_reads_legacy_generic_values() {
        let state: SessionRuntimeConfigState = serde_json::from_value(serde_json::json!({
            "configValues": {
                "approval_mode": { "value": "ask", "label": "Ask" }
            }
        }))
        .unwrap();
        let value = state.config_values.get("approval_mode").unwrap();
        assert_eq!(value.preferred.as_ref().unwrap().value, "ask");
        assert_eq!(value.effective.as_ref().unwrap().value, "ask");
        let encoded = serde_json::to_value(state).unwrap();
        assert_eq!(
            encoded["configValues"]["approval_mode"]["preferred"]["value"],
            "ask"
        );
    }

    #[test]
    fn session_runtime_config_generation_marker_requires_convergence() {
        let mut state = SessionRuntimeConfigState::default();
        state.mark_generation_if_converged(7);
        assert!(state.is_applied_to_generation(7));
        state.preferred_model = Some("model-a".to_string());
        assert!(!state.is_applied_to_generation(7));
    }

    #[test]
    fn enum_serde_uses_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&RuntimeSwitchStatus::AmbiguousExternalEffect).unwrap(),
            "\"ambiguous_external_effect\""
        );
        assert_eq!(
            serde_json::to_string(&MessageSubmissionStatus::AboutToPrompt).unwrap(),
            "\"about_to_prompt\""
        );
        assert_eq!(
            serde_json::to_string(&RetrySemantics::NonRetryableWhenAmbiguous).unwrap(),
            "\"non_retryable_when_ambiguous\""
        );
        assert_eq!(
            serde_json::to_string(&TransportKind::Acp).unwrap(),
            "\"acp\""
        );
        let status: BindingState = serde_json::from_str("\"preparing\"").unwrap();
        assert_eq!(status, BindingState::Preparing);
    }

    #[test]
    fn runtime_switch_policy_serde_locks_snake_case_strings() {
        let cases = [
            (RuntimeSwitchPolicy::Automatic, "\"automatic\""),
            (
                RuntimeSwitchPolicy::PreferLiveMutation,
                "\"prefer_live_mutation\"",
            ),
            (RuntimeSwitchPolicy::PreferResume, "\"prefer_resume\""),
            (
                RuntimeSwitchPolicy::ForceFreshSession,
                "\"force_fresh_session\"",
            ),
        ];
        for (policy, expected) in cases {
            let encoded = serde_json::to_string(&policy).unwrap();
            assert_eq!(encoded, expected);
            let decoded: RuntimeSwitchPolicy = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, policy);
        }
    }

    #[test]
    fn active_work_kind_serde_locks_snake_case_strings() {
        let cases = [
            (ActiveWorkKind::ActiveTurn, "\"active_turn\""),
            (ActiveWorkKind::PendingPermission, "\"pending_permission\""),
            (ActiveWorkKind::ActiveTerminal, "\"active_terminal\""),
            (ActiveWorkKind::BackgroundWork, "\"background_work\""),
        ];
        for (kind, expected) in cases {
            let encoded = serde_json::to_string(&kind).unwrap();
            assert_eq!(encoded, expected);
            let decoded: ActiveWorkKind = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn busy_disposition_serde_locks_durable_json_shape() {
        // Unit variants serialize as plain strings; `Wait` is externally
        // tagged with a camelCase deadline field. These literals are the
        // durable v1 wire contract for `active_work_policy_json`.
        assert_eq!(
            serde_json::to_string(&BusyDisposition::Reject).unwrap(),
            "\"reject\""
        );
        assert_eq!(
            serde_json::to_string(&BusyDisposition::Cancel).unwrap(),
            "\"cancel\""
        );
        assert_eq!(
            serde_json::to_string(&BusyDisposition::Wait { deadline_ms: 30000 }).unwrap(),
            "{\"wait\":{\"deadlineMs\":30000}}"
        );
        for disposition in [
            BusyDisposition::Reject,
            BusyDisposition::Cancel,
            BusyDisposition::Wait { deadline_ms: 1 },
        ] {
            let encoded = serde_json::to_string(&disposition).unwrap();
            let decoded: BusyDisposition = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, disposition);
        }
        assert_eq!(BusyDisposition::default(), BusyDisposition::Reject);
    }

    #[test]
    fn active_work_policy_defaults_to_all_reject_and_locks_json_shape() {
        let policy = RuntimeSwitchActiveWorkPolicy::default();
        for kind in [
            ActiveWorkKind::ActiveTurn,
            ActiveWorkKind::PendingPermission,
            ActiveWorkKind::ActiveTerminal,
            ActiveWorkKind::BackgroundWork,
        ] {
            assert_eq!(policy.disposition(kind), BusyDisposition::Reject);
        }
        let default_json = concat!(
            "{\"activeTurn\":\"reject\",\"pendingPermission\":\"reject\",",
            "\"activeTerminal\":\"reject\",\"backgroundWork\":\"reject\"}"
        );
        assert_eq!(serde_json::to_string(&policy).unwrap(), default_json);

        let mixed = RuntimeSwitchActiveWorkPolicy {
            active_turn: BusyDisposition::Wait { deadline_ms: 45000 },
            pending_permission: BusyDisposition::Wait { deadline_ms: 45000 },
            active_terminal: BusyDisposition::Cancel,
            background_work: BusyDisposition::Reject,
        };
        assert_eq!(
            mixed.disposition(ActiveWorkKind::ActiveTerminal),
            BusyDisposition::Cancel
        );
        let encoded = serde_json::to_string(&mixed).unwrap();
        let mixed_json = concat!(
            "{\"activeTurn\":{\"wait\":{\"deadlineMs\":45000}},",
            "\"pendingPermission\":{\"wait\":{\"deadlineMs\":45000}},",
            "\"activeTerminal\":\"cancel\",\"backgroundWork\":\"reject\"}"
        );
        assert_eq!(encoded, mixed_json);
        let decoded: RuntimeSwitchActiveWorkPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, mixed);
        assert!(mixed.validate().is_ok());

        for deadline_ms in [0, MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS + 1] {
            let invalid = RuntimeSwitchActiveWorkPolicy {
                active_turn: BusyDisposition::Wait { deadline_ms },
                ..RuntimeSwitchActiveWorkPolicy::default()
            };
            let error = invalid.validate().unwrap_err();
            assert_eq!(error.code, "runtime_switch_wait_deadline_invalid");
        }
    }

    #[test]
    fn runtime_selection_api_and_event_shapes_are_stable() {
        let session_id = VibexSessionId::new();
        let request = SetDesiredAgentSessionRuntimeRequest {
            session_id: session_id.clone(),
            idempotency_key: "selection-1".to_string(),
            expected_revision: 3,
            expected_selection_revision: 7,
            desired: SessionRuntimeSelection {
                agent_id: AgentId::parse("codex").unwrap(),
                provider_profile_id: ProviderProfileId::new(),
                model_id: "gpt-5".to_string(),
                reasoning_effort: Some("high".to_string()),
                mode_id: None,
                config_values: Default::default(),
            },
            interaction: RuntimeSelectionInteraction::Seamless,
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["idempotencyKey"], "selection-1");
        assert_eq!(encoded["expectedSelectionRevision"], 7);
        assert_eq!(encoded["interaction"], "seamless");

        let event = RuntimeSwitchEventProjection {
            event_id: EventId::new(),
            switch_id: RuntimeSwitchId::new(),
            session_id,
            kind: RuntimeSwitchEventKind::RuntimeSwitchFailed,
            visibility: RuntimeSwitchEventKind::RuntimeSwitchFailed.default_visibility(),
            status: RuntimeSwitchStatus::Failed,
            error_code: Some("runtime_switch_wait_timeout".to_string()),
            created_at_ms: 42,
        };
        let encoded = serde_json::to_value(event).unwrap();
        assert_eq!(encoded["kind"], "runtime_switch_failed");
        assert_eq!(encoded["visibility"], "user_notice");
        assert_eq!(encoded["status"], "failed");
    }

    #[test]
    fn actionable_error_is_bounded_and_control_free() {
        let error = RuntimeSelectionActionableError::new(
            "runtime_switch_wait_timeout",
            "The current Agent work did not finish before the switch deadline.",
            Some("Try the selection again after the current work finishes.".to_string()),
        )
        .unwrap();
        assert_eq!(error.code, "runtime_switch_wait_timeout");

        let invalid = RuntimeSelectionActionableError::new(
            "runtime_switch_wait_timeout",
            "line one\nline two",
            None,
        )
        .unwrap_err();
        assert_eq!(invalid.code, "runtime_selection_error_message_invalid");
    }

    #[test]
    fn switch_status_transitions() {
        use RuntimeSwitchStatus as S;
        assert!(S::Requested.can_transition_to(S::Reserved));
        assert!(S::Reserved.can_transition_to(S::Preparing));
        assert!(S::Reserved.can_transition_to(S::WaitingForIdle));
        assert!(S::Preparing.can_transition_to(S::Prepared));
        assert!(S::Prepared.can_transition_to(S::Committing));
        for external_effect_phase in [S::Reserved, S::WaitingForIdle, S::Prepared] {
            assert!(external_effect_phase.can_transition_to(S::AmbiguousExternalEffect));
        }
        assert!(S::Committing.can_transition_to(S::Committed));
        assert!(S::Committing.can_transition_to(S::Superseded));
        // Commit cannot be reached without going through Prepared/Committing.
        assert!(!S::Requested.can_transition_to(S::Committed));
        assert!(!S::Preparing.can_transition_to(S::Committed));
        // Terminal states are frozen.
        for terminal in [
            S::Committed,
            S::Failed,
            S::Cancelled,
            S::Superseded,
            S::AmbiguousExternalEffect,
        ] {
            assert!(terminal.is_terminal());
            assert!(!terminal.can_transition_to(S::Requested));
            assert!(!terminal.can_transition_to(S::Committed));
        }
    }

    #[test]
    fn submission_status_transitions() {
        use MessageSubmissionStatus as M;
        assert!(M::AwaitingRuntime.can_transition_to(M::ReadyToDispatch));
        assert!(M::ReadyToDispatch.can_transition_to(M::AboutToPrompt));
        assert!(M::AboutToPrompt.can_transition_to(M::Dispatched));
        assert!(M::AboutToPrompt.can_transition_to(M::AmbiguousPromptDispatch));
        assert!(M::Dispatched.can_transition_to(M::Completed));
        assert!(!M::AboutToPrompt.can_transition_to(M::Failed));
        assert!(!M::AboutToPrompt.can_transition_to(M::Cancelled));
        assert!(!M::Dispatched.can_transition_to(M::Failed));
        assert!(!M::AwaitingRuntime.can_transition_to(M::Dispatched));
        assert!(!M::Completed.can_transition_to(M::AwaitingRuntime));
        assert!(M::AmbiguousPromptDispatch.is_terminal());
    }
}

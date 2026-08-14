//! Generic ACP Agent provider foundation.
//!
//! This crate owns ACP-specific adapter boundaries while `vibex-agent` remains
//! provider-neutral. The first implementation uses an internal client seam so
//! deterministic tests can validate Vibex timeline mapping without starting a
//! real ACP CLI.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use vibex_agent::{
    AgentProvider, AgentUsageTelemetryEvent, ProviderCreateRequest, ProviderElicitationResolution,
    ProviderEvent, ProviderPermissionResolution, ProviderRuntimeResources, ProviderSessionHandle,
    ProviderTurnAttachment, ProviderTurnExecutionIdentity, ProviderTurnRequest, ProviderTurnResult,
    materialize_provider_attachments, reject_forbidden_agent_smoke_workspace,
    resolve_agent_smoke_workspace,
};
use vibex_config_switch::{ProviderConfigService, acp_capabilities_from_config};
use vibex_core::{
    AcpProviderConfig, AcpProviderEnvSource, AcpProviderProfileCreateRequest, AgentAuthCatalog,
    AgentAuthenticateRequest, AgentAuthenticateResult, AgentAuthenticationCancelRequest,
    AgentAuthenticationCompleteRequest, AgentCommandDiscoverRequest, AgentCommandDiscoverResponse,
    AgentCommandEntry, AgentCommandExecuteRequest, AgentCommandExecutionBehavior,
    AgentCommandSelectionBehavior, AgentCommandSourceKind, AgentCommandTrigger,
    AgentEventRawExtension, AgentLogoutRequest, AgentMessageDeltaPayload, AgentMessagePayload,
    AgentMessagePhase, AgentModelCapabilities, AgentModelListResponse, AgentModelListSource,
    AgentReasoningEffort, AgentSessionConfigProbe, AgentSessionSafety, AgentUsageCounterOrigin,
    AgentUsageExecutionContext, ElicitationRequest, ExternalSessionImportCandidate,
    MessageSubmissionId, PermissionActionDetail, PermissionRequest, PermissionRequestStatus,
    PermissionResponseKind, PermissionResponseOption, PermissionRiskCategory, PlanPayload,
    PlanStepPayload, ProviderBinding, ProviderBindingMetadata, ProviderCapabilities,
    ProviderCapabilitySummary, ProviderKind, ProviderNativeBinding, ProviderProfileId,
    ProviderRunCapabilityProbesRequest, ProviderSessionConfigOption, ProviderSessionConfigValue,
    ReasoningPayload, RequestId, SessionRuntimeConfigMutationRequest,
    SessionRuntimeConfigMutationResult, SessionRuntimeSelection, SystemNoticeLevel,
    SystemNoticePayload, TimelineErrorPayload, TimelinePayload, TimelineRedactionState,
    ToolCallPayload, ToolCallStatus, VibexError, VibexResult, VibexSessionId, unix_timestamp_ms,
};

mod adapter_activation;
mod auth;
mod bridge_contract;
mod claude;
mod codex;
mod events;
mod managed_adapter;
mod private_fs;
mod process_environment;
mod process_registry;
mod protocol;
mod registry;
mod runtime;
mod runtime_probe;
mod session_attachment_registry;
mod session_config;
mod session_restore;
mod spawn_config;

pub use adapter_activation::VerifiedAcpAdapterActivation;
pub use bridge_contract::{
    AcpBridgeContractAdapterReport, AcpBridgeContractRunner, BridgeContractMcpFixture,
};
pub use claude::{
    ClaudeAcpSmokeError, ClaudeAcpSmokeResult, ClaudeBackgroundWorkRegistry, ClaudeExtensionEvent,
    ClaudeTranscriptDeduper, ClaudeTranscriptEvent, ClaudeTranscriptEventKind,
    ClaudeTranscriptTailWatcher, ClaudeWorkKey, claude_prompt_fingerprint,
    claude_transcript_event_input, decode_claude_extension, parse_claude_transcript_line,
    run_claude_agent_acp_smoke,
};
pub use codex::{
    CODEX_FORK_EXTENSION_VERSION, CodexAcpSmokeResult, CodexForkPlan, codex_acp_runtime_home_path,
    decode_codex_extension, plan_codex_fork, prepare_codex_acp_runtime_home,
    run_codex_agent_acp_smoke, write_codex_acp_runtime_config,
};
pub use events::{
    AgentEventEnricher, AgentEventInput, AgentEventInputSource, CanonicalAgentEvent,
    ClaudeEventEnricher, CodexEventEnricher, NormalizedAgentEvent, PassthroughEventEnricher,
    normalize_agent_event, parse_event_locations, parse_event_meta, stable_event_correlation_id,
};
pub use managed_adapter::{
    AcpAdapterHealthReport, ManagedAcpAdapterStore, ManagedAdapterCommand,
    VerifiedAcpAdapterInstallation,
};
pub use private_fs::{ensure_private_runtime_directory, write_private_runtime_file_atomic};
pub use process_environment::sanitize_inherited_appimage_environment;
pub use process_registry::{
    AcpProcessCrash, AcpProcessHandle, AcpProcessInstanceId, AcpProcessRegistry,
    AcpProcessSnapshot, AcpProcessStatus, MultiSessionContractEvidence, MultiSessionEvidenceKind,
    ProcessAcquireKey, ProcessLease, ProcessReuseDecision, WorkspaceScope, decide_process_reuse,
};
pub use protocol::{
    AcpOperation, AcpOperationStability, AcpOperationSupport, AcpWireEncoding, CapabilitySource,
    baseline_operation_matrix,
};
pub use registry::{
    AcpAdapterDistribution, AcpAgentCompatibility, AcpCompatibilityRegistry,
    AdapterCompatibilityIdentity, AgentEventEnricherKind, BridgeContractCase,
    BridgeContractCaseResult, BridgeContractEvidenceKind, BridgeContractRequirement,
    BridgeContractStatus, BridgeContractSummary, CapabilityEvidence, CapabilityResolutionInput,
    CapabilitySupport, CommandVariant, CompatibilitySupport, ConfigOptionAliasCompatibility,
    ManagedRuntimeDependency, NativeStateHomePolicy, ResolvedCapability, RestorePolicy,
    TranscriptStrategy, VersionedAgentQuirk, VersionedOperationDescriptor,
    agent_supports_session_config_probe, fallback_reasoning_efforts, fallback_session_modes,
    known_reasoning_effort_values, known_session_mode_values,
};
pub use runtime::{
    AcpRuntimeClient, AcpRuntimeLifecycleBackend, AcpRuntimeSwitchBridge, AcpTerminalAuthRequest,
    AcpTerminalCreateRequest, AcpTerminalExitStatus, AcpTerminalHost, AcpTerminalOutput,
    DisabledAcpTerminalHost, redacted_terminal_auth_action_descriptor,
};
pub use runtime_probe::{AgentRuntimeProbeReconcileReport, AgentRuntimeProbeService};
pub use session_attachment_registry::{
    SessionAttachmentAcquireKey, SessionAttachmentAcquireOutput, SessionAttachmentAcquireResult,
    SessionAttachmentEventFence, SessionAttachmentHandle, SessionAttachmentPromptGuard,
    SessionAttachmentRegistry, SessionAttachmentRoute, SessionAttachmentRouteDiagnostic,
    SessionAttachmentRouteRejection, SessionAttachmentState,
};
pub use session_config::{
    CanonicalKeyError, CanonicalSessionConfigKey, RuntimeOptionCatalogAgentEvidence,
    RuntimeOptionCatalogProfileEvidence, SessionConfigExtension, SessionConfigFieldKind,
    SessionConfigFieldRequest, SessionConfigOperationEvidence, SessionConfigPlan,
    SessionConfigPlanner, SessionModelCatalogEntry, SessionModelCatalogSource,
    append_agent_account_runtime_options, build_runtime_option_catalog,
    build_runtime_option_catalog_for_agents, merge_model_catalog, normalize_identifier,
    refresh_runtime_option_catalog_revision, resolve_canonical_option_key, validate_effort_value,
    validate_model_value,
};
pub use session_restore::{
    RestoreCapabilityEvidence, RestoreCapabilityMap, classify_restore_error, encoding_name,
    operation_for_method, resolve_restore_compatibility, restore_methods, result_for_failure,
    result_for_success,
};
pub use spawn_config::{
    ProcessConfigField, ProcessConfigStatus, ProcessConfigStatusEvent, ProcessSpawnConfigSnapshot,
    secret_reference_version,
};

const OPENCODE_PRESET_ID: &str = "opencode";
const DEFAULT_OPENCODE_SMOKE_PROMPT: &str =
    "Reply with a one-line ACP smoke marker and do not edit files.";
const OPENCODE_ACP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const OPENCODE_ACP_PROMPT_TIMEOUT: Duration = Duration::from_secs(60);
const OPENCODE_ACP_MODEL_SAMPLE_LIMIT: usize = 5;
const OPENCODE_ACP_CATEGORY_SAMPLE_LIMIT: usize = 12;
const OPENCODE_ACP_PERMISSION_DETAIL_LIMIT: usize = 6;
const OPENCODE_ACP_PERMISSION_VALUE_LIMIT: usize = 120;
const REDACTED_SENSITIVE_OUTPUT: &str = "[redacted-sensitive-output]";
const CODEX_ACP_PROVIDER_COMMANDS: &[CodexAcpSlashCommand] = &[
    CodexAcpSlashCommand {
        name: "plan",
        description: "Turn plan mode on.",
    },
    CodexAcpSlashCommand {
        name: "mcp",
        description: "List configured Model Context Protocol (MCP) tools.",
    },
    CodexAcpSlashCommand {
        name: "skills",
        description: "List available skills.",
    },
    CodexAcpSlashCommand {
        name: "status",
        description: "Display session configuration and token usage.",
    },
    CodexAcpSlashCommand {
        name: "review",
        description: "Review uncommitted changes, or review with custom instructions.",
    },
    CodexAcpSlashCommand {
        name: "review-branch",
        description: "Review changes relative to a base branch.",
    },
    CodexAcpSlashCommand {
        name: "review-commit",
        description: "Review a specific commit.",
    },
    CodexAcpSlashCommand {
        name: "compact",
        description: "Summarize conversation to avoid hitting the context limit.",
    },
    CodexAcpSlashCommand {
        name: "goal",
        description: "Set a goal to keep pursuing.",
    },
    CodexAcpSlashCommand {
        name: "logout",
        description: "Sign out of Codex. This option is available when you are logged in via ChatGPT.",
    },
];
const OPENCODE_ACP_PROVIDER_COMMANDS: &[OpenCodeAcpSlashCommand] = &[
    OpenCodeAcpSlashCommand {
        name: "help",
        label: "/help",
        insertion_text: "/help ",
        description: "Show OpenCode slash command help.",
    },
    OpenCodeAcpSlashCommand {
        name: "model",
        label: "/model",
        insertion_text: "/model ",
        description: "Choose or switch the active OpenCode model.",
    },
    OpenCodeAcpSlashCommand {
        name: "status",
        label: "/status",
        insertion_text: "/status ",
        description: "Show OpenCode session status and configuration.",
    },
    OpenCodeAcpSlashCommand {
        name: "session",
        label: "/session",
        insertion_text: "/session ",
        description: "Inspect or manage the current OpenCode session.",
    },
    OpenCodeAcpSlashCommand {
        name: "sessions",
        label: "/sessions",
        insertion_text: "/sessions ",
        description: "List or switch OpenCode sessions.",
    },
    OpenCodeAcpSlashCommand {
        name: "share",
        label: "/share",
        insertion_text: "/share ",
        description: "Share the current OpenCode session when supported.",
    },
    OpenCodeAcpSlashCommand {
        name: "compact",
        label: "/compact",
        insertion_text: "/compact ",
        description: "Compact OpenCode conversation context.",
    },
    OpenCodeAcpSlashCommand {
        name: "init",
        label: "/init",
        insertion_text: "/init ",
        description: "Initialize OpenCode project context for this workspace.",
    },
    OpenCodeAcpSlashCommand {
        name: "mcp",
        label: "/mcp",
        insertion_text: "/mcp ",
        description: "Inspect OpenCode MCP server status and tools.",
    },
    OpenCodeAcpSlashCommand {
        name: "permissions",
        label: "/permissions",
        insertion_text: "/permissions ",
        description: "Inspect or update OpenCode permission settings.",
    },
    OpenCodeAcpSlashCommand {
        name: "review",
        label: "/review",
        insertion_text: "/review ",
        description: "Ask OpenCode to review the current changes.",
    },
    OpenCodeAcpSlashCommand {
        name: "undo",
        label: "/undo",
        insertion_text: "/undo ",
        description: "Undo the latest OpenCode action when supported.",
    },
    OpenCodeAcpSlashCommand {
        name: "redo",
        label: "/redo",
        insertion_text: "/redo ",
        description: "Redo the latest OpenCode action when supported.",
    },
];

#[derive(Debug, Clone, Copy)]
struct CodexAcpSlashCommand {
    name: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct OpenCodeAcpSlashCommand {
    name: &'static str,
    label: &'static str,
    insertion_text: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct AcpSession {
    pub native_session_id: Option<String>,
    pub native_thread_id: Option<String>,
    pub native_resume_token: Option<String>,
    pub session_config_state: Option<vibex_core::ProviderSessionConfigState>,
    pub redacted_metadata: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone)]
pub struct AcpTurn {
    pub events: Vec<AcpEvent>,
    pub binding_update: Option<AcpSession>,
    pub completed: bool,
}

#[derive(Debug, Clone)]
pub enum AcpEvent {
    AssistantDelta {
        text_delta: String,
        chunk_index: u32,
        phase: Option<AgentMessagePhase>,
    },
    AssistantMessage {
        text: String,
        is_final: bool,
    },
    Reasoning {
        text: String,
        is_final: bool,
    },
    Plan {
        title: String,
        steps: Vec<PlanStepPayload>,
    },
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        status: ToolCallStatus,
        summary: String,
        input_summary: Option<String>,
        output_summary: Option<String>,
    },
    Canonical(NormalizedAgentEvent),
    PermissionRequest {
        /// Vibex request id pre-allocated by the runtime so provider-side
        /// permission resolution can find the pending native request again.
        request_id: Option<RequestId>,
        provider_request_id: Option<String>,
        risk_category: PermissionRiskCategory,
        title: String,
        details: Vec<PermissionActionDetail>,
        options: Vec<PermissionResponseOption>,
    },
    ElicitationRequest(ElicitationRequest),
    SystemNotice {
        level: SystemNoticeLevel,
        message: String,
    },
    Error {
        code: String,
        message: String,
        recoverable: bool,
        provider_correlation_id: Option<String>,
    },
    Unknown {
        event_kind: String,
    },
}

/// Slash command advertised by a live ACP agent through
/// `available_commands_update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpRuntimeCommand {
    pub name: String,
    pub description: Option<String>,
}

/// Product-safe evidence harvested from one stateless `initialize` +
/// `session/new` probe: model ids plus the session-level modes and
/// reasoning-effort levels the agent advertised for that session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpRuntimeSessionProbe {
    pub models: Vec<String>,
    pub modes: Vec<ProviderSessionConfigValue>,
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
    pub options: Vec<ProviderSessionConfigOption>,
}

#[async_trait]
pub trait AcpClient: Send + Sync {
    async fn list_auth_methods(
        &self,
        _agent_id: &vibex_core::AgentId,
        _provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        Err(VibexError::capability(
            "acp_auth_discovery_unsupported",
            "ACP authentication discovery is not supported by this adapter",
        ))
    }

    async fn authenticate_agent(
        &self,
        _request: AgentAuthenticateRequest,
    ) -> VibexResult<AgentAuthenticateResult> {
        Err(VibexError::capability(
            "acp_authenticate_unsupported",
            "ACP authentication is not supported by this adapter",
        ))
    }

    async fn cancel_agent_authentication(
        &self,
        _request: AgentAuthenticationCancelRequest,
    ) -> VibexResult<bool> {
        Err(VibexError::capability(
            "acp_authentication_cancel_unsupported",
            "cancelling ACP authentication is not supported by this adapter",
        ))
    }

    async fn complete_agent_authentication(
        &self,
        _request: AgentAuthenticationCompleteRequest,
    ) -> VibexResult<bool> {
        Err(VibexError::capability(
            "acp_authentication_complete_unsupported",
            "completing interactive ACP authentication is not supported by this adapter",
        ))
    }

    async fn logout_agent(&self, _request: AgentLogoutRequest) -> VibexResult<()> {
        Err(VibexError::capability(
            "acp_logout_unsupported",
            "ACP logout is not supported by this adapter",
        ))
    }

    async fn create_session(&self, request: AcpCreateSessionRequest) -> VibexResult<AcpSession>;

    async fn resume_session(&self, binding: ProviderBinding) -> VibexResult<AcpSession>;

    async fn list_sessions(
        &self,
        _provider_profile_id: &ProviderProfileId,
        _workspace_root: Option<&str>,
    ) -> VibexResult<Vec<ExternalSessionImportCandidate>> {
        Err(VibexError::capability(
            "acp_session_list_unsupported",
            "ACP native session listing is not supported by this adapter",
        ))
    }

    async fn import_session(&self, request: AcpImportSessionRequest) -> VibexResult<AcpSession> {
        let binding = ProviderBinding {
            session_id: request.session_id,
            provider_kind: ProviderKind::Acp,
            auth_source: vibex_core::RuntimeAuthSource::provider_profile(
                request.provider_profile_id,
            ),
            auth_source_revision: 0,
            native: ProviderNativeBinding {
                native_session_id: request.native_session_id,
                native_thread_id: None,
                native_resume_token: None,
                session_config_state: None,
                redacted_metadata: Vec::new(),
            },
            created_at_ms: unix_timestamp_ms(),
            updated_at_ms: unix_timestamp_ms(),
        };
        self.resume_session(binding).await
    }

    async fn send_turn(&self, request: AcpSendTurnRequest) -> VibexResult<AcpTurn>;

    async fn prepare_turn_execution(
        &self,
        _request: &AcpSendTurnRequest,
    ) -> VibexResult<Option<ProviderTurnExecutionIdentity>> {
        Ok(None)
    }

    /// Applies a session-scoped preferred/effective configuration patch. ACP
    /// runtimes own the attachment fence and wire operation selection; other
    /// adapters remain capability-aware by default.
    async fn update_session_runtime_config(
        &self,
        _request: SessionRuntimeConfigMutationRequest,
    ) -> VibexResult<SessionRuntimeConfigMutationResult> {
        Err(VibexError::capability(
            "acp_session_runtime_config_unsupported",
            "session runtime configuration mutation is not supported by this adapter",
        ))
    }

    async fn resolve_permission(&self, _request: AcpPermissionResolution) -> VibexResult<()> {
        Err(VibexError::capability(
            "acp_permission_resolution_unsupported",
            "ACP permission resolution is not supported by this adapter",
        ))
    }

    async fn resolve_elicitation(&self, _request: AcpElicitationResolution) -> VibexResult<()> {
        Err(VibexError::capability(
            "acp_elicitation_resolution_unsupported",
            "ACP elicitation resolution is not supported by this adapter",
        ))
    }

    async fn interrupt(&self, _binding: &ProviderBinding) -> VibexResult<()> {
        Err(VibexError::capability(
            "acp_interrupt_unsupported",
            "ACP interrupt is not supported by this adapter",
        ))
    }

    async fn close_session(&self, _binding: &ProviderBinding) -> VibexResult<()> {
        Ok(())
    }

    async fn list_runtime_models(
        &self,
        _provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<Vec<String>> {
        Err(VibexError::capability(
            "acp_runtime_model_probe_unsupported",
            "ACP runtime model probing is not supported by this adapter",
        ))
    }

    /// Stateless session-config probe. The default keeps model discovery
    /// working for clients that only implement `list_runtime_models`.
    async fn probe_runtime_session_config(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<AcpRuntimeSessionProbe> {
        Ok(AcpRuntimeSessionProbe {
            models: self.list_runtime_models(provider_profile_id).await?,
            modes: Vec::new(),
            reasoning_efforts: Vec::new(),
            options: Vec::new(),
        })
    }

    /// Stateless session-config probe for one concrete model. Clients without
    /// model-sensitive discovery support may reuse the Profile-level probe.
    async fn probe_runtime_session_config_for_model(
        &self,
        provider_profile_id: &ProviderProfileId,
        _model_id: &str,
    ) -> VibexResult<AcpRuntimeSessionProbe> {
        self.probe_runtime_session_config(provider_profile_id).await
    }

    /// Stateless Agent-level session option probe. Implementations may
    /// override this when the CLI can be launched from its Agent-owned
    /// command configuration without a Provider Profile.
    async fn probe_runtime_session_config_for_agent(
        &self,
        _agent_id: &vibex_core::AgentId,
    ) -> VibexResult<AcpRuntimeSessionProbe> {
        Err(VibexError::capability(
            "acp_agent_runtime_probe_unsupported",
            "this ACP adapter does not support Agent-level runtime option probing",
        ))
    }

    async fn list_runtime_model_capabilities(
        &self,
        _provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<Vec<AgentModelCapabilities>> {
        Ok(Vec::new())
    }

    async fn list_session_commands(
        &self,
        _session_id: &VibexSessionId,
    ) -> VibexResult<Option<Vec<AcpRuntimeCommand>>> {
        Ok(None)
    }
}

#[derive(Debug, Default)]
pub struct DisabledAcpClient;

impl DisabledAcpClient {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone)]
pub struct AcpImportSessionRequest {
    pub session_id: VibexSessionId,
    pub provider_profile_id: ProviderProfileId,
    pub native_session_id: Option<String>,
    pub workspace_root: String,
    pub runtime_resources: ProviderRuntimeResources,
}

#[async_trait]
impl AcpClient for DisabledAcpClient {
    async fn create_session(&self, _request: AcpCreateSessionRequest) -> VibexResult<AcpSession> {
        Err(VibexError::capability(
            "acp_runtime_unavailable",
            "ACP runtime client is not configured; run the explicit provider smoke setup before starting ACP sessions",
        ))
    }

    async fn resume_session(&self, _binding: ProviderBinding) -> VibexResult<AcpSession> {
        Err(VibexError::capability(
            "acp_runtime_unavailable",
            "ACP runtime client is not configured; run the explicit provider smoke setup before resuming ACP sessions",
        ))
    }

    async fn send_turn(&self, _request: AcpSendTurnRequest) -> VibexResult<AcpTurn> {
        Err(VibexError::capability(
            "acp_runtime_unavailable",
            "ACP runtime client is not configured; run the explicit provider smoke setup before sending ACP turns",
        ))
    }

    async fn update_session_runtime_config(
        &self,
        _request: SessionRuntimeConfigMutationRequest,
    ) -> VibexResult<SessionRuntimeConfigMutationResult> {
        Err(VibexError::capability(
            "acp_runtime_unavailable",
            "ACP runtime client is not configured; session runtime configuration cannot be changed",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeAcpRuntimeConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub cwd_template: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub features: Vec<String>,
}

impl OpenCodeAcpRuntimeConfig {
    pub fn from_acp_config(config: &AcpProviderConfig) -> VibexResult<Self> {
        if config.command.trim().is_empty() {
            return Err(VibexError::validation(
                "acp_opencode_command_empty",
                "OpenCode ACP runtime command must not be empty",
            ));
        }
        if config.args.iter().all(|arg| arg != "acp") {
            return Err(VibexError::validation(
                "acp_opencode_args_missing_acp",
                "OpenCode ACP runtime must be configured with the opencode acp subcommand",
            )
            .with_diagnostic("args", redacted_args_summary(&config.args)));
        }
        if let Some(cwd_template) = config.cwd_template.as_deref()
            && cwd_template != "{workspaceRoot}"
        {
            return Err(VibexError::validation(
                "acp_opencode_cwd_template_unsupported",
                "OpenCode ACP runtime currently supports only the {workspaceRoot} cwd template",
            )
            .with_diagnostic("cwdTemplate", cwd_template));
        }

        Ok(Self {
            command: PathBuf::from(config.command.clone()),
            args: config.args.clone(),
            cwd_template: config.cwd_template.clone(),
            model: config.models.first().cloned(),
            mode: config.modes.first().cloned(),
            features: config.features.clone(),
        })
    }

    fn stdio_args(&self) -> Vec<String> {
        self.args.clone()
    }
}

#[derive(Clone)]
pub struct OpenCodeAcpClient {
    runtime_config: OpenCodeAcpRuntimeConfig,
    handshake_timeout: Duration,
    prompt_timeout: Duration,
    active_session: Arc<Mutex<Option<OpenCodeAcpStdioSession>>>,
}

impl OpenCodeAcpClient {
    pub fn new(runtime_config: OpenCodeAcpRuntimeConfig) -> Self {
        Self {
            runtime_config,
            handshake_timeout: OPENCODE_ACP_HANDSHAKE_TIMEOUT,
            prompt_timeout: OPENCODE_ACP_PROMPT_TIMEOUT,
            active_session: Arc::new(Mutex::new(None)),
        }
    }

    fn resolve_runtime_workspace(&self, workspace_root: &str) -> VibexResult<PathBuf> {
        let raw = PathBuf::from(workspace_root);
        if !raw.is_absolute() {
            return Err(VibexError::validation(
                "validation/acp_opencode_workspace_relative",
                "OpenCode ACP runtime workspace must be an absolute path",
            )
            .with_diagnostic("workspaceRoot", workspace_root));
        }
        reject_forbidden_agent_smoke_workspace(&raw).map_err(|err| {
            VibexError::validation(
                "validation/acp_opencode_workspace_forbidden",
                "OpenCode ACP runtime workspace must stay outside the Vibex development root",
            )
            .with_diagnostic("sourceCode", err.code)
            .with_diagnostic("workspaceRoot", workspace_root)
        })?;
        let canonical = raw.canonicalize().map_err(|err| {
            VibexError::validation(
                "validation/acp_opencode_workspace_missing",
                "OpenCode ACP runtime workspace must exist before process startup",
            )
            .with_diagnostic("workspaceRoot", workspace_root)
            .with_diagnostic("error", err.to_string())
        })?;
        reject_forbidden_agent_smoke_workspace(&canonical).map_err(|err| {
            VibexError::validation(
                "validation/acp_opencode_workspace_forbidden",
                "OpenCode ACP runtime workspace must stay outside the Vibex development root",
            )
            .with_diagnostic("sourceCode", err.code)
            .with_diagnostic("workspaceRoot", canonical.display().to_string())
        })?;
        Ok(canonical)
    }

    fn ensure_binary(&self) -> VibexResult<()> {
        if self.runtime_config.command.is_absolute() && !self.runtime_config.command.is_file() {
            return Err(VibexError::process(
                "process/acp_opencode_binary_missing",
                "OpenCode ACP runtime binary was not found",
            )
            .with_diagnostic("command", self.runtime_config.command.display().to_string()));
        }
        Ok(())
    }

    fn run_startup_probe(&self, workspace_path: &Path) -> VibexResult<OpenCodeAcpProcessAttempt> {
        self.ensure_binary()?;
        let mut session = self.start_stdio_session(workspace_path)?;
        let initialize = session.initialize(self.handshake_timeout)?;
        let new_session = session.new_session(
            workspace_path,
            self.runtime_config.model.as_deref(),
            self.handshake_timeout,
        )?;
        let stderr_summary = session.stderr_summary();
        let exit_code = session.shutdown();

        Ok(OpenCodeAcpProcessAttempt {
            command: self.runtime_config.command.display().to_string(),
            args: self.runtime_config.stdio_args(),
            cwd: workspace_path.to_path_buf(),
            exit_code,
            timed_out: false,
            stdout_summary: Some(format!(
                "stdio initialize/session_new completed; protocolVersion={}",
                initialize.protocol_version
            )),
            stderr_summary,
            raw_output_stored: false,
            session_config_snapshot: new_session.config_snapshot,
        })
    }

    fn start_stdio_session(&self, workspace_path: &Path) -> VibexResult<OpenCodeAcpStdioSession> {
        self.ensure_binary()?;
        OpenCodeAcpStdioSession::spawn(
            &self.runtime_config.command,
            self.runtime_config.stdio_args(),
            workspace_path,
        )
    }
}

fn acp_prompt_content(text: &str, attachments: &[ProviderTurnAttachment]) -> Vec<Value> {
    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(json!({
            "type": "text",
            "text": text
        }));
    }

    for attachment in attachments {
        if attachment.is_image()
            && let Some(path) = attachment.local_path.as_ref()
        {
            content.push(json!({
                "type": "localImage",
                "path": path.display().to_string()
            }));
        } else if let Some(uri) = attachment.uri.as_ref() {
            content.push(json!({
                "type": "image",
                "url": uri
            }));
        }
    }

    if content.is_empty() {
        content.push(json!({
            "type": "text",
            "text": text
        }));
    }
    content
}

impl std::fmt::Debug for OpenCodeAcpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenCodeAcpClient")
            .field("runtime_config", &self.runtime_config)
            .field("handshake_timeout", &self.handshake_timeout)
            .field("prompt_timeout", &self.prompt_timeout)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AcpClient for OpenCodeAcpClient {
    async fn create_session(&self, request: AcpCreateSessionRequest) -> VibexResult<AcpSession> {
        let workspace_path = self.resolve_runtime_workspace(&request.workspace_root)?;
        let mut session = self.start_stdio_session(&workspace_path)?;
        let initialize = session.initialize(self.handshake_timeout)?;
        let model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .or(self.runtime_config.model.as_deref());
        let new_session = session.new_session(&workspace_path, model, self.handshake_timeout)?;
        let stderr_summary = session.stderr_summary();

        let acp_session = AcpSession {
            native_session_id: Some(new_session.session_id.clone()),
            native_thread_id: None,
            native_resume_token: None,
            session_config_state: None,
            redacted_metadata: opencode_session_metadata(
                &self.runtime_config,
                &workspace_path,
                &initialize,
                &new_session.config_snapshot,
                stderr_summary.as_deref(),
            ),
        };

        let mut active_session = self.active_session.lock().map_err(|_| {
            VibexError::provider(
                "provider/acp_opencode_session_lock_poisoned",
                "OpenCode ACP runtime session state could not be locked",
            )
        })?;
        *active_session = Some(session);
        Ok(acp_session)
    }

    async fn resume_session(&self, _binding: ProviderBinding) -> VibexResult<AcpSession> {
        Err(VibexError::capability(
            "capability/acp_opencode_resume_unsupported",
            "OpenCode ACP runtime resume is not supported until the session handshake is implemented",
        ))
    }

    async fn send_turn(&self, request: AcpSendTurnRequest) -> VibexResult<AcpTurn> {
        let _workspace_path = self.resolve_runtime_workspace(&request.workspace_root)?;
        let native_session_id = request
            .binding
            .native
            .native_session_id
            .clone()
            .ok_or_else(|| {
                VibexError::provider(
                    "provider/acp_opencode_session_missing",
                    "OpenCode ACP runtime cannot send a turn without a native session id",
                )
            })?;
        let mut active_session = self.active_session.lock().map_err(|_| {
            VibexError::provider(
                "provider/acp_opencode_session_lock_poisoned",
                "OpenCode ACP runtime session state could not be locked",
            )
        })?;
        let session = active_session.as_mut().ok_or_else(|| {
            VibexError::provider(
                "provider/acp_opencode_session_not_active",
                "OpenCode ACP runtime session process is not active",
            )
        })?;
        if session.native_session_id.as_deref() != Some(native_session_id.as_str()) {
            return Err(VibexError::provider(
                "provider/acp_opencode_session_mismatch",
                "OpenCode ACP runtime active session does not match the provider binding",
            ));
        }

        let prompt_result = session.prompt(
            &native_session_id,
            &request.text,
            &request.attachments,
            self.prompt_timeout,
        )?;
        let mut events = Vec::new();
        let permission_requested = !prompt_result.permission_requests.is_empty();
        events.extend(
            prompt_result
                .permission_requests
                .into_iter()
                .map(|request| AcpEvent::PermissionRequest {
                    request_id: None,
                    provider_request_id: request.provider_request_id,
                    risk_category: request.risk_category,
                    title: request.title,
                    details: request.details,
                    options: Vec::new(),
                }),
        );
        if !prompt_result.reasoning_text.trim().is_empty() {
            events.push(AcpEvent::Reasoning {
                text: prompt_result.reasoning_text,
                is_final: true,
            });
        }
        if !prompt_result.assistant_text.trim().is_empty() {
            events.push(AcpEvent::AssistantMessage {
                text: prompt_result.assistant_text,
                is_final: true,
            });
        }
        if permission_requested {
            events.push(AcpEvent::SystemNotice {
                level: SystemNoticeLevel::Warning,
                message: "OpenCode ACP requested permission; Vibex recorded the request, but provider-side approval callbacks are not supported yet, so the underlying ACP request was conservatively cancelled".to_string(),
            });
        }
        if events.is_empty() {
            events.push(AcpEvent::SystemNotice {
                level: SystemNoticeLevel::Info,
                message: "OpenCode ACP turn completed without assistant text".to_string(),
            });
        }
        if let Some(stop_reason) = prompt_result.stop_reason {
            events.push(AcpEvent::SystemNotice {
                level: SystemNoticeLevel::Info,
                message: format!("OpenCode ACP stop reason: {stop_reason}"),
            });
        }

        Ok(AcpTurn {
            events,
            binding_update: None,
            completed: !permission_requested,
        })
    }
}

struct OpenCodeAcpStdioSession {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<String>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    next_id: u64,
    native_session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct OpenCodeAcpInitializeSummary {
    protocol_version: i64,
    agent_name: Option<String>,
    agent_version: Option<String>,
}

#[derive(Debug, Clone)]
struct OpenCodeAcpNewSessionResult {
    session_id: String,
    config_snapshot: OpenCodeAcpSessionConfigSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeAcpSessionConfigSnapshot {
    pub status: OpenCodeAcpSessionConfigSnapshotStatus,
    pub response_keys: Vec<String>,
    pub config_option_categories: Vec<String>,
    pub model_option_count: usize,
    pub current_model_id: Option<String>,
    pub current_model_name: Option<String>,
    pub model_options_sample: Vec<OpenCodeAcpModelOptionSummary>,
    pub raw_payload_stored: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeAcpSessionConfigSnapshotStatus {
    Available,
    NoModelOptions,
    Unavailable,
}

impl OpenCodeAcpSessionConfigSnapshotStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NoModelOptions => "no_model_options",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeAcpModelOptionSummary {
    pub model_id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct OpenCodeAcpPromptResult {
    assistant_text: String,
    reasoning_text: String,
    stop_reason: Option<String>,
    permission_requests: Vec<OpenCodeAcpPermissionRequestSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenCodeAcpPermissionRequestSummary {
    provider_request_id: Option<String>,
    risk_category: PermissionRiskCategory,
    title: String,
    details: Vec<PermissionActionDetail>,
}

impl OpenCodeAcpStdioSession {
    fn spawn(command: &Path, args: Vec<String>, workspace_path: &Path) -> VibexResult<Self> {
        let mut command_builder = Command::new(command);
        command_builder
            .args(&args)
            .current_dir(workspace_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        process_environment::sanitize_inherited_appimage_environment(&mut command_builder);
        let mut child = command_builder.spawn().map_err(|err| {
            VibexError::process(
                "process/acp_opencode_spawn_failed",
                "OpenCode ACP runtime process could not be started",
            )
            .with_diagnostic("command", command.display().to_string())
            .with_diagnostic("args", redacted_args_summary(&args))
            .with_diagnostic("cwd", workspace_path.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            VibexError::process(
                "process/acp_opencode_stdio_unavailable",
                "OpenCode ACP runtime stdin was not available",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            VibexError::process(
                "process/acp_opencode_stdio_unavailable",
                "OpenCode ACP runtime stdout was not available",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            VibexError::process(
                "process/acp_opencode_stdio_unavailable",
                "OpenCode ACP runtime stderr was not available",
            )
        })?;

        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if stdout_tx.send(line).is_err() {
                    break;
                }
            }
        });

        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_lines_for_thread = Arc::clone(&stderr_lines);
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(mut lines) = stderr_lines_for_thread.lock()
                    && lines.len() < 20
                {
                    lines.push(line);
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout_rx,
            stderr_lines,
            next_id: 1,
            native_session_id: None,
        })
    }

    fn initialize(&mut self, timeout: Duration) -> VibexResult<OpenCodeAcpInitializeSummary> {
        let result = self.send_request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": false,
                        "writeTextFile": false
                    },
                    "terminal": false
                },
                "clientInfo": {
                    "name": "vibex",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
            timeout,
            None,
        )?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                VibexError::provider(
                    "provider/acp_opencode_handshake_unsupported",
                    "OpenCode ACP initialize response did not include a protocol version",
                )
                .with_diagnostic("responseKeys", value_keys_summary(&result))
            })?;
        Ok(OpenCodeAcpInitializeSummary {
            protocol_version,
            agent_name: result
                .get("agentInfo")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
            agent_version: result
                .get("agentInfo")
                .and_then(|info| info.get("version"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
        })
    }

    fn new_session(
        &mut self,
        workspace_path: &Path,
        model: Option<&str>,
        timeout: Duration,
    ) -> VibexResult<OpenCodeAcpNewSessionResult> {
        let mut params = json!({
            "cwd": workspace_path.display().to_string(),
            "mcpServers": []
        });
        if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty())
            && let Some(object) = params.as_object_mut()
        {
            object.insert("model".to_string(), Value::String(model.to_string()));
        }
        let result = self.send_request("session/new", params, timeout, None)?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                VibexError::provider(
                    "provider/acp_opencode_protocol_mismatch",
                    "OpenCode ACP session/new response did not include a session id",
                )
                .with_diagnostic("responseKeys", value_keys_summary(&result))
            })?
            .to_string();
        self.native_session_id = Some(session_id.clone());
        Ok(OpenCodeAcpNewSessionResult {
            session_id,
            config_snapshot: extract_session_config_snapshot(&result),
        })
    }

    fn prompt(
        &mut self,
        session_id: &str,
        text: &str,
        attachments: &[ProviderTurnAttachment],
        timeout: Duration,
    ) -> VibexResult<OpenCodeAcpPromptResult> {
        let mut prompt_result = OpenCodeAcpPromptResult::default();
        let result = self.send_request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": acp_prompt_content(text, attachments)
            }),
            timeout,
            Some(&mut prompt_result),
        )?;
        prompt_result.stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        Ok(prompt_result)
    }

    fn send_request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        mut prompt_result: Option<&mut OpenCodeAcpPromptResult>,
    ) -> VibexResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;

        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(VibexError::process(
                    "process/acp_opencode_start_timeout",
                    "OpenCode ACP runtime request timed out before a JSON-RPC response",
                )
                .with_diagnostic("method", method)
                .with_diagnostic("timeoutMs", timeout.as_millis().to_string())
                .with_diagnostic("stderrSummary", self.stderr_summary().unwrap_or_default()));
            }
            if let Some(status) = self.child.try_wait().map_err(|err| {
                VibexError::process(
                    "process/acp_opencode_wait_failed",
                    "OpenCode ACP runtime process status could not be checked",
                )
                .with_diagnostic("error", err.to_string())
            })? {
                return Err(VibexError::process(
                    "process/acp_opencode_exited_before_handshake",
                    "OpenCode ACP runtime exited before returning the expected JSON-RPC response",
                )
                .with_diagnostic("method", method)
                .with_diagnostic(
                    "exitCode",
                    status
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".to_string()),
                )
                .with_diagnostic("stderrSummary", self.stderr_summary().unwrap_or_default()));
            }

            match self.stdout_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    let message = parse_json_rpc_line(&line)?;
                    if let Some(incoming_method) = message.get("method").and_then(Value::as_str) {
                        if message.get("id").is_some() {
                            self.respond_to_incoming_request(
                                &message,
                                incoming_method,
                                prompt_result.as_deref_mut(),
                            )?;
                        } else if incoming_method == "session/update"
                            && let Some(result) = prompt_result.as_deref_mut()
                        {
                            collect_session_update(result, message.get("params"));
                        }
                        continue;
                    }
                    if message.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(error) = message.get("error") {
                        return Err(VibexError::provider(
                            "provider/acp_opencode_protocol_error",
                            "OpenCode ACP returned a JSON-RPC error",
                        )
                        .with_diagnostic("method", method)
                        .with_diagnostic(
                            "rpcCode",
                            error
                                .get("code")
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| "unknown".to_string()),
                        )
                        .with_diagnostic(
                            "rpcMessage",
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .map(redact_summary)
                                .unwrap_or_else(|| "unknown".to_string()),
                        ));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(VibexError::process(
                        "process/acp_opencode_stdout_closed",
                        "OpenCode ACP runtime stdout closed before a JSON-RPC response",
                    )
                    .with_diagnostic("method", method)
                    .with_diagnostic("stderrSummary", self.stderr_summary().unwrap_or_default()));
                }
            }
        }
    }

    fn respond_to_incoming_request(
        &mut self,
        message: &Value,
        method: &str,
        prompt_result: Option<&mut OpenCodeAcpPromptResult>,
    ) -> VibexResult<()> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        if method == "session/request_permission" {
            if let Some(prompt_result) = prompt_result {
                prompt_result
                    .permission_requests
                    .push(summarize_opencode_permission_request(message));
            }
            self.write_json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "outcome": {
                        "outcome": "cancelled"
                    }
                }
            }))
        } else {
            self.write_json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }))
        }
    }

    fn write_json(&mut self, value: &Value) -> VibexResult<()> {
        let serialized = serde_json::to_string(value).map_err(|err| {
            VibexError::provider(
                "provider/acp_opencode_request_encode_failed",
                "OpenCode ACP JSON-RPC request could not be encoded",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        writeln!(self.stdin, "{serialized}").map_err(|err| {
            VibexError::process(
                "process/acp_opencode_stdin_write_failed",
                "OpenCode ACP runtime stdin write failed",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        self.stdin.flush().map_err(|err| {
            VibexError::process(
                "process/acp_opencode_stdin_write_failed",
                "OpenCode ACP runtime stdin flush failed",
            )
            .with_diagnostic("error", err.to_string())
        })
    }

    fn stderr_summary(&self) -> Option<String> {
        let lines = self.stderr_lines.lock().ok()?;
        if lines.is_empty() {
            return None;
        }
        let joined = lines.join("\n");
        Some(redact_summary(&joined))
    }

    fn shutdown(mut self) -> Option<i32> {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        self.child.wait().ok().and_then(|status| status.code())
    }
}

impl Drop for OpenCodeAcpStdioSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_json_rpc_line(line: &str) -> VibexResult<Value> {
    let value = serde_json::from_str::<Value>(line).map_err(|err| {
        VibexError::provider(
            "provider/acp_opencode_protocol_mismatch",
            "OpenCode ACP runtime emitted non-JSON-RPC stdout",
        )
        .with_diagnostic("error", err.to_string())
        .with_diagnostic("lineSummary", redact_summary(line))
    })?;
    if !value.is_object() {
        return Err(VibexError::provider(
            "provider/acp_opencode_protocol_mismatch",
            "OpenCode ACP runtime emitted a non-object JSON-RPC message",
        ));
    }
    Ok(value)
}

fn collect_session_update(result: &mut OpenCodeAcpPromptResult, params: Option<&Value>) {
    let Some(update) = params.and_then(|params| params.get("update")) else {
        return;
    };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return;
    };
    let text = update
        .get("content")
        .and_then(|content| {
            if content.get("type").and_then(Value::as_str) == Some("text") {
                content.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .unwrap_or_default();

    match kind {
        "agent_message_chunk" => result.assistant_text.push_str(text),
        "agent_thought_chunk" => result.reasoning_text.push_str(text),
        _ => {}
    }
}

fn summarize_opencode_permission_request(message: &Value) -> OpenCodeAcpPermissionRequestSummary {
    let params = message.get("params");
    let tool_call = params.and_then(|params| params.get("toolCall"));
    let provider_request_id = message
        .get("id")
        .and_then(json_rpc_id_to_redacted_string)
        .filter(|value| !value.is_empty());
    let title = permission_request_title(params, tool_call);
    let risk_source = permission_risk_source(params, tool_call, &title);
    let mut details = Vec::new();

    if let Some(request_id) = provider_request_id.as_deref() {
        push_permission_detail(&mut details, "requestId", request_id);
    }
    if let Some(tool_call_id) = permission_string_field(tool_call, &["toolCallId", "id"]) {
        push_permission_detail(&mut details, "toolCallId", &tool_call_id);
    }
    if let Some(tool_kind) = permission_string_field(
        tool_call,
        &["kind", "name", "toolName", "tool", "type", "title"],
    ) {
        push_permission_detail(&mut details, "tool", &tool_kind);
    }
    if let Some(options) = params
        .and_then(|params| params.get("options"))
        .and_then(Value::as_array)
    {
        push_permission_detail(&mut details, "optionCount", &options.len().to_string());
        let option_kinds = options
            .iter()
            .filter_map(|option| permission_string_field(Some(option), &["kind", "name", "type"]))
            .take(OPENCODE_ACP_PERMISSION_DETAIL_LIMIT)
            .collect::<Vec<_>>();
        if !option_kinds.is_empty() {
            push_permission_detail(&mut details, "optionKinds", &option_kinds.join(","));
        }
    }
    if details.is_empty() {
        push_permission_detail(&mut details, "source", "OpenCode ACP");
    }

    OpenCodeAcpPermissionRequestSummary {
        provider_request_id,
        risk_category: infer_permission_risk_category(&risk_source),
        title,
        details,
    }
}

fn permission_request_title(params: Option<&Value>, tool_call: Option<&Value>) -> String {
    let raw_title = permission_string_field(tool_call, &["title", "name", "kind", "toolName"])
        .or_else(|| permission_string_field(params, &["title", "name", "kind"]));
    let Some(raw_title) = raw_title else {
        return "OpenCode ACP permission request".to_string();
    };
    let title = bounded_redacted_value(&raw_title);
    if title.is_empty() || title == REDACTED_SENSITIVE_OUTPUT {
        "OpenCode ACP permission request".to_string()
    } else {
        title
    }
}

fn permission_risk_source(
    params: Option<&Value>,
    tool_call: Option<&Value>,
    title: &str,
) -> String {
    let mut candidates = vec![title.to_string()];
    for key in [
        "kind",
        "name",
        "toolName",
        "tool",
        "type",
        "title",
        "description",
    ] {
        if let Some(value) = permission_string_field(tool_call, &[key]) {
            candidates.push(value);
        }
        if let Some(value) = permission_string_field(params, &[key]) {
            candidates.push(value);
        }
    }
    candidates.join(" ")
}

fn permission_string_field(value: Option<&Value>, keys: &[&str]) -> Option<String> {
    let object = value?.as_object()?;
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn json_rpc_id_to_redacted_string(value: &Value) -> Option<String> {
    let raw = if let Some(value) = value.as_str() {
        value.to_string()
    } else if let Some(value) = value.as_i64() {
        value.to_string()
    } else {
        value.as_u64()?.to_string()
    };
    Some(bounded_redacted_value(&raw))
}

fn push_permission_detail(details: &mut Vec<PermissionActionDetail>, label: &str, value: &str) {
    if details.len() >= OPENCODE_ACP_PERMISSION_DETAIL_LIMIT {
        return;
    }
    let value = bounded_redacted_value(value);
    if value.is_empty() {
        return;
    }
    details.push(PermissionActionDetail {
        label: label.to_string(),
        value,
    });
}

fn bounded_redacted_value(value: &str) -> String {
    let value = redact_summary(value);
    if value.len() > OPENCODE_ACP_PERMISSION_VALUE_LIMIT {
        let prefix: String = value
            .chars()
            .take(OPENCODE_ACP_PERMISSION_VALUE_LIMIT)
            .collect();
        format!("{prefix}...(truncated)")
    } else {
        value
    }
}

pub(crate) fn infer_permission_risk_category(value: &str) -> PermissionRiskCategory {
    let lower = value.to_ascii_lowercase();
    if lower.contains("git reset")
        || lower.contains("git clean")
        || lower.contains("git rebase")
        || lower.contains("git checkout")
    {
        PermissionRiskCategory::GitDestructive
    } else if lower.contains("delete")
        || lower.contains("remove")
        || lower.contains("move")
        || lower.contains("unlink")
        || lower.contains(" rmdir")
        || lower.contains(" rm ")
        || lower.starts_with("rm ")
    {
        PermissionRiskCategory::FileDeleteOrMove
    } else if lower.contains("write")
        || lower.contains("edit")
        || lower.contains("create")
        || lower.contains("patch")
        || lower.contains("save")
    {
        PermissionRiskCategory::FileWrite
    } else if lower.contains("network")
        || lower.contains("web")
        || lower.contains("http")
        || lower.contains("curl")
        || lower.contains("wget")
        || lower.contains("fetch")
    {
        PermissionRiskCategory::Network
    } else if lower.contains("read")
        && (lower.contains(".env")
            || lower.contains("secret")
            || lower.contains("credential")
            || lower.contains("private key")
            || lower.contains("private_key"))
    {
        PermissionRiskCategory::FileReadSensitive
    } else if lower.contains("command")
        || lower.contains("terminal")
        || lower.contains("shell")
        || lower.contains("bash")
        || lower.contains("exec")
        || lower.contains("run")
    {
        PermissionRiskCategory::Command
    } else {
        PermissionRiskCategory::CustomTool
    }
}

fn value_keys_summary(value: &Value) -> String {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_else(|| "non-object".to_string())
}

fn extract_session_config_snapshot(result: &Value) -> OpenCodeAcpSessionConfigSnapshot {
    let response_keys = value_key_list(result);
    let config_options = result.get("configOptions").and_then(Value::as_array);
    let config_option_categories = config_options
        .map(|entries| {
            let mut categories = Vec::new();
            for entry in entries {
                let Some(category) = entry.get("category").and_then(Value::as_str) else {
                    continue;
                };
                let category = redact_summary(category);
                if category.is_empty() || categories.iter().any(|value| value == &category) {
                    continue;
                }
                categories.push(category);
                if categories.len() >= OPENCODE_ACP_CATEGORY_SAMPLE_LIMIT {
                    break;
                }
            }
            categories
        })
        .unwrap_or_default();
    let model_config_option = config_options.and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.get("category").and_then(Value::as_str) == Some("model"))
    });
    let nested_models = result.get("models").filter(|value| value.is_object());
    let raw_model_options = result
        .get("availableModels")
        .and_then(Value::as_array)
        .or_else(|| {
            nested_models
                .and_then(|models| models.get("availableModels"))
                .and_then(Value::as_array)
        })
        .or_else(|| {
            model_config_option
                .and_then(|entry| entry.get("options"))
                .and_then(Value::as_array)
        });
    let current_model_id = result
        .get("currentModelId")
        .and_then(Value::as_str)
        .or_else(|| {
            nested_models
                .and_then(|models| models.get("currentModelId"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            model_config_option
                .and_then(|entry| entry.get("currentValue"))
                .and_then(Value::as_str)
        })
        .map(redact_summary)
        .filter(|value| !value.is_empty());
    let model_option_count = raw_model_options.map_or(0, |models| models.len());
    let model_options_sample = raw_model_options
        .map(|models| {
            models
                .iter()
                .filter_map(model_option_summary)
                .take(OPENCODE_ACP_MODEL_SAMPLE_LIMIT)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_model_name = current_model_id.as_deref().and_then(|current| {
        raw_model_options.and_then(|models| {
            models.iter().find_map(|model| {
                let summary = model_option_summary(model)?;
                if summary.model_id == current {
                    summary.name
                } else {
                    None
                }
            })
        })
    });
    let status = if model_option_count > 0 || current_model_id.is_some() {
        OpenCodeAcpSessionConfigSnapshotStatus::Available
    } else if config_options.is_some() {
        OpenCodeAcpSessionConfigSnapshotStatus::NoModelOptions
    } else {
        OpenCodeAcpSessionConfigSnapshotStatus::Unavailable
    };

    OpenCodeAcpSessionConfigSnapshot {
        status,
        response_keys,
        config_option_categories,
        model_option_count,
        current_model_id,
        current_model_name,
        model_options_sample,
        raw_payload_stored: false,
    }
}

fn value_key_list(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys
        })
        .unwrap_or_default()
}

fn model_option_summary(value: &Value) -> Option<OpenCodeAcpModelOptionSummary> {
    if let Some(model_id) = value.as_str() {
        let model_id = redact_summary(model_id);
        if model_id.is_empty() {
            return None;
        }
        return Some(OpenCodeAcpModelOptionSummary {
            model_id,
            name: None,
        });
    }

    let model_id = value
        .get("modelId")
        .and_then(Value::as_str)
        .or_else(|| value.get("value").and_then(Value::as_str))
        .or_else(|| value.get("id").and_then(Value::as_str))
        .map(redact_summary)
        .filter(|value| !value.is_empty())?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(redact_summary)
        .filter(|value| !value.is_empty());

    Some(OpenCodeAcpModelOptionSummary { model_id, name })
}

fn opencode_session_metadata(
    runtime_config: &OpenCodeAcpRuntimeConfig,
    workspace_path: &Path,
    initialize: &OpenCodeAcpInitializeSummary,
    config_snapshot: &OpenCodeAcpSessionConfigSnapshot,
    stderr_summary: Option<&str>,
) -> Vec<ProviderBindingMetadata> {
    let mut metadata = vec![
        ProviderBindingMetadata {
            key: "transport".to_string(),
            value: "stdio_ndjson".to_string(),
        },
        ProviderBindingMetadata {
            key: "command".to_string(),
            value: runtime_config.command.display().to_string(),
        },
        ProviderBindingMetadata {
            key: "args".to_string(),
            value: redacted_args_summary(&runtime_config.stdio_args()),
        },
        ProviderBindingMetadata {
            key: "workspacePath".to_string(),
            value: workspace_path.display().to_string(),
        },
        ProviderBindingMetadata {
            key: "protocolVersion".to_string(),
            value: initialize.protocol_version.to_string(),
        },
        ProviderBindingMetadata {
            key: "sessionConfigSnapshotStatus".to_string(),
            value: config_snapshot.status.as_str().to_string(),
        },
        ProviderBindingMetadata {
            key: "sessionConfigResponseKeys".to_string(),
            value: config_snapshot.response_keys.join(","),
        },
        ProviderBindingMetadata {
            key: "sessionConfigCategories".to_string(),
            value: config_snapshot.config_option_categories.join(","),
        },
        ProviderBindingMetadata {
            key: "sessionConfigModelOptionCount".to_string(),
            value: config_snapshot.model_option_count.to_string(),
        },
    ];
    if let Some(current_model_id) = config_snapshot.current_model_id.as_deref() {
        metadata.push(ProviderBindingMetadata {
            key: "sessionConfigCurrentModelId".to_string(),
            value: current_model_id.to_string(),
        });
    }
    if let Some(current_model_name) = config_snapshot.current_model_name.as_deref() {
        metadata.push(ProviderBindingMetadata {
            key: "sessionConfigCurrentModelName".to_string(),
            value: current_model_name.to_string(),
        });
    }
    if let Some(agent_name) = initialize.agent_name.as_deref() {
        metadata.push(ProviderBindingMetadata {
            key: "agentName".to_string(),
            value: redact_summary(agent_name),
        });
    }
    if let Some(agent_version) = initialize.agent_version.as_deref() {
        metadata.push(ProviderBindingMetadata {
            key: "agentVersion".to_string(),
            value: redact_summary(agent_version),
        });
    }
    if let Some(model) = runtime_config.model.as_deref() {
        metadata.push(ProviderBindingMetadata {
            key: "configuredModel".to_string(),
            value: model.to_string(),
        });
    }
    if let Some(mode) = runtime_config.mode.as_deref() {
        metadata.push(ProviderBindingMetadata {
            key: "configuredMode".to_string(),
            value: mode.to_string(),
        });
    }
    if let Some(stderr_summary) = stderr_summary {
        metadata.push(ProviderBindingMetadata {
            key: "stderrSummary".to_string(),
            value: redact_summary(stderr_summary),
        });
    }
    metadata
}

#[derive(Debug, Clone)]
pub struct AcpCreateSessionRequest {
    pub session_id: vibex_core::VibexSessionId,
    pub provider_profile_id: vibex_core::ProviderProfileId,
    pub model: Option<String>,
    pub workspace_root: String,
    pub runtime_resources: ProviderRuntimeResources,
}

#[derive(Clone)]
pub struct AcpSendTurnRequest {
    pub session_id: vibex_core::VibexSessionId,
    pub message_submission_id: Option<MessageSubmissionId>,
    pub required_runtime: Option<SessionRuntimeSelection>,
    pub text: String,
    pub attachments: Vec<ProviderTurnAttachment>,
    pub workspace_root: String,
    pub binding: ProviderBinding,
    pub runtime_resources: ProviderRuntimeResources,
    pub execution_identity: Option<ProviderTurnExecutionIdentity>,
    /// Streams translated ACP events while the turn is still running. When
    /// absent, adapters buffer events into the returned [`AcpTurn`].
    pub event_sender: Option<tokio::sync::mpsc::UnboundedSender<AcpEvent>>,
    pub usage_execution_context: Option<AgentUsageExecutionContext>,
    pub usage_counter_origin: AgentUsageCounterOrigin,
    pub usage_event_sender: Option<tokio::sync::mpsc::UnboundedSender<AgentUsageTelemetryEvent>>,
}

impl fmt::Debug for AcpSendTurnRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpSendTurnRequest")
            .field("session_id", &self.session_id)
            .field(
                "has_message_submission_id",
                &self.message_submission_id.is_some(),
            )
            .field("has_required_runtime", &self.required_runtime.is_some())
            .field("has_text", &!self.text.is_empty())
            .field("attachment_count", &self.attachments.len())
            .field("has_workspace_root", &!self.workspace_root.is_empty())
            .field("has_binding", &true)
            .field(
                "mcp_server_count",
                &self.runtime_resources.mcp_servers.len(),
            )
            .field("skill_count", &self.runtime_resources.skills.len())
            .field("has_execution_identity", &self.execution_identity.is_some())
            .field("has_event_sender", &self.event_sender.is_some())
            .field(
                "has_usage_execution_context",
                &self.usage_execution_context.is_some(),
            )
            .field("usage_counter_origin", &self.usage_counter_origin)
            .field("has_usage_event_sender", &self.usage_event_sender.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AcpPermissionResolution {
    pub binding: ProviderBinding,
    pub resolution: vibex_core::PermissionResolution,
}

#[derive(Debug, Clone)]
pub struct AcpElicitationResolution {
    pub binding: ProviderBinding,
    pub execution_identity: ProviderTurnExecutionIdentity,
    pub resolution: vibex_core::ElicitationResolution,
}

#[derive(Clone)]
pub struct AcpAgentProvider {
    client: Arc<dyn AcpClient>,
    config_service: Option<ProviderConfigService>,
}

impl AcpAgentProvider {
    pub fn new(client: Arc<dyn AcpClient>) -> Self {
        Self {
            client,
            config_service: None,
        }
    }

    pub fn with_config_service(
        client: Arc<dyn AcpClient>,
        config_service: ProviderConfigService,
    ) -> Self {
        Self {
            client,
            config_service: Some(config_service),
        }
    }

    pub fn capabilities_static() -> ProviderCapabilities {
        let mut capabilities =
            ProviderCapabilities::conservative(ProviderKind::Acp, "acp-foundation-static");
        capabilities.slash_commands = true;
        capabilities.skills = true;
        capabilities.elicitation = true;
        capabilities
    }
}

impl std::fmt::Debug for AcpAgentProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpAgentProvider")
            .field("capabilities", &Self::capabilities_static())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AgentProvider for AcpAgentProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Acp
    }

    fn capabilities(&self) -> ProviderCapabilities {
        Self::capabilities_static()
    }

    fn capabilities_for_profile(
        &self,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> ProviderCapabilities {
        match (provider_profile_id, self.config_service.as_ref()) {
            (Some(profile_id), Some(service)) => service
                .get_acp_profile_config(profile_id.clone())
                .map(|config| acp_capabilities_from_config(&config))
                .unwrap_or_else(|_| {
                    ProviderCapabilities::conservative(
                        ProviderKind::Acp,
                        "acp_profile_config_missing",
                    )
                }),
            _ => Self::capabilities_static(),
        }
    }

    async fn list_auth_methods(
        &self,
        agent_id: &vibex_core::AgentId,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        self.client
            .list_auth_methods(agent_id, provider_profile_id)
            .await
    }

    async fn authenticate_agent(
        &self,
        request: AgentAuthenticateRequest,
    ) -> VibexResult<AgentAuthenticateResult> {
        self.client.authenticate_agent(request).await
    }

    async fn cancel_agent_authentication(
        &self,
        request: AgentAuthenticationCancelRequest,
    ) -> VibexResult<bool> {
        self.client.cancel_agent_authentication(request).await
    }

    async fn complete_agent_authentication(
        &self,
        request: AgentAuthenticationCompleteRequest,
    ) -> VibexResult<bool> {
        self.client.complete_agent_authentication(request).await
    }

    async fn logout_agent(&self, request: AgentLogoutRequest) -> VibexResult<()> {
        self.client.logout_agent(request).await
    }

    async fn list_models(
        &self,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<AgentModelListResponse> {
        let Some(profile_id) = provider_profile_id else {
            return Ok(AgentModelListResponse {
                agent_id: None,
                provider_kind: ProviderKind::Acp,
                provider_profile_id: None,
                models: Vec::new(),
                reasoning_efforts: Vec::new(),
                model_capabilities: Vec::new(),
                source: AgentModelListSource::Unavailable,
                diagnostics: vec![ProviderBindingMetadata {
                    key: "modelList".to_string(),
                    value: "profile_required".to_string(),
                }],
            });
        };
        let Some(service) = self.config_service.as_ref() else {
            return Ok(AgentModelListResponse {
                agent_id: None,
                provider_kind: ProviderKind::Acp,
                provider_profile_id: Some(profile_id.clone()),
                models: Vec::new(),
                reasoning_efforts: Vec::new(),
                model_capabilities: Vec::new(),
                source: AgentModelListSource::Unavailable,
                diagnostics: vec![ProviderBindingMetadata {
                    key: "modelList".to_string(),
                    value: "config_service_unavailable".to_string(),
                }],
            });
        };
        let config = service.get_acp_profile_config(profile_id.clone())?;
        let models = normalize_model_list(config.models);
        if !models.is_empty() {
            let is_codex = service
                .get_profile(profile_id)?
                .is_some_and(|profile| profile.agent_id.as_str() == "codex");
            let (mut model_capabilities, capability_diagnostic) = if is_codex {
                match self
                    .client
                    .list_runtime_model_capabilities(profile_id)
                    .await
                {
                    Ok(capabilities) => (capabilities, "codex_app_server"),
                    Err(_) => (Vec::new(), "codex_app_server_unavailable"),
                }
            } else {
                (Vec::new(), "not_requested")
            };
            model_capabilities.retain(|capability| {
                models
                    .iter()
                    .any(|configured| configured == &capability.model)
            });
            return Ok(AgentModelListResponse {
                agent_id: None,
                provider_kind: ProviderKind::Acp,
                provider_profile_id: Some(profile_id.clone()),
                models,
                reasoning_efforts: Vec::new(),
                model_capabilities,
                source: AgentModelListSource::Configured,
                diagnostics: vec![
                    ProviderBindingMetadata {
                        key: "modelList".to_string(),
                        value: "acp_profile_config".to_string(),
                    },
                    ProviderBindingMetadata {
                        key: "modelCapabilities".to_string(),
                        value: capability_diagnostic.to_string(),
                    },
                ],
            });
        }

        // No configured models: ask the ACP CLI itself through a short-lived
        // probe process (initialize + session/new).
        match self.client.probe_runtime_session_config(profile_id).await {
            Ok(probed) => {
                let models = normalize_model_list(probed.models);
                let source = if models.is_empty() {
                    AgentModelListSource::Unavailable
                } else {
                    AgentModelListSource::Probed
                };
                Ok(AgentModelListResponse {
                    agent_id: None,
                    provider_kind: ProviderKind::Acp,
                    provider_profile_id: Some(profile_id.clone()),
                    models,
                    reasoning_efforts: probed.reasoning_efforts,
                    model_capabilities: Vec::new(),
                    source,
                    diagnostics: vec![ProviderBindingMetadata {
                        key: "modelList".to_string(),
                        value: "acp_runtime_probe".to_string(),
                    }],
                })
            }
            Err(err) => Ok(AgentModelListResponse {
                agent_id: None,
                provider_kind: ProviderKind::Acp,
                provider_profile_id: Some(profile_id.clone()),
                models: Vec::new(),
                reasoning_efforts: Vec::new(),
                model_capabilities: Vec::new(),
                source: AgentModelListSource::Unavailable,
                diagnostics: vec![
                    ProviderBindingMetadata {
                        key: "modelList".to_string(),
                        value: "acp_runtime_probe_failed".to_string(),
                    },
                    ProviderBindingMetadata {
                        key: "probeError".to_string(),
                        value: err.code.clone(),
                    },
                ],
            }),
        }
    }

    async fn probe_session_config(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<AgentSessionConfigProbe> {
        let probed = self
            .client
            .probe_runtime_session_config(provider_profile_id)
            .await?;
        Ok(AgentSessionConfigProbe {
            models: probed.models,
            modes: probed.modes,
            reasoning_efforts: probed.reasoning_efforts,
            options: probed.options,
        })
    }

    async fn probe_session_config_for_model(
        &self,
        provider_profile_id: &ProviderProfileId,
        model_id: &str,
    ) -> VibexResult<AgentSessionConfigProbe> {
        let probed = self
            .client
            .probe_runtime_session_config_for_model(provider_profile_id, model_id)
            .await?;
        Ok(AgentSessionConfigProbe {
            models: probed.models,
            modes: probed.modes,
            reasoning_efforts: probed.reasoning_efforts,
            options: probed.options,
        })
    }

    async fn probe_agent_session_config(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> VibexResult<AgentSessionConfigProbe> {
        let probed = self
            .client
            .probe_runtime_session_config_for_agent(agent_id)
            .await?;
        Ok(AgentSessionConfigProbe {
            // Models belong to Provider Profiles. Agent setup persists only
            // CLI-owned runtime controls even if session/new reports a model.
            models: Vec::new(),
            modes: probed.modes,
            reasoning_efforts: probed.reasoning_efforts,
            options: probed.options,
        })
    }

    async fn create_session(
        &self,
        request: ProviderCreateRequest,
    ) -> VibexResult<ProviderSessionHandle> {
        let profile_revision = match self.config_service.as_ref() {
            Some(service) => service
                .get_profile(&request.provider_profile_id)?
                .map(|profile| profile.updated_at_ms)
                .ok_or_else(|| {
                    VibexError::validation(
                        "provider_profile_not_found",
                        "Provider Profile was not found while creating the ACP binding",
                    )
                })?,
            None => 0,
        };
        let acp_session = self
            .client
            .create_session(AcpCreateSessionRequest {
                session_id: request.session_id.clone(),
                provider_profile_id: request.provider_profile_id.clone(),
                model: request.model.clone(),
                workspace_root: request.workspace_root,
                runtime_resources: request.runtime_resources.clone(),
            })
            .await?;

        let capabilities = self.capabilities_for_profile(Some(&request.provider_profile_id));
        Ok(ProviderSessionHandle {
            binding: provider_binding(
                request.session_id,
                vibex_core::RuntimeAuthSource::provider_profile(request.provider_profile_id),
                profile_revision,
                acp_session,
                request.model,
                None,
            ),
            capabilities,
        })
    }

    async fn resume_session(&self, binding: ProviderBinding) -> VibexResult<ProviderSessionHandle> {
        let capabilities = self.capabilities_for_profile(binding.auth_source.provider_profile_id());
        let acp_session = self.client.resume_session(binding.clone()).await?;
        Ok(ProviderSessionHandle {
            binding: provider_binding(
                binding.session_id,
                binding.auth_source,
                binding.auth_source_revision,
                acp_session,
                None,
                Some(binding.created_at_ms),
            ),
            capabilities,
        })
    }

    async fn prepare_turn_execution(
        &self,
        _handle: &ProviderSessionHandle,
        request: &ProviderTurnRequest,
    ) -> VibexResult<Option<ProviderTurnExecutionIdentity>> {
        let attachments =
            materialize_provider_attachments(&request.session_id, &request.attachments)?;
        self.client
            .prepare_turn_execution(&AcpSendTurnRequest {
                session_id: request.session_id.clone(),
                message_submission_id: request.message_submission_id.clone(),
                required_runtime: request.required_runtime.clone(),
                text: request.text.clone(),
                attachments,
                workspace_root: request.workspace_root.clone(),
                binding: request.binding.clone(),
                runtime_resources: request.runtime_resources.clone(),
                execution_identity: request.execution_identity.clone(),
                event_sender: None,
                usage_execution_context: request.usage_execution_context.clone(),
                usage_counter_origin: request.usage_counter_origin,
                usage_event_sender: request.usage_event_sender.clone(),
            })
            .await
    }

    async fn import_session(
        &self,
        request: ProviderCreateRequest,
        candidate: ExternalSessionImportCandidate,
    ) -> VibexResult<ProviderSessionHandle> {
        let profile_revision = match self.config_service.as_ref() {
            Some(service) => service
                .get_profile(&request.provider_profile_id)?
                .map(|profile| profile.updated_at_ms)
                .ok_or_else(|| {
                    VibexError::validation(
                        "provider_profile_not_found",
                        "Provider Profile was not found while importing the ACP binding",
                    )
                })?,
            None => 0,
        };
        let capabilities = self.capabilities_for_profile(Some(&request.provider_profile_id));
        let acp_session = self
            .client
            .import_session(AcpImportSessionRequest {
                session_id: request.session_id.clone(),
                provider_profile_id: request.provider_profile_id.clone(),
                native_session_id: candidate.native_session_id.clone(),
                workspace_root: request.workspace_root,
                runtime_resources: request.runtime_resources,
            })
            .await?;
        Ok(ProviderSessionHandle {
            binding: provider_binding(
                request.session_id,
                vibex_core::RuntimeAuthSource::provider_profile(request.provider_profile_id),
                profile_revision,
                acp_session,
                None,
                None,
            ),
            capabilities,
        })
    }

    async fn list_import_candidates(
        &self,
        provider_profile_id: &ProviderProfileId,
        workspace_root: Option<&str>,
    ) -> VibexResult<Vec<ExternalSessionImportCandidate>> {
        self.client
            .list_sessions(provider_profile_id, workspace_root)
            .await
    }

    async fn send_turn(
        &self,
        handle: ProviderSessionHandle,
        request: ProviderTurnRequest,
    ) -> VibexResult<ProviderTurnResult> {
        let attachments =
            materialize_provider_attachments(&request.session_id, &request.attachments)?;

        // Bridge streamed AcpEvents into the provider-neutral event channel so
        // the AgentManager persists timeline items while the turn is running.
        let (acp_event_sender, forwarder) = match request.event_sender.clone() {
            Some(provider_event_sender) => {
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<AcpEvent>();
                let stream_session_id = request.binding.session_id.clone();
                let forwarder = tokio::spawn(async move {
                    while let Some(event) = receiver.recv().await {
                        let provider_event = map_acp_event(stream_session_id.clone(), event);
                        if provider_event_sender.send(provider_event).is_err() {
                            break;
                        }
                    }
                });
                (Some(sender), Some(forwarder))
            }
            None => (None, None),
        };

        let turn = self
            .client
            .send_turn(AcpSendTurnRequest {
                session_id: request.session_id.clone(),
                message_submission_id: request.message_submission_id.clone(),
                required_runtime: request.required_runtime.clone(),
                text: request.text,
                attachments,
                workspace_root: request.workspace_root,
                binding: request.binding.clone(),
                runtime_resources: request.runtime_resources,
                execution_identity: request.execution_identity,
                event_sender: acp_event_sender,
                usage_execution_context: request.usage_execution_context,
                usage_counter_origin: request.usage_counter_origin,
                usage_event_sender: request.usage_event_sender,
            })
            .await;
        if let Some(forwarder) = forwarder {
            let _ = forwarder.await;
        }
        let turn = turn?;
        let binding_update = turn.binding_update.map(|session| {
            provider_binding(
                request.session_id,
                request.binding.auth_source.clone(),
                request.binding.auth_source_revision,
                session,
                None,
                Some(handle.binding.created_at_ms),
            )
        });
        let events = turn
            .events
            .into_iter()
            .map(|event| map_acp_event(request.binding.session_id.clone(), event))
            .collect();

        Ok(ProviderTurnResult {
            events,
            binding_update,
            completed: turn.completed,
        })
    }

    async fn discover_commands(
        &self,
        request: AgentCommandDiscoverRequest,
    ) -> VibexResult<AgentCommandDiscoverResponse> {
        // Prefer commands announced by the live ACP agent through
        // available_commands_update; they are authoritative for the session.
        if let Some(session_id) = request.session_id.as_ref()
            && let Some(commands) = self.client.list_session_commands(session_id).await?
        {
            return Ok(AgentCommandDiscoverResponse {
                entries: commands
                    .iter()
                    .map(|command| acp_runtime_command_entry(command, ProviderKind::Acp))
                    .collect(),
                diagnostics: vec![ProviderBindingMetadata {
                    key: "catalogSource".to_string(),
                    value: "acp-session-runtime".to_string(),
                }],
            });
        }

        if self.request_supports_codex_commands(&request) {
            return Ok(AgentCommandDiscoverResponse {
                entries: CODEX_ACP_PROVIDER_COMMANDS
                    .iter()
                    .map(|command| codex_acp_command_entry(command, ProviderKind::Acp))
                    .collect(),
                diagnostics: vec![ProviderBindingMetadata {
                    key: "catalogSource".to_string(),
                    value: "codex-acp-pinned-adapter".to_string(),
                }],
            });
        }

        if !self.request_supports_opencode_commands(&request) {
            return Ok(AgentCommandDiscoverResponse {
                entries: Vec::new(),
                diagnostics: vec![ProviderBindingMetadata {
                    key: "catalogSource".to_string(),
                    value: "unsupported-acp-profile".to_string(),
                }],
            });
        }

        Ok(AgentCommandDiscoverResponse {
            entries: OPENCODE_ACP_PROVIDER_COMMANDS
                .iter()
                .map(|command| opencode_acp_command_entry(command, ProviderKind::Acp))
                .collect(),
            diagnostics: Vec::new(),
        })
    }

    async fn execute_command(
        &self,
        handle: ProviderSessionHandle,
        request: AgentCommandExecuteRequest,
        turn: ProviderTurnRequest,
    ) -> VibexResult<ProviderTurnResult> {
        if request.trigger != AgentCommandTrigger::Slash {
            return Err(VibexError::validation(
                "acp_command_trigger_invalid",
                "ACP provider commands must use slash trigger syntax",
            ));
        }
        self.send_turn(handle, turn).await
    }

    async fn resolve_permission(&self, request: ProviderPermissionResolution) -> VibexResult<()> {
        self.client
            .resolve_permission(AcpPermissionResolution {
                binding: request.binding,
                resolution: request.resolution,
            })
            .await
    }

    async fn resolve_elicitation(&self, request: ProviderElicitationResolution) -> VibexResult<()> {
        self.client
            .resolve_elicitation(AcpElicitationResolution {
                binding: request.binding,
                execution_identity: request.execution_identity,
                resolution: request.resolution,
            })
            .await
    }

    async fn interrupt(&self, handle: ProviderSessionHandle) -> VibexResult<()> {
        self.client.interrupt(&handle.binding).await
    }

    async fn close_session(&self, binding: ProviderBinding) -> VibexResult<()> {
        self.client.close_session(&binding).await
    }
}

impl AcpAgentProvider {
    fn acp_config_for_profile(
        &self,
        provider_profile_id: &vibex_core::ProviderProfileId,
    ) -> Option<AcpProviderConfig> {
        self.config_service
            .as_ref()?
            .get_acp_profile_config(provider_profile_id.clone())
            .ok()
    }

    fn request_supports_opencode_commands(&self, request: &AgentCommandDiscoverRequest) -> bool {
        if let Some(profile_id) = request.provider_profile_id.as_ref() {
            if let Some(config) = self.acp_config_for_profile(profile_id) {
                return acp_config_supports_opencode_commands(&config);
            }

            return self.config_service.is_none();
        }

        self.config_service.is_none()
    }

    fn request_supports_codex_commands(&self, request: &AgentCommandDiscoverRequest) -> bool {
        if request.agent_id.as_ref().map(vibex_core::AgentId::as_str)
            != Some(registry::CODEX_AGENT_ID)
        {
            return false;
        }

        let Some(profile_id) = request.provider_profile_id.as_ref() else {
            return self.config_service.is_none();
        };
        self.acp_config_for_profile(profile_id)
            .is_some_and(|config| acp_config_supports_codex_commands(&config))
    }
}

fn acp_config_supports_codex_commands(config: &AcpProviderConfig) -> bool {
    acp_config_has_feature(config, &["slash_commands"]) && acp_runtime_looks_like_codex(config)
}

fn acp_config_supports_opencode_commands(config: &AcpProviderConfig) -> bool {
    acp_config_has_feature(config, &["slash_commands"])
        && (acp_runtime_looks_like_opencode(config)
            || acp_config_has_feature(
                config,
                &[
                    "opencode",
                    "opencode_commands",
                    "opencode_slash_commands",
                    "opencode_catalog",
                ],
            ))
}

fn normalize_model_list(models: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for model in models {
        let model = model.trim();
        if !model.is_empty() && !normalized.iter().any(|existing| existing == model) {
            normalized.push(model.to_string());
        }
    }
    normalized
}

fn acp_runtime_looks_like_opencode(config: &AcpProviderConfig) -> bool {
    let command_name = Path::new(&config.command)
        .file_stem()
        .or_else(|| Path::new(&config.command).file_name())
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    command_name.contains("opencode")
        || config
            .args
            .iter()
            .any(|arg| normalize_acp_feature_token(arg).contains("opencode"))
}

fn acp_runtime_looks_like_codex(config: &AcpProviderConfig) -> bool {
    let adapter_token = normalize_acp_feature_token(registry::CODEX_ADAPTER_ID);
    std::iter::once(config.command.as_str())
        .chain(config.args.iter().map(String::as_str))
        .any(|value| normalize_acp_feature_token(value).contains(&adapter_token))
}

fn acp_config_has_feature(config: &AcpProviderConfig, aliases: &[&str]) -> bool {
    config.features.iter().any(|feature| {
        let normalized = normalize_acp_feature_token(feature);
        aliases
            .iter()
            .any(|alias| normalized == normalize_acp_feature_token(alias))
    })
}

fn normalize_acp_feature_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-', '.'], "_")
}

fn acp_runtime_command_entry(
    command: &AcpRuntimeCommand,
    provider_kind: ProviderKind,
) -> AgentCommandEntry {
    AgentCommandEntry {
        id: format!("provider:acp:runtime:{}", command.name),
        trigger: AgentCommandTrigger::Slash,
        source_kind: AgentCommandSourceKind::Provider,
        label: format!("/{}", command.name),
        description: command.description.clone(),
        insertion_text: format!("/{} ", command.name),
        command_name: Some(command.name.clone()),
        provider_kind: Some(provider_kind),
        prompt_id: None,
        skill_id: None,
        reference_path: None,
        selection_behavior: AgentCommandSelectionBehavior::Insert,
        execution_behavior: AgentCommandExecutionBehavior::ProviderCommand,
        destructive: false,
        metadata: vec![ProviderBindingMetadata {
            key: "catalogSource".to_string(),
            value: "acp-session-runtime".to_string(),
        }],
    }
}

fn codex_acp_command_entry(
    command: &CodexAcpSlashCommand,
    provider_kind: ProviderKind,
) -> AgentCommandEntry {
    AgentCommandEntry {
        id: format!("provider:acp:codex:{}", command.name),
        trigger: AgentCommandTrigger::Slash,
        source_kind: AgentCommandSourceKind::Provider,
        label: format!("/{}", command.name),
        description: Some(command.description.to_string()),
        insertion_text: format!("/{} ", command.name),
        command_name: Some(command.name.to_string()),
        provider_kind: Some(provider_kind),
        prompt_id: None,
        skill_id: None,
        reference_path: None,
        selection_behavior: AgentCommandSelectionBehavior::Insert,
        execution_behavior: AgentCommandExecutionBehavior::ProviderCommand,
        destructive: false,
        metadata: vec![
            ProviderBindingMetadata {
                key: "catalogSource".to_string(),
                value: "codex-acp-pinned-adapter".to_string(),
            },
            ProviderBindingMetadata {
                key: "acpAdapter".to_string(),
                value: registry::CODEX_ADAPTER_ID.to_string(),
            },
            ProviderBindingMetadata {
                key: "acpAdapterVersion".to_string(),
                value: registry::CODEX_ADAPTER_VERSION.to_string(),
            },
        ],
    }
}

fn opencode_acp_command_entry(
    command: &OpenCodeAcpSlashCommand,
    provider_kind: ProviderKind,
) -> AgentCommandEntry {
    AgentCommandEntry {
        id: format!("provider:acp:opencode:{}", command.name),
        trigger: AgentCommandTrigger::Slash,
        source_kind: AgentCommandSourceKind::Provider,
        label: command.label.to_string(),
        description: Some(command.description.to_string()),
        insertion_text: command.insertion_text.to_string(),
        command_name: Some(command.name.to_string()),
        provider_kind: Some(provider_kind),
        prompt_id: None,
        skill_id: None,
        reference_path: None,
        selection_behavior: AgentCommandSelectionBehavior::Insert,
        execution_behavior: AgentCommandExecutionBehavior::ProviderCommand,
        destructive: false,
        metadata: vec![
            ProviderBindingMetadata {
                key: "catalogSource".to_string(),
                value: "opencode-acp-static-tui".to_string(),
            },
            ProviderBindingMetadata {
                key: "acpPreset".to_string(),
                value: OPENCODE_PRESET_ID.to_string(),
            },
        ],
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeAcpSmokeResult {
    pub status: OpenCodeAcpSmokeStatus,
    pub binary_path: PathBuf,
    pub provider_version: String,
    pub workspace_path: PathBuf,
    pub prompt: String,
    pub profile_id: String,
    pub redacted_config: RedactedAcpProviderConfig,
    pub capability_summary: ProviderCapabilitySummary,
    pub process_attempt: OpenCodeAcpProcessAttempt,
    pub adapter_boundary: AcpAdapterBoundaryAttempt,
    pub turn_boundary: AcpAdapterBoundaryAttempt,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeAcpSmokeStatus {
    Completed,
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactedAcpProviderConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<RedactedAcpEnvReference>,
    pub cwd_template: Option<String>,
    pub models: Vec<String>,
    pub modes: Vec<String>,
    pub features: Vec<String>,
    pub disabled_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactedAcpEnvReference {
    pub key: String,
    pub source: AcpProviderEnvSource,
    pub value_stored: bool,
    pub secret_lookup_key_stored: bool,
    pub redacted_hint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeAcpProcessAttempt {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_summary: Option<String>,
    pub stderr_summary: Option<String>,
    pub raw_output_stored: bool,
    pub session_config_snapshot: OpenCodeAcpSessionConfigSnapshot,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAdapterBoundaryAttempt {
    pub operation: String,
    pub status: AcpAdapterBoundaryStatus,
    pub provider_capabilities: ProviderCapabilities,
    pub structured_error: Option<VibexError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpAdapterBoundaryStatus {
    Completed,
    Blocked,
}

#[derive(Debug, Error)]
pub enum OpenCodeAcpSmokeError {
    #[error("OpenCode ACP smoke workspace failed: {0}")]
    Workspace(String),
    #[error("OpenCode ACP smoke config failed: {0}")]
    Config(String),
    #[error("OpenCode binary is missing at {0}")]
    BinaryMissing(String),
    #[error("OpenCode version probe failed: {0}")]
    VersionProbe(String),
    #[error("OpenCode ACP process attempt failed: {0}")]
    ProcessAttempt(String),
}

pub type OpenCodeAcpSmokeRunResult<T> = std::result::Result<T, OpenCodeAcpSmokeError>;

pub async fn run_opencode_acp_smoke(
    prompt: Option<String>,
) -> OpenCodeAcpSmokeRunResult<OpenCodeAcpSmokeResult> {
    let workspace_path = resolve_agent_smoke_workspace("opencode-acp", "direct")
        .map_err(|err| OpenCodeAcpSmokeError::Workspace(err.to_string()))?;
    let prompt = prompt.unwrap_or_else(|| DEFAULT_OPENCODE_SMOKE_PROMPT.to_string());
    let service = ProviderConfigService::new(temp_smoke_db_path());
    let profile = service
        .create_acp_profile(AcpProviderProfileCreateRequest {
            agent_id: None,
            display_name: "OpenCode ACP Smoke".to_string(),
            account_alias: None,
            preset_id: Some(OPENCODE_PRESET_ID.to_string()),
            config: None,
        })
        .map_err(|err| OpenCodeAcpSmokeError::Config(err.to_string()))?;
    let config = service
        .get_acp_profile_config(profile.id.clone())
        .map_err(|err| OpenCodeAcpSmokeError::Config(err.to_string()))?;
    let capability_result = service
        .run_capability_probes(ProviderRunCapabilityProbesRequest {
            provider_profile_ids: Some(vec![profile.id.clone()]),
            force_refresh: true,
        })
        .map_err(|err| OpenCodeAcpSmokeError::Config(err.to_string()))?;
    let capability_summary = capability_result
        .summaries
        .into_iter()
        .find(|summary| summary.profile.id == profile.id)
        .ok_or_else(|| OpenCodeAcpSmokeError::Config("capability summary missing".to_string()))?;

    let binary_path = PathBuf::from(&config.command);
    if !binary_path.is_file() {
        return Err(OpenCodeAcpSmokeError::BinaryMissing(
            binary_path.display().to_string(),
        ));
    }
    let provider_version = probe_opencode_version(&binary_path)?;
    let runtime_config = OpenCodeAcpRuntimeConfig::from_acp_config(&config)
        .map_err(|err| OpenCodeAcpSmokeError::Config(err.to_string()))?;
    let runtime_client = OpenCodeAcpClient::new(runtime_config);
    let process_attempt = runtime_client
        .run_startup_probe(&workspace_path)
        .map_err(|err| OpenCodeAcpSmokeError::ProcessAttempt(err.to_string()))?;
    let client: Arc<dyn AcpClient> = Arc::new(runtime_client);
    let (adapter_boundary, session_handle) =
        attempt_acp_adapter_boundary(&profile.id, &workspace_path, client.clone()).await;
    let turn_boundary = attempt_acp_turn_boundary(
        &profile.id,
        &workspace_path,
        client,
        &prompt,
        session_handle,
    )
    .await;
    let status = if adapter_boundary.status == AcpAdapterBoundaryStatus::Completed
        && turn_boundary.status == AcpAdapterBoundaryStatus::Completed
    {
        OpenCodeAcpSmokeStatus::Completed
    } else {
        OpenCodeAcpSmokeStatus::Blocked
    };
    let limitations = match status {
        OpenCodeAcpSmokeStatus::Completed => Vec::new(),
        OpenCodeAcpSmokeStatus::Blocked => vec![
            "OpenCode ACP runtime is exercised only by this explicit smoke command".to_string(),
            "Local OpenCode ACP exited or timed out before Vibex could establish a session handshake"
                .to_string(),
        ],
    };

    Ok(OpenCodeAcpSmokeResult {
        status,
        binary_path,
        provider_version,
        workspace_path,
        prompt,
        profile_id: profile.id.as_str().to_string(),
        redacted_config: RedactedAcpProviderConfig::from_config(&config),
        capability_summary,
        process_attempt,
        adapter_boundary,
        turn_boundary,
        limitations,
    })
}

impl RedactedAcpProviderConfig {
    fn from_config(config: &AcpProviderConfig) -> Self {
        Self {
            command: config.command.clone(),
            args: config.args.clone(),
            env: config
                .env
                .iter()
                .map(|entry| RedactedAcpEnvReference {
                    key: entry.key.clone(),
                    source: entry.source,
                    value_stored: false,
                    secret_lookup_key_stored: false,
                    redacted_hint: entry.redacted_hint.clone(),
                })
                .collect(),
            cwd_template: config.cwd_template.clone(),
            models: config.models.clone(),
            modes: config.modes.clone(),
            features: config.features.clone(),
            disabled_tools: config.disabled_tools.clone(),
        }
    }
}

fn temp_smoke_db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "vibex-acp-opencode-smoke-{}.db",
        RequestId::new().as_str()
    ))
}

fn probe_opencode_version(binary_path: &Path) -> OpenCodeAcpSmokeRunResult<String> {
    let mut command = std::process::Command::new(binary_path);
    command.arg("--version").stdin(Stdio::null());
    process_environment::sanitize_inherited_appimage_environment(&mut command);
    let output = command
        .output()
        .map_err(|err| OpenCodeAcpSmokeError::VersionProbe(err.to_string()))?;
    if !output.status.success() {
        return Err(OpenCodeAcpSmokeError::VersionProbe(format!(
            "exit code {:?}",
            output.status.code()
        )));
    }
    Ok(redacted_output_summary(&output.stdout).unwrap_or_else(|| "unknown".to_string()))
}

async fn attempt_acp_adapter_boundary(
    provider_profile_id: &vibex_core::ProviderProfileId,
    workspace_path: &Path,
    client: Arc<dyn AcpClient>,
) -> (AcpAdapterBoundaryAttempt, Option<ProviderSessionHandle>) {
    let provider = AcpAgentProvider::new(client);
    let provider_capabilities = provider.capabilities();
    let result = provider
        .create_session(ProviderCreateRequest {
            session_id: VibexSessionId::new(),
            provider_profile_id: provider_profile_id.clone(),
            model: None,
            workspace_root: workspace_path.display().to_string(),
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            runtime_resources: ProviderRuntimeResources::default(),
        })
        .await;

    match result {
        Ok(handle) => (
            AcpAdapterBoundaryAttempt {
                operation: "AgentProvider::create_session".to_string(),
                status: AcpAdapterBoundaryStatus::Completed,
                provider_capabilities,
                structured_error: None,
            },
            Some(handle),
        ),
        Err(error) => (
            AcpAdapterBoundaryAttempt {
                operation: "AgentProvider::create_session".to_string(),
                status: AcpAdapterBoundaryStatus::Blocked,
                provider_capabilities,
                structured_error: Some(error),
            },
            None,
        ),
    }
}

async fn attempt_acp_turn_boundary(
    _provider_profile_id: &vibex_core::ProviderProfileId,
    workspace_path: &Path,
    client: Arc<dyn AcpClient>,
    prompt: &str,
    session_handle: Option<ProviderSessionHandle>,
) -> AcpAdapterBoundaryAttempt {
    let provider = AcpAgentProvider::new(client);
    let provider_capabilities = provider.capabilities();
    let Some(handle) = session_handle else {
        return AcpAdapterBoundaryAttempt {
            operation: "AgentProvider::send_turn".to_string(),
            status: AcpAdapterBoundaryStatus::Blocked,
            provider_capabilities,
            structured_error: Some(VibexError::provider(
                "provider/acp_opencode_session_missing",
                "OpenCode ACP send_turn was skipped because create_session did not return a binding",
            )),
        };
    };
    let session_id = handle.binding.session_id.clone();
    let binding = handle.binding.clone();
    let mut request = ProviderTurnRequest {
        session_id,
        message_submission_id: None,
        required_runtime: None,
        text: prompt.to_string(),
        attachments: Vec::new(),
        workspace_root: workspace_path.display().to_string(),
        binding,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        runtime_resources: Default::default(),
        execution_identity: None,
        event_sender: None,
        binding_update_sender: None,
        usage_execution_context: None,
        usage_counter_origin: AgentUsageCounterOrigin::Unknown,
        usage_event_sender: None,
    };
    let result = match provider.prepare_turn_execution(&handle, &request).await {
        Ok(execution_identity) => {
            request.execution_identity = execution_identity;
            provider.send_turn(handle, request).await
        }
        Err(error) => Err(error),
    };

    match result {
        Ok(_) => AcpAdapterBoundaryAttempt {
            operation: "AgentProvider::send_turn".to_string(),
            status: AcpAdapterBoundaryStatus::Completed,
            provider_capabilities,
            structured_error: None,
        },
        Err(error) => AcpAdapterBoundaryAttempt {
            operation: "AgentProvider::send_turn".to_string(),
            status: AcpAdapterBoundaryStatus::Blocked,
            provider_capabilities,
            structured_error: Some(error),
        },
    }
}

fn redacted_output_summary(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    let text = text.trim();
    if text.is_empty() {
        None
    } else if looks_sensitive(text) {
        Some("[redacted-sensitive-output]".to_string())
    } else if text.len() > 300 {
        let prefix: String = text.chars().take(300).collect();
        Some(format!("{prefix}...(truncated)"))
    } else {
        Some(text.to_string())
    }
}

pub(crate) fn redacted_args_summary(args: &[String]) -> String {
    redacted_args(args).join(" ")
}

pub(crate) fn redacted_args(args: &[String]) -> Vec<String> {
    let mut redact_next = false;
    args.iter()
        .map(|arg| {
            let redact_current = redact_next || looks_sensitive(arg);
            redact_next = sensitive_option_takes_value(arg);
            if redact_current {
                "[redacted]".to_string()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn sensitive_option_takes_value(argument: &str) -> bool {
    argument.starts_with('-') && !argument.contains('=') && looks_sensitive(argument)
}

pub(crate) fn redact_summary(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    if looks_sensitive(value) {
        return "[redacted-sensitive-output]".to_string();
    }
    bounded_summary(value)
}

/// Timeline labels may contain security vocabulary in paths, symbols, and search
/// queries. Only credential-shaped data uses the placeholder on this surface.
pub(crate) fn redact_timeline_summary(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    if contains_sensitive_data(value) {
        return REDACTED_SENSITIVE_OUTPUT.to_string();
    }
    bounded_summary(value)
}

fn bounded_summary(value: &str) -> String {
    if value.len() > 300 {
        let prefix: String = value.chars().take(300).collect();
        format!("{prefix}...(truncated)")
    } else {
        value.to_string()
    }
}

pub(crate) fn contains_sensitive_data(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if contains_secret_literal(value) {
        return true;
    }
    if let Ok(json) = serde_json::from_str::<Value>(value)
        && json_contains_sensitive_data(&json)
    {
        return true;
    }
    contains_sensitive_assignment(value)
}

fn json_contains_sensitive_data(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (sensitive_field_name(key) && json_value_can_contain_secret(value))
                || json_contains_sensitive_data(value)
        }),
        Value::Array(items) => items.iter().any(json_contains_sensitive_data),
        Value::String(value) => {
            contains_secret_literal(value) || contains_sensitive_assignment(value)
        }
        _ => false,
    }
}

fn json_value_can_contain_secret(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

fn contains_secret_literal(value: &str) -> bool {
    let tokens = value
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | '<' | '>' | '{' | '}' | '[' | ']' | '(' | ')' | ',' | ';'
                )
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    for (index, token) in tokens.iter().enumerate() {
        let token = token.trim_matches(|character: char| matches!(character, ':' | '='));
        let lower = token.to_ascii_lowercase();
        if [
            "sk-",
            "ghp_",
            "github_pat_",
            "glpat-",
            "xoxb-",
            "xoxp-",
            "xoxa-",
            "xoxr-",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix) && token.len() > prefix.len() + 4)
            || token.starts_with("AKIA") && token.len() >= 16
            || token.starts_with("AIza") && token.len() >= 20
            || looks_like_jwt(token)
        {
            return true;
        }
        if lower == "bearer"
            && tokens
                .get(index + 1)
                .is_some_and(|next| !next.trim_matches([':', '=']).is_empty())
        {
            return true;
        }
    }
    value.contains("-----BEGIN PRIVATE KEY-----")
        || value.contains("-----BEGIN RSA PRIVATE KEY-----")
        || value.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
}

fn looks_like_jwt(value: &str) -> bool {
    let mut segments = value.split('.');
    let Some(header) = segments.next() else {
        return false;
    };
    let Some(payload) = segments.next() else {
        return false;
    };
    let Some(signature) = segments.next() else {
        return false;
    };
    segments.next().is_none()
        && header.starts_with("eyJ")
        && payload.len() >= 8
        && signature.len() >= 8
        && [header, payload, signature].iter().all(|segment| {
            segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        })
}

fn contains_sensitive_assignment(value: &str) -> bool {
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    tokens.iter().enumerate().any(|(index, token)| {
        let token = token.trim_matches(|character: char| {
            matches!(
                character,
                '\'' | '"' | '`' | '{' | '}' | '[' | ']' | '(' | ')' | ',' | ';'
            )
        });
        for separator in ['=', ':'] {
            if let Some((key, assigned)) = token.split_once(separator)
                && sensitive_field_name(key)
                && (!assigned.trim_matches(['\'', '"']).is_empty()
                    || tokens.get(index + 1).is_some_and(|next| !next.is_empty()))
            {
                return true;
            }
        }

        let key = token.trim_end_matches(['=', ':']);
        if !sensitive_field_name(key) {
            return false;
        }
        let separated_assignment = tokens
            .get(index + 1)
            .is_some_and(|next| matches!(*next, "=" | ":"))
            && tokens.get(index + 2).is_some_and(|next| !next.is_empty());
        let option_or_env_value = (key.starts_with('-') || is_environment_style_key(key))
            && tokens.get(index + 1).is_some_and(|next| !next.is_empty());
        separated_assignment || option_or_env_value
    })
}

fn sensitive_field_name(value: &str) -> bool {
    let compact = value
        .trim_matches(|character: char| {
            matches!(
                character,
                '-' | '$' | '\'' | '"' | '`' | '{' | '}' | '[' | ']' | '(' | ')'
            )
        })
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    matches!(
        compact.as_str(),
        "auth"
            | "authorization"
            | "apikey"
            | "password"
            | "passwd"
            | "privatekey"
            | "secret"
            | "token"
    ) || [
        "accesstoken",
        "apikey",
        "authtoken",
        "clientsecret",
        "password",
        "privatekey",
        "refreshtoken",
        "secret",
        "secretkey",
        "token",
    ]
    .iter()
    .any(|suffix| compact.ends_with(suffix))
}

fn is_environment_style_key(value: &str) -> bool {
    let value = value
        .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '_');
    value.contains('_')
        && value.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn provider_binding(
    session_id: vibex_core::VibexSessionId,
    auth_source: vibex_core::RuntimeAuthSource,
    auth_source_revision: i64,
    mut acp_session: AcpSession,
    selected_model: Option<String>,
    created_at_ms: Option<i64>,
) -> ProviderBinding {
    let now = unix_timestamp_ms();
    if let Some(model) = selected_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        acp_session.redacted_metadata.push(ProviderBindingMetadata {
            key: vibex_agent::PROVIDER_SELECTED_MODEL_METADATA_KEY.to_string(),
            value: model.to_string(),
        });
    }
    ProviderBinding {
        session_id,
        provider_kind: ProviderKind::Acp,
        auth_source,
        auth_source_revision,
        native: ProviderNativeBinding {
            native_session_id: sanitize_optional_native_value(acp_session.native_session_id),
            native_thread_id: sanitize_optional_native_value(acp_session.native_thread_id),
            native_resume_token: sanitize_optional_native_value(acp_session.native_resume_token),
            session_config_state: acp_session.session_config_state,
            redacted_metadata: sanitize_metadata(acp_session.redacted_metadata),
        },
        created_at_ms: created_at_ms.unwrap_or(now),
        updated_at_ms: now,
    }
}

fn map_acp_event(session_id: vibex_core::VibexSessionId, event: AcpEvent) -> ProviderEvent {
    match event {
        AcpEvent::AssistantDelta {
            text_delta,
            chunk_index,
            phase,
        } => ProviderEvent::agent(TimelinePayload::AgentMessageDelta(
            AgentMessageDeltaPayload {
                text_delta,
                chunk_index,
                phase,
            },
        )),
        AcpEvent::AssistantMessage { text, is_final } => {
            ProviderEvent::agent(TimelinePayload::AgentMessage(AgentMessagePayload {
                text,
                is_final,
            }))
        }
        AcpEvent::Reasoning { text, is_final } => {
            ProviderEvent::agent(TimelinePayload::Reasoning(ReasoningPayload {
                text,
                is_final,
            }))
        }
        AcpEvent::Plan { title, steps } => {
            ProviderEvent::agent(TimelinePayload::Plan(PlanPayload { title, steps }))
        }
        AcpEvent::ToolCall {
            tool_call_id,
            tool_name,
            status,
            summary,
            input_summary,
            output_summary,
        } => ProviderEvent {
            source: vibex_core::TimelineSource::Provider,
            payload: TimelinePayload::ToolCall(ToolCallPayload {
                // Keep the legacy test seam on the same non-leaking identity
                // contract as live canonical events. Older fixture adapters
                // only provide summaries, so they cannot be re-enriched, but
                // their native tool id must still never cross the timeline
                // boundary.
                tool_call_id: stable_event_correlation_id(
                    "legacy-acp",
                    &tool_call_id,
                    "tool_call",
                    0,
                ),
                tool_name: AgentEventRawExtension::sanitize_text(tool_name),
                status,
                summary: AgentEventRawExtension::sanitize_text(summary),
                input_summary: input_summary.map(AgentEventRawExtension::sanitize_text),
                output_summary: output_summary.map(AgentEventRawExtension::sanitize_text),
                raw_extension: None,
            }),
            provider_correlation_id: Some(stable_event_correlation_id(
                "legacy-acp",
                &tool_call_id,
                "tool_call",
                0,
            )),
            redaction_state: TimelineRedactionState::None,
        },
        AcpEvent::Canonical(event) => event.into_provider_event(),
        AcpEvent::PermissionRequest {
            request_id,
            provider_request_id,
            risk_category,
            title,
            details,
            options,
        } => ProviderEvent {
            source: vibex_core::TimelineSource::Provider,
            payload: TimelinePayload::PermissionRequest(PermissionRequest {
                id: request_id.unwrap_or_else(vibex_core::RequestId::new),
                session_id,
                project_id: None,
                workspace_id: None,
                provider_request_id: provider_request_id.clone(),
                risk_category,
                title,
                details,
                allowed_responses: if options.is_empty() {
                    vec![
                        PermissionResponseKind::Approve,
                        PermissionResponseKind::Deny,
                    ]
                } else {
                    options.iter().map(|option| option.response).collect()
                },
                response_options: options,
                status: PermissionRequestStatus::Pending,
                requested_at_ms: unix_timestamp_ms(),
                expires_at_ms: None,
            }),
            provider_correlation_id: provider_request_id,
            redaction_state: TimelineRedactionState::None,
        },
        AcpEvent::ElicitationRequest(request) => ProviderEvent {
            source: vibex_core::TimelineSource::Provider,
            provider_correlation_id: request.provider_request_id.clone(),
            payload: TimelinePayload::ElicitationRequest(request),
            redaction_state: TimelineRedactionState::None,
        },
        AcpEvent::SystemNotice { level, message } => {
            ProviderEvent::provider(TimelinePayload::SystemNotice(SystemNoticePayload {
                level,
                message,
            }))
        }
        AcpEvent::Error {
            code,
            message,
            recoverable,
            provider_correlation_id,
        } => ProviderEvent {
            source: vibex_core::TimelineSource::Provider,
            payload: TimelinePayload::Error(TimelineErrorPayload {
                code,
                message,
                recoverable,
            }),
            provider_correlation_id,
            redaction_state: TimelineRedactionState::ContainsRedactions,
        },
        AcpEvent::Unknown { event_kind } => {
            let event_kind = if looks_sensitive(&event_kind) {
                "redacted".to_string()
            } else {
                event_kind
            };
            ProviderEvent::provider(TimelinePayload::SystemNotice(SystemNoticePayload {
                level: SystemNoticeLevel::Warning,
                message: format!("Ignored unsupported ACP event kind: {event_kind}"),
            }))
        }
    }
}

fn sanitize_metadata(metadata: Vec<ProviderBindingMetadata>) -> Vec<ProviderBindingMetadata> {
    metadata
        .into_iter()
        .filter_map(|entry| {
            if looks_sensitive(&entry.key) || looks_sensitive(&entry.value) {
                return None;
            }
            Some(entry)
        })
        .collect()
}

fn sanitize_optional_native_value(value: Option<String>) -> Option<String> {
    value.filter(|value| !looks_sensitive(value))
}

pub(crate) fn looks_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("auth")
        || lower.contains("bearer ")
        || lower.contains("password")
        || lower.contains("private_key")
        || lower.contains("secret")
        || lower.contains("token")
        || lower.starts_with("sk-")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::Mutex;
    use vibex_agent::AgentManager;
    use vibex_core::{
        AcpAdapterId, AgentRuntimeRouteKey, PermissionActionDetail, PermissionRiskCategory,
        ProviderBindingMetadata, ProviderKind, ProviderVersionInfo, TimelinePayload, TransportKind,
    };

    use super::*;

    #[derive(Debug)]
    struct FixtureAcpClient {
        session: AcpSession,
        turn: Mutex<Option<VibexResult<AcpTurn>>>,
        commands: Option<Vec<AcpRuntimeCommand>>,
    }

    impl FixtureAcpClient {
        fn new(session: AcpSession, turn: VibexResult<AcpTurn>) -> Self {
            Self {
                session,
                turn: Mutex::new(Some(turn)),
                commands: None,
            }
        }

        fn with_commands(mut self, commands: Vec<AcpRuntimeCommand>) -> Self {
            self.commands = Some(commands);
            self
        }
    }

    #[async_trait]
    impl AcpClient for FixtureAcpClient {
        async fn create_session(
            &self,
            _request: AcpCreateSessionRequest,
        ) -> VibexResult<AcpSession> {
            Ok(self.session.clone())
        }

        async fn resume_session(&self, _binding: ProviderBinding) -> VibexResult<AcpSession> {
            Ok(self.session.clone())
        }

        async fn send_turn(&self, _request: AcpSendTurnRequest) -> VibexResult<AcpTurn> {
            self.turn
                .lock()
                .await
                .take()
                .expect("fixture turn should be consumed once")
        }

        async fn list_session_commands(
            &self,
            _session_id: &VibexSessionId,
        ) -> VibexResult<Option<Vec<AcpRuntimeCommand>>> {
            Ok(self.commands.clone())
        }
    }

    #[test]
    fn acp_capabilities_keep_core_operations_conservative() {
        let capabilities = AcpAgentProvider::capabilities_static();

        assert_eq!(capabilities.kind, ProviderKind::Acp);
        assert!(!capabilities.model_list);
        assert!(!capabilities.permission_requests);
        assert!(!capabilities.interrupt);
        assert!(capabilities.slash_commands);
        assert!(capabilities.skills);
        assert_eq!(
            capabilities.version,
            ProviderVersionInfo {
                provider_version: None,
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                capability_source: "acp-foundation-static".to_string(),
            }
        );
    }

    #[test]
    fn acp_send_turn_request_debug_omits_prompt_and_runtime_secrets() {
        let session_id = VibexSessionId::new();
        let submission_id = MessageSubmissionId::new();
        let mut binding = test_binding(session_id.clone());
        binding.native.native_session_id = Some("native-secret-id".to_string());
        binding.native.native_resume_token = Some("resume-secret-token".to_string());
        let request = AcpSendTurnRequest {
            session_id,
            message_submission_id: Some(submission_id.clone()),
            required_runtime: Some(SessionRuntimeSelection {
                agent_id: vibex_core::AgentId::parse("opencode").unwrap(),
                auth_source: binding.auth_source.clone(),
                model: vibex_core::RuntimeModelSelection::explicit("model-secret-id"),
                reasoning_effort: Some("high".to_string()),
                mode_id: Some("build".to_string()),
                config_values: Default::default(),
            }),
            text: "prompt-secret-SHOULD-NOT-DEBUG".to_string(),
            attachments: vec![ProviderTurnAttachment {
                label: "secret attachment".to_string(),
                mime_type: Some("text/plain".to_string()),
                uri: Some("file:///private/secret.txt".to_string()),
                local_path: Some(PathBuf::from("/private/secret.txt")),
            }],
            workspace_root: "/private/workspace".to_string(),
            binding,
            runtime_resources: ProviderRuntimeResources::default(),
            execution_identity: Some(ProviderTurnExecutionIdentity {
                binding_id: vibex_core::RuntimeBindingId::new(),
                activation_generation: 7,
                model_id: Some("model-secret-id".to_string()),
            }),
            event_sender: None,
            usage_execution_context: None,
            usage_counter_origin: AgentUsageCounterOrigin::Unknown,
            usage_event_sender: None,
        };

        let debug = format!("{request:?}");
        assert!(!debug.contains("prompt-secret-SHOULD-NOT-DEBUG"));
        assert!(!debug.contains("secret attachment"));
        assert!(!debug.contains("/private/secret.txt"));
        assert!(!debug.contains("/private/workspace"));
        assert!(!debug.contains("native-secret-id"));
        assert!(!debug.contains("resume-secret-token"));
        assert!(!debug.contains("model-secret-id"));
        assert!(!debug.contains(submission_id.as_str()));
        assert!(debug.contains("has_message_submission_id: true"));
        assert!(debug.contains("has_required_runtime: true"));
        assert!(debug.contains("attachment_count: 1"));
    }

    #[tokio::test]
    async fn discovers_opencode_acp_static_provider_commands() {
        let provider = AcpAgentProvider::new(Arc::new(FixtureAcpClient::new(
            AcpSession::default(),
            Ok(AcpTurn {
                events: Vec::new(),
                binding_update: None,
                completed: true,
            }),
        )));
        let response = provider
            .discover_commands(AgentCommandDiscoverRequest {
                agent_id: Some(vibex_core::AgentId::parse("opencode").unwrap()),
                provider_profile_id: None,
                session_id: None,
                workspace_id: None,
                trigger: Some(AgentCommandTrigger::Slash),
                query: None,
                limit: None,
            })
            .await
            .unwrap();

        assert!(provider.capabilities().slash_commands);
        assert!(response.entries.iter().any(|entry| {
            entry.source_kind == AgentCommandSourceKind::Provider
                && entry.label == "/status"
                && entry.command_name.as_deref() == Some("status")
        }));
        assert!(
            response
                .entries
                .iter()
                .any(|entry| entry.label == "/share" && entry.insertion_text == "/share ")
        );
    }

    #[tokio::test]
    async fn discovers_opencode_commands_for_opencode_profile() {
        let db_path = temp_db_path("commands-opencode-profile");
        let service = ProviderConfigService::new(db_path.clone());
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: None,
                display_name: "OpenCode ACP".to_string(),
                account_alias: Some("local opencode".to_string()),
                preset_id: Some(OPENCODE_PRESET_ID.to_string()),
                config: None,
            })
            .unwrap();
        let provider = AcpAgentProvider::with_config_service(
            Arc::new(FixtureAcpClient::new(
                AcpSession::default(),
                Ok(AcpTurn {
                    events: Vec::new(),
                    binding_update: None,
                    completed: true,
                }),
            )),
            service,
        );

        let response = provider
            .discover_commands(AgentCommandDiscoverRequest {
                agent_id: Some(profile.agent_id.clone()),
                provider_profile_id: Some(profile.id.clone()),
                session_id: None,
                workspace_id: None,
                trigger: Some(AgentCommandTrigger::Slash),
                query: None,
                limit: None,
            })
            .await
            .unwrap();

        assert!(provider.capabilities_for_profile(Some(&profile.id)).skills);
        assert!(response.entries.iter().any(|entry| {
            entry.source_kind == AgentCommandSourceKind::Provider
                && entry.label == "/status"
                && entry.command_name.as_deref() == Some("status")
        }));

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn discovers_codex_commands_before_a_session_exists() {
        let db_path = temp_db_path("commands-codex-profile");
        let service = ProviderConfigService::new(db_path.clone());
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse(registry::CODEX_AGENT_ID).unwrap()),
                display_name: "Codex ACP".to_string(),
                account_alias: None,
                preset_id: None,
                config: Some(AcpProviderConfig {
                    command: "node".to_string(),
                    args: vec![format!(
                        "/managed/acp-adapters/{}/{}/node_modules/@agentclientprotocol/codex-acp/dist/index.js",
                        registry::CODEX_ADAPTER_ID,
                        registry::CODEX_ADAPTER_VERSION
                    )],
                    env: Vec::new(),
                    cwd_template: Some("{workspaceRoot}".to_string()),
                    process_strategy: vibex_core::AcpProcessStrategy::default(),
                    terminal_tools: false,
                    terminal_auth: false,
                    models: Vec::new(),
                    modes: Vec::new(),
                    features: vec!["slash_commands".to_string()],
                    disabled_tools: Vec::new(),
                }),
            })
            .unwrap();
        let provider = Arc::new(AcpAgentProvider::with_config_service(
            Arc::new(FixtureAcpClient::new(
                AcpSession::default(),
                Ok(AcpTurn {
                    events: Vec::new(),
                    binding_update: None,
                    completed: true,
                }),
            )),
            service,
        ));
        let mut manager = AgentManager::new(&db_path).unwrap();
        manager
            .register_runtime(
                AgentRuntimeRouteKey {
                    agent_id: profile.agent_id.clone(),
                    transport_kind: TransportKind::Acp,
                    adapter_id: AcpAdapterId::parse(registry::CODEX_ADAPTER_ID).unwrap(),
                },
                provider,
            )
            .unwrap();

        let response = manager
            .discover_commands(AgentCommandDiscoverRequest {
                agent_id: Some(profile.agent_id.clone()),
                provider_profile_id: Some(profile.id.clone()),
                session_id: None,
                workspace_id: None,
                trigger: Some(AgentCommandTrigger::Slash),
                query: Some(String::new()),
                limit: Some(10),
            })
            .await
            .unwrap();

        assert_eq!(
            response
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/compact",
                "/goal",
                "/logout",
                "/mcp",
                "/plan",
                "/review",
                "/review-branch",
                "/review-commit",
                "/skills",
                "/status",
            ]
        );
        assert!(response.entries.iter().any(|entry| {
            entry.label == "/status"
                && entry.insertion_text == "/status "
                && entry.command_name.as_deref() == Some("status")
        }));
        assert!(response.entries.iter().any(|entry| {
            entry.label == "/review-commit"
                && entry.execution_behavior == AgentCommandExecutionBehavior::ProviderCommand
        }));
        assert!(response.diagnostics.iter().any(|diagnostic| {
            diagnostic.key == "catalogSource" && diagnostic.value == "codex-acp-pinned-adapter"
        }));

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn live_acp_commands_override_the_codex_pre_session_catalog() {
        let provider = AcpAgentProvider::new(Arc::new(
            FixtureAcpClient::new(
                AcpSession::default(),
                Ok(AcpTurn {
                    events: Vec::new(),
                    binding_update: None,
                    completed: true,
                }),
            )
            .with_commands(vec![AcpRuntimeCommand {
                name: "runtime-only".to_string(),
                description: Some("Announced by the active ACP session.".to_string()),
            }]),
        ));

        let response = provider
            .discover_commands(AgentCommandDiscoverRequest {
                agent_id: Some(vibex_core::AgentId::parse(registry::CODEX_AGENT_ID).unwrap()),
                provider_profile_id: None,
                session_id: Some(VibexSessionId::new()),
                workspace_id: None,
                trigger: Some(AgentCommandTrigger::Slash),
                query: None,
                limit: None,
            })
            .await
            .unwrap();

        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].label, "/runtime-only");
        assert!(
            !response
                .entries
                .iter()
                .any(|entry| entry.label == "/status")
        );
        assert!(response.diagnostics.iter().any(|diagnostic| {
            diagnostic.key == "catalogSource" && diagnostic.value == "acp-session-runtime"
        }));
    }

    #[tokio::test]
    async fn empty_live_acp_catalog_suppresses_the_codex_pre_session_catalog() {
        let provider = AcpAgentProvider::new(Arc::new(
            FixtureAcpClient::new(
                AcpSession::default(),
                Ok(AcpTurn {
                    events: Vec::new(),
                    binding_update: None,
                    completed: true,
                }),
            )
            .with_commands(Vec::new()),
        ));

        let response = provider
            .discover_commands(AgentCommandDiscoverRequest {
                agent_id: Some(vibex_core::AgentId::parse(registry::CODEX_AGENT_ID).unwrap()),
                provider_profile_id: None,
                session_id: Some(VibexSessionId::new()),
                workspace_id: None,
                trigger: Some(AgentCommandTrigger::Slash),
                query: None,
                limit: None,
            })
            .await
            .unwrap();

        assert!(response.entries.is_empty());
        assert!(response.diagnostics.iter().any(|diagnostic| {
            diagnostic.key == "catalogSource" && diagnostic.value == "acp-session-runtime"
        }));
    }

    #[tokio::test]
    async fn skips_static_commands_for_generic_codex_acp_profile() {
        let db_path = temp_db_path("commands-generic-profile");
        let service = ProviderConfigService::new(db_path.clone());
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse(registry::CODEX_AGENT_ID).unwrap()),
                display_name: "Generic ACP".to_string(),
                account_alias: None,
                preset_id: None,
                config: Some(AcpProviderConfig {
                    command: "/usr/bin/custom-acp".to_string(),
                    args: vec!["acp".to_string()],
                    env: Vec::new(),
                    cwd_template: Some("{workspaceRoot}".to_string()),
                    process_strategy: vibex_core::AcpProcessStrategy::default(),
                    terminal_tools: false,
                    terminal_auth: false,
                    models: vec!["generic".to_string()],
                    modes: vec!["default".to_string()],
                    features: vec!["slash_commands".to_string(), "skills".to_string()],
                    disabled_tools: Vec::new(),
                }),
            })
            .unwrap();
        let provider = AcpAgentProvider::with_config_service(
            Arc::new(FixtureAcpClient::new(
                AcpSession::default(),
                Ok(AcpTurn {
                    events: Vec::new(),
                    binding_update: None,
                    completed: true,
                }),
            )),
            service,
        );

        let response = provider
            .discover_commands(AgentCommandDiscoverRequest {
                agent_id: Some(profile.agent_id.clone()),
                provider_profile_id: Some(profile.id.clone()),
                session_id: None,
                workspace_id: None,
                trigger: Some(AgentCommandTrigger::Slash),
                query: None,
                limit: None,
            })
            .await
            .unwrap();

        let capabilities = provider.capabilities_for_profile(Some(&profile.id));
        assert!(capabilities.slash_commands);
        assert!(capabilities.skills);
        assert!(response.entries.is_empty());
        assert!(response.diagnostics.iter().any(|diagnostic| {
            diagnostic.key == "catalogSource" && diagnostic.value == "unsupported-acp-profile"
        }));

        cleanup_db(db_path);
    }

    #[test]
    fn smoke_config_redacts_env_values() {
        let config = AcpProviderConfig {
            command: "/usr/bin/opencode".to_string(),
            args: vec!["acp".to_string()],
            env: vec![vibex_core::AcpProviderEnvReference {
                key: "OPENCODE_AUTH_TOKEN".to_string(),
                source: AcpProviderEnvSource::Literal,
                value: Some("secret-token-value".to_string()),
                secret_lookup_key: Some("secret/backend/key".to_string()),
                redacted_hint: "configured".to_string(),
            }],
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: vibex_core::AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: vec!["opencode-default".to_string()],
            modes: vec!["default".to_string()],
            features: vec!["agent_messages".to_string()],
            disabled_tools: Vec::new(),
        };

        let redacted = RedactedAcpProviderConfig::from_config(&config);
        let json = serde_json::to_string(&redacted).unwrap();

        assert!(json.contains("OPENCODE_AUTH_TOKEN"));
        assert!(!json.contains("secret-token-value"));
        assert!(!json.contains("secret/backend/key"));
        assert!(!redacted.env[0].value_stored);
        assert!(!redacted.env[0].secret_lookup_key_stored);
    }

    #[test]
    fn opencode_runtime_config_uses_typed_acp_profile() {
        let config = AcpProviderConfig {
            command: "/usr/bin/opencode".to_string(),
            args: vec!["acp".to_string()],
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: vibex_core::AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: vec!["opencode-default".to_string()],
            modes: vec!["default".to_string()],
            features: vec![
                "agent_messages".to_string(),
                "tool_calls".to_string(),
                "permission_requests".to_string(),
                "slash_commands".to_string(),
                "skills".to_string(),
            ],
            disabled_tools: Vec::new(),
        };

        let runtime = OpenCodeAcpRuntimeConfig::from_acp_config(&config).unwrap();

        assert_eq!(runtime.command, PathBuf::from("/usr/bin/opencode"));
        assert_eq!(runtime.args, vec!["acp"]);
        assert_eq!(runtime.cwd_template.as_deref(), Some("{workspaceRoot}"));
        assert_eq!(runtime.model.as_deref(), Some("opencode-default"));
        assert_eq!(runtime.mode.as_deref(), Some("default"));
        assert!(runtime.features.contains(&"agent_messages".to_string()));
        assert!(runtime.features.contains(&"slash_commands".to_string()));
        assert!(runtime.features.contains(&"skills".to_string()));
    }

    #[test]
    fn opencode_runtime_rejects_non_acp_args() {
        let config = AcpProviderConfig {
            command: "/usr/bin/opencode".to_string(),
            args: vec!["serve".to_string()],
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: vibex_core::AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: vec!["opencode-default".to_string()],
            modes: vec!["default".to_string()],
            features: Vec::new(),
            disabled_tools: Vec::new(),
        };

        let err = OpenCodeAcpRuntimeConfig::from_acp_config(&config).unwrap_err();

        assert_eq!(err.code, "acp_opencode_args_missing_acp");
    }

    #[test]
    fn opencode_runtime_args_preserve_typed_stdio_command() {
        let runtime = OpenCodeAcpRuntimeConfig {
            command: PathBuf::from("/usr/bin/opencode"),
            args: vec!["acp".to_string()],
            cwd_template: Some("{workspaceRoot}".to_string()),
            model: Some("opencode-default".to_string()),
            mode: Some("default".to_string()),
            features: Vec::new(),
        };

        let args = runtime.stdio_args();

        assert_eq!(args, vec!["acp"]);
        assert!(!args.iter().any(|arg| arg == "--port"));
        assert!(!args.iter().any(|arg| arg == "--hostname"));
    }

    #[test]
    fn redacted_args_summary_removes_secret_like_values() {
        let summary = redacted_args_summary(&[
            "acp".to_string(),
            "--token".to_string(),
            "plain-secret-value".to_string(),
            "--mode".to_string(),
            "safe".to_string(),
        ]);

        assert!(summary.contains("[redacted]"));
        assert!(!summary.contains("plain-secret-value"));
        assert!(summary.ends_with("--mode safe"));
    }

    #[test]
    fn timeline_summary_preserves_sensitive_vocabulary_without_secret_values() {
        for summary in [
            "Read file '/workspace/src/authentication.rs'",
            "Search for 'token_usage' in registry.rs",
            "Run cargo test secret_store",
            "Inspect password policy",
            r#"{"path":"/workspace/src/auth.rs","query":"supports_logout"}"#,
        ] {
            assert_eq!(redact_timeline_summary(summary), summary);
            assert!(!contains_sensitive_data(summary));
        }
    }

    #[test]
    fn timeline_summary_redacts_structured_credentials() {
        for summary in [
            "OPENAI_API_KEY=sk-sensitive-value",
            "curl --token private-value",
            "Authorization: Bearer private-value",
            r#"{"token":"private-value","path":"src/lib.rs"}"#,
            "-----BEGIN PRIVATE KEY----- private material",
        ] {
            assert_eq!(redact_timeline_summary(summary), REDACTED_SENSITIVE_OUTPUT);
            assert!(contains_sensitive_data(summary));
        }
    }

    #[test]
    fn session_update_chunks_map_to_prompt_result() {
        let mut result = OpenCodeAcpPromptResult::default();
        collect_session_update(
            &mut result,
            Some(&json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": { "type": "text", "text": "thinking" }
                }
            })),
        );
        collect_session_update(
            &mut result,
            Some(&json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "answer" }
                }
            })),
        );
        collect_session_update(
            &mut result,
            Some(&json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 1
                }
            })),
        );

        assert_eq!(result.reasoning_text, "thinking");
        assert_eq!(result.assistant_text, "answer");
    }

    #[test]
    fn opencode_permission_request_summary_maps_tool_payload() {
        let summary = summarize_opencode_permission_request(&json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "session/request_permission",
            "params": {
                "toolCall": {
                    "toolCallId": "tool-1",
                    "title": "Run shell command",
                    "kind": "bash"
                },
                "options": [
                    { "kind": "allow_once" },
                    { "kind": "reject_once" }
                ]
            }
        }));

        assert_eq!(summary.provider_request_id.as_deref(), Some("42"));
        assert_eq!(summary.risk_category, PermissionRiskCategory::Command);
        assert_eq!(summary.title, "Run shell command");
        assert!(
            summary
                .details
                .iter()
                .any(|detail| { detail.label == "toolCallId" && detail.value == "tool-1" })
        );
        assert!(
            summary
                .details
                .iter()
                .any(|detail| { detail.label == "optionCount" && detail.value == "2" })
        );
    }

    #[test]
    fn opencode_permission_request_summary_redacts_secret_like_values() {
        let summary = summarize_opencode_permission_request(&json!({
            "jsonrpc": "2.0",
            "id": "sk-secret-request-id",
            "method": "session/request_permission",
            "params": {
                "toolCall": {
                    "toolCallId": "token-tool-call",
                    "title": "Run command with sk-secret-token",
                    "kind": "bash"
                },
                "options": [
                    { "kind": "allow_once" }
                ]
            }
        }));
        let serialized = format!("{summary:?}");

        assert_eq!(
            summary.provider_request_id.as_deref(),
            Some(REDACTED_SENSITIVE_OUTPUT)
        );
        assert_eq!(summary.title, "OpenCode ACP permission request");
        assert!(!serialized.contains("sk-secret"));
        assert!(!serialized.contains("token-tool-call"));
    }

    #[test]
    fn session_config_snapshot_extracts_model_config_options() {
        let snapshot = extract_session_config_snapshot(&json!({
            "sessionId": "session-1",
            "configOptions": [
                {
                    "category": "model",
                    "currentValue": "anthropic/claude-sonnet-4",
                    "options": [
                        { "modelId": "anthropic/claude-sonnet-4", "name": "Claude Sonnet 4" },
                        { "value": "openai/gpt-5", "name": "GPT-5" }
                    ]
                },
                {
                    "category": "mode",
                    "currentValue": "default",
                    "options": ["default"]
                }
            ]
        }));

        assert_eq!(
            snapshot.status,
            OpenCodeAcpSessionConfigSnapshotStatus::Available
        );
        assert!(
            snapshot
                .response_keys
                .contains(&"configOptions".to_string())
        );
        assert_eq!(snapshot.config_option_categories, vec!["model", "mode"]);
        assert_eq!(snapshot.model_option_count, 2);
        assert_eq!(
            snapshot.current_model_id.as_deref(),
            Some("anthropic/claude-sonnet-4")
        );
        assert_eq!(
            snapshot.current_model_name.as_deref(),
            Some("Claude Sonnet 4")
        );
        assert_eq!(snapshot.model_options_sample.len(), 2);
        assert!(!snapshot.raw_payload_stored);
    }

    #[test]
    fn session_config_snapshot_bounds_samples_and_redacts_secret_like_values() {
        let snapshot = extract_session_config_snapshot(&json!({
            "sessionId": "session-1",
            "availableModels": [
                { "modelId": "sk-secret-model", "name": "Secret Model" },
                { "modelId": "provider/model-2", "name": "Model 2" },
                { "modelId": "provider/model-3", "name": "Model 3" },
                { "modelId": "provider/model-4", "name": "Model 4" },
                { "modelId": "provider/model-5", "name": "Model 5" },
                { "modelId": "provider/model-6", "name": "Model 6" }
            ],
            "currentModelId": "sk-secret-model"
        }));

        assert_eq!(snapshot.model_option_count, 6);
        assert_eq!(
            snapshot.current_model_id.as_deref(),
            Some("[redacted-sensitive-output]")
        );
        assert_eq!(snapshot.model_options_sample.len(), 5);
        assert_eq!(
            snapshot.model_options_sample[0].model_id,
            "[redacted-sensitive-output]"
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("sk-secret-model"));
        assert!(!serialized.contains("provider/model-6"));
    }

    #[test]
    fn session_config_snapshot_is_unavailable_without_optional_model_fields() {
        let snapshot = extract_session_config_snapshot(&json!({
            "sessionId": "session-1"
        }));

        assert_eq!(
            snapshot.status,
            OpenCodeAcpSessionConfigSnapshotStatus::Unavailable
        );
        assert_eq!(snapshot.response_keys, vec!["sessionId"]);
        assert!(snapshot.config_option_categories.is_empty());
        assert_eq!(snapshot.model_option_count, 0);
        assert!(snapshot.current_model_id.is_none());
        assert!(snapshot.model_options_sample.is_empty());
        assert!(!snapshot.raw_payload_stored);
    }

    #[test]
    fn acp_prompt_content_maps_local_image_attachments() {
        let attachment = ProviderTurnAttachment {
            label: "screenshot.png".to_string(),
            mime_type: Some("image/png".to_string()),
            uri: Some("file:///tmp/screenshot.png".to_string()),
            local_path: Some(PathBuf::from("/tmp/screenshot.png")),
        };

        let content = acp_prompt_content("inspect this", &[attachment]);

        assert_eq!(
            content,
            vec![
                serde_json::json!({
                    "type": "text",
                    "text": "inspect this"
                }),
                serde_json::json!({
                    "type": "localImage",
                    "path": "/tmp/screenshot.png"
                })
            ]
        );
    }

    #[tokio::test]
    async fn opencode_runtime_rejects_forbidden_workspace_before_spawn() {
        let client = OpenCodeAcpClient::new(OpenCodeAcpRuntimeConfig {
            command: PathBuf::from("/usr/bin/opencode"),
            args: vec!["acp".to_string()],
            cwd_template: Some("{workspaceRoot}".to_string()),
            model: Some("opencode-default".to_string()),
            mode: Some("default".to_string()),
            features: Vec::new(),
        });

        let err = client
            .create_session(AcpCreateSessionRequest {
                session_id: VibexSessionId::new(),
                provider_profile_id: vibex_core::ProviderProfileId::parse(
                    "provider_local_default_acp",
                )
                .unwrap(),
                model: None,
                workspace_root: vibex_agent::forbidden_agent_smoke_root()
                    .join("vibex")
                    .display()
                    .to_string(),
                runtime_resources: ProviderRuntimeResources::default(),
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, "validation/acp_opencode_workspace_forbidden");
    }

    #[tokio::test]
    async fn opencode_runtime_reports_missing_absolute_binary() {
        let workspace = std::env::temp_dir().join(format!(
            "vibex-agent-acp-missing-binary-{}",
            RequestId::new().as_str()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let client = OpenCodeAcpClient::new(OpenCodeAcpRuntimeConfig {
            command: workspace.join("missing-opencode"),
            args: vec!["acp".to_string()],
            cwd_template: Some("{workspaceRoot}".to_string()),
            model: Some("opencode-default".to_string()),
            mode: Some("default".to_string()),
            features: Vec::new(),
        });

        let err = client
            .create_session(AcpCreateSessionRequest {
                session_id: VibexSessionId::new(),
                provider_profile_id: vibex_core::ProviderProfileId::parse(
                    "provider_local_default_acp",
                )
                .unwrap(),
                model: None,
                workspace_root: workspace.display().to_string(),
                runtime_resources: ProviderRuntimeResources::default(),
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, "process/acp_opencode_binary_missing");
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn create_session_redacts_secret_like_metadata() {
        let provider = AcpAgentProvider::new(Arc::new(FixtureAcpClient::new(
            AcpSession {
                native_session_id: Some("acp-session-1".to_string()),
                native_thread_id: Some("acp-thread-1".to_string()),
                native_resume_token: Some("secret-token-value".to_string()),
                session_config_state: None,
                redacted_metadata: vec![
                    ProviderBindingMetadata {
                        key: "client".to_string(),
                        value: "fixture".to_string(),
                    },
                    ProviderBindingMetadata {
                        key: "api_key".to_string(),
                        value: "sk-should-not-persist".to_string(),
                    },
                ],
            },
            Ok(AcpTurn {
                events: Vec::new(),
                binding_update: None,
                completed: true,
            }),
        )));

        let handle = provider
            .create_session(ProviderCreateRequest {
                session_id: vibex_core::VibexSessionId::new(),
                provider_profile_id: vibex_core::ProviderProfileId::parse(
                    "provider_local_default_acp",
                )
                .unwrap(),
                model: None,
                workspace_root: "/tmp/vibex-acp".to_string(),
                safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                runtime_resources: ProviderRuntimeResources::default(),
            })
            .await
            .unwrap();

        assert_eq!(handle.binding.provider_kind, ProviderKind::Acp);
        assert_eq!(handle.binding.native.native_resume_token, None);
        assert_eq!(handle.binding.native.redacted_metadata.len(), 1);
        assert_eq!(handle.binding.native.redacted_metadata[0].key, "client");
        let binding_json = format!("{:?}", handle.binding);
        assert!(!binding_json.contains("sk-should-not-persist"));
        assert!(!binding_json.contains("secret-token-value"));
    }

    #[tokio::test]
    async fn maps_acp_events_to_provider_neutral_timeline_payloads() {
        let session_id = vibex_core::VibexSessionId::new();
        let binding = test_binding(session_id.clone());
        let provider = AcpAgentProvider::new(Arc::new(FixtureAcpClient::new(
            AcpSession::default(),
            Ok(AcpTurn {
                events: vec![
                    AcpEvent::Reasoning {
                        text: "thinking".to_string(),
                        is_final: true,
                    },
                    AcpEvent::AssistantDelta {
                        text_delta: "hello".to_string(),
                        chunk_index: 0,
                        phase: Some(AgentMessagePhase::FinalAnswer),
                    },
                    AcpEvent::ToolCall {
                        tool_call_id: "tool-1".to_string(),
                        tool_name: "read_file".to_string(),
                        status: ToolCallStatus::Completed,
                        summary: "Read file".to_string(),
                        input_summary: Some("README.md".to_string()),
                        output_summary: Some("ok".to_string()),
                    },
                    AcpEvent::PermissionRequest {
                        request_id: None,
                        provider_request_id: Some("perm-1".to_string()),
                        risk_category: PermissionRiskCategory::Command,
                        title: "Run command".to_string(),
                        details: vec![PermissionActionDetail {
                            label: "command".to_string(),
                            value: "echo acp".to_string(),
                        }],
                        options: Vec::new(),
                    },
                    AcpEvent::Unknown {
                        event_kind: "future_event".to_string(),
                    },
                ],
                binding_update: None,
                completed: false,
            }),
        )));

        let result = provider
            .send_turn(
                ProviderSessionHandle {
                    binding: binding.clone(),
                    capabilities: provider.capabilities(),
                },
                ProviderTurnRequest {
                    session_id,
                    message_submission_id: None,
                    required_runtime: None,
                    text: "hello".to_string(),
                    attachments: Vec::new(),
                    workspace_root: "/tmp/vibex-acp".to_string(),
                    binding,
                    safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
                    runtime_resources: Default::default(),
                    execution_identity: None,
                    event_sender: None,
                    binding_update_sender: None,
                    usage_execution_context: None,
                    usage_counter_origin: AgentUsageCounterOrigin::Unknown,
                    usage_event_sender: None,
                },
            )
            .await
            .unwrap();

        assert!(!result.completed);
        assert_eq!(result.events.len(), 5);
        assert!(matches!(
            result.events[0].payload,
            TimelinePayload::Reasoning(_)
        ));
        assert!(matches!(
            result.events[1].payload,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                phase: Some(AgentMessagePhase::FinalAnswer),
                ..
            })
        ));
        let legacy_tool_correlation = result.events[2]
            .provider_correlation_id
            .as_deref()
            .expect("legacy tool event must have a correlation id");
        assert!(legacy_tool_correlation.starts_with("acp_event_"));
        assert!(!legacy_tool_correlation.contains("tool-1"));
        match &result.events[2].payload {
            TimelinePayload::ToolCall(payload) => {
                assert_eq!(payload.tool_call_id, legacy_tool_correlation);
                assert!(!payload.tool_call_id.contains("tool-1"));
            }
            other => panic!("expected legacy tool payload, got {other:?}"),
        }
        assert!(matches!(
            result.events[3].payload,
            TimelinePayload::PermissionRequest(_)
        ));
        assert!(matches!(
            result.events[4].payload,
            TimelinePayload::SystemNotice(_)
        ));
    }

    fn test_binding(session_id: vibex_core::VibexSessionId) -> ProviderBinding {
        ProviderBinding {
            session_id,
            provider_kind: ProviderKind::Acp,
            auth_source: vibex_core::RuntimeAuthSource::provider_profile(
                vibex_core::ProviderProfileId::parse("provider_local_default_acp").unwrap(),
            ),
            auth_source_revision: 1,
            native: ProviderNativeBinding::empty(),
            created_at_ms: unix_timestamp_ms(),
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-agent-acp-{label}-{}.db",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn cleanup_db(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}

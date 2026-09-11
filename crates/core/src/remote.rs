use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::agent::{
    AgentSession, AgentSessionSummary, AgentTimelineDisplaySettings, ContinueAgentTurnRequest,
    CreateAgentSessionRequest, FetchTimelineRequest, ForkAgentSessionRequest,
    GetMessageSubmissionRequest, MessageSubmissionState, RenameAgentSessionRequest,
    ReplaceUserMessagePayload, ResolveElicitationRequest, ResolvePermissionRequest,
    SendAgentMessageRequest,
};
use crate::agent_auth::{
    AgentAuthCatalog, AgentAuthContext, AgentAuthContextAuthenticateRequest,
    AgentAuthContextAuthenticateResult, AgentAuthContextCancelAuthenticationRequest,
    AgentAuthContextLogoutPreview, AgentAuthContextLogoutRequest, AgentAuthContextMutationResult,
    AgentAuthContextRefreshModelsRequest, AgentAuthContextVerifyRequest,
    AgentAuthenticationOperation,
};
use crate::agent_config::{
    AgentCatalogListResponse, AgentConfigStatus, AgentId, AgentManagedInstallState,
    AgentRefreshSnapshotRequest, AgentRefreshSnapshotResponse, AgentRuntimeOptionProbeRequest,
    AgentRuntimeOptionProbeResult, AgentRuntimeStatus, AgentSnapshotEntry,
    AgentUpdateConfigRequest,
};
use crate::agent_provider_runtime::{
    AgentRuntimeProbeCancelRequest, AgentRuntimeProbeListRequest, AgentRuntimeProbeRecord,
    AgentRuntimeProbeStartRequest,
};
use crate::automation_graph::{
    AutomationGraph, AutomationGraphCreateRequest, AutomationGraphDefinitionUpdateRequest,
    AutomationGraphListRequest, AutomationGraphStatus, AutomationGraphUpdateRequest, AutomationRun,
    AutomationRunCancelRequest, AutomationRunListRequest, AutomationRunResumeRequest,
    AutomationRunStartRequest, AutomationRunStep, AutomationRunStepListRequest,
};
use crate::error::VibexError;
use crate::file::{
    FileMutationRequest, FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult,
    FileTreeEntry, FileTreeRequest, FileWriteRequest,
};
use crate::git::{
    GitBlameRequest, GitBlameResponse, GitBranchCheckoutRequest, GitBranchCreateRequest,
    GitBranchListResponse, GitCommitDetail, GitCommitDetailRequest, GitCommitRequest,
    GitCommitResult, GitDiffRequest, GitDiffResponse, GitHistoryRequest, GitHistoryResponse,
    GitProjectEligibility, GitRemoteActionRequest, GitRemoteActionResult, GitStageRequest,
    GitStatusSummary, GitWorktreeLifecycleSnapshot,
};
use crate::ids::{
    AgentAuthContextId, AgentRuntimeProbeId, CorrelationId, DeviceId, EventId, RequestId,
    RuntimeProcessId, TerminalId, VibexSessionId,
};
use crate::provider::{
    AcpProviderCatalogListResponse, AcpProviderConfig, AcpProviderProfileUpdateRequest,
    AgentModelProviderProfile, AgentModelProviderProfileCreateRequest,
    AgentModelProviderProfileSecretValueResponse,
    AgentModelProviderProfileSecretValueUpdateRequest, AgentModelProviderProfileUpdateRequest,
    Hook, HookCreateRequest, HookDeleteRequest, HookInstallPreview, HookInstallPreviewRequest,
    HookUpdateRequest, McpServer, McpServerAgentMatrix, McpServerAgentMatrixListRequest,
    McpServerCreateRequest, McpServerDeleteRequest, McpServerDiscoverRequest,
    McpServerDiscoveryResponse, McpServerImportRequest, McpServerImportResult,
    McpServerSetAgentMatrixRequest, McpServerUpdateRequest, McpServerValidateRequest,
    McpServerValidationResult, Prompt, PromptCreateRequest, PromptDeleteRequest,
    PromptUpdateRequest, PromptValidateRequest, PromptValidationResult, ProviderCapabilitySummary,
    ProviderFailoverRecommendation, ProviderFailoverRecommendationRequest, ProviderHealthSummary,
    ProviderInjectionPreview, ProviderInjectionPreviewRequest, ProviderNativeExportApplyRequest,
    ProviderNativeExportApplyResult, ProviderNativeExportListRequest, ProviderNativeExportPreview,
    ProviderNativeExportPreviewRequest, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackRequest, ProviderNativeExportRollbackResult,
    ProviderNativeImportCreateRequest, ProviderNativeImportCreateResult,
    ProviderNativeImportPreview, ProviderNativeImportPreviewRequest, ProviderProfile,
    ProviderProfileDefaultScope, ProviderProfileSummary, ProviderRunHealthProbesRequest,
    ProviderRunHealthProbesResult, ProviderUsageListRequest, ProviderUsageSummary, Skill,
    SkillAgentMatrix, SkillAgentMatrixListRequest, SkillCreateRequest, SkillDeleteRequest,
    SkillDiscoverRequest, SkillDiscoveryResponse, SkillImportRequest, SkillImportResult,
    SkillSetAgentMatrixRequest, SkillUpdateRequest, SkillValidateRequest, SkillValidationResult,
};
use crate::provider_projection::{
    AgentModelProviderBinding, AgentProviderProjectionCapability,
    AgentProviderProjectionCapabilityRequest, AgentProviderProjectionPreview,
    AgentProviderProjectionPreviewRequest, AgentRuntimeProfile,
};
use crate::runtime::{
    AgentSessionRuntimeSelectionState, AgentSessionRuntimeSnapshot, AttachRuntimeRequest,
    AttachRuntimeResponse, CancelAgentSessionRuntimeSwitchRequest, DetachRuntimeRequest,
    DetachRuntimeResponse, GetRuntimeEventsRequest, RuntimeEventBatch, RuntimeProcessSnapshot,
    SessionRuntimeOptionCatalog, SetDesiredAgentSessionRuntimeRequest,
};
use crate::scheduled_task::{
    ScheduledTask, ScheduledTaskAttentionListRequest, ScheduledTaskAttentionSummary,
    ScheduledTaskAuditListRequest, ScheduledTaskAuditRecord, ScheduledTaskCreateRequest,
    ScheduledTaskListRequest, ScheduledTaskRun, ScheduledTaskRunListRequest,
    ScheduledTaskUpdateRequest,
};
use crate::terminal::{
    TerminalCreateRequest, TerminalResizeRequest, TerminalSession, TerminalSnapshot,
    TerminalWriteRequest,
};
use crate::time::unix_timestamp_ms;
use crate::timeline::{TimelineItem, TimelinePage};
use crate::workspace::{OpenWorkspaceRequest, ProjectWorkspaceSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl RemoteProtocolVersion {
    pub const fn foundation() -> Self {
        Self { major: 0, minor: 4 }
    }
}

impl Default for RemoteProtocolVersion {
    fn default() -> Self {
        Self::foundation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCapabilitySummary {
    pub protocol_version: RemoteProtocolVersion,
    pub supports_pairing: bool,
    pub supports_auth: bool,
    pub supports_catch_up: bool,
    pub supports_agent_sessions: bool,
    #[serde(default)]
    pub supports_runtime_lifecycle: bool,
    #[serde(default)]
    pub supports_seamless_runtime_selection: bool,
    #[serde(default)]
    pub supports_agent_account_auth: bool,
    pub supports_workspace_files: bool,
    pub supports_git: bool,
    #[serde(default)]
    pub supports_worktree_read: bool,
    pub supports_terminal: bool,
    pub supports_provider_settings: bool,
    /// Provider, Agent runtime, binding, and Secret management mutations are
    /// exposed to full-control devices. Older runtimes omit the field.
    #[serde(default)]
    pub supports_provider_management: bool,
    /// Scheduled-task management is exposed to paired devices. Older runtimes
    /// omit the field.
    #[serde(default)]
    pub supports_scheduled_tasks: bool,
    /// Automation graph and run management is exposed to paired devices.
    #[serde(default)]
    pub supports_automation: bool,
    /// The aggregated Config Center read bundle is served to paired devices.
    #[serde(default)]
    pub supports_management_snapshot: bool,
    pub live_event_channels: Vec<RemoteLiveEventChannel>,
}

impl RemoteCapabilitySummary {
    pub fn foundation() -> Self {
        Self {
            protocol_version: RemoteProtocolVersion::foundation(),
            supports_pairing: true,
            supports_auth: true,
            supports_catch_up: true,
            supports_agent_sessions: false,
            supports_runtime_lifecycle: false,
            supports_seamless_runtime_selection: false,
            supports_agent_account_auth: false,
            supports_workspace_files: false,
            supports_git: false,
            supports_worktree_read: false,
            supports_terminal: false,
            supports_provider_settings: false,
            supports_provider_management: false,
            supports_scheduled_tasks: false,
            supports_automation: false,
            supports_management_snapshot: false,
            live_event_channels: vec![RemoteLiveEventChannel::System],
        }
    }

    pub fn with_agent_sessions() -> Self {
        let mut capabilities = Self::foundation();
        capabilities.supports_agent_sessions = true;
        capabilities
            .live_event_channels
            .push(RemoteLiveEventChannel::AgentSession);
        capabilities
    }

    pub fn with_agent_and_workbench() -> Self {
        let mut capabilities = Self::with_agent_sessions();
        capabilities.supports_workspace_files = true;
        capabilities.supports_git = true;
        capabilities.supports_worktree_read = true;
        capabilities.supports_terminal = true;
        capabilities
    }

    pub fn with_agent_workbench_and_provider() -> Self {
        let mut capabilities = Self::with_agent_and_workbench();
        capabilities.supports_provider_settings = true;
        capabilities.supports_provider_management = true;
        capabilities
            .live_event_channels
            .push(RemoteLiveEventChannel::Provider);
        capabilities
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDevicePermissionLevel {
    ReadOnly,
    ApproveOnly,
    FullControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDeviceStatus {
    Pending,
    Active,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceSummary {
    pub device_id: DeviceId,
    pub display_name: String,
    pub permission_level: RemoteDevicePermissionLevel,
    pub status: RemoteDeviceStatus,
    pub paired_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceDetail {
    pub device_id: DeviceId,
    pub display_name: String,
    pub public_key: Option<String>,
    #[serde(default)]
    pub grant_revision: u64,
    pub permission_level: RemoteDevicePermissionLevel,
    pub status: RemoteDeviceStatus,
    pub paired_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl RemoteDeviceDetail {
    pub fn summary(&self) -> RemoteDeviceSummary {
        RemoteDeviceSummary {
            device_id: self.device_id.clone(),
            display_name: self.display_name.clone(),
            permission_level: self.permission_level,
            status: self.status,
            paired_at_ms: self.paired_at_ms,
            last_seen_at_ms: self.last_seen_at_ms,
            revoked_at_ms: self.revoked_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingCode {
    pub pairing_id: RequestId,
    pub permission_level: RemoteDevicePermissionLevel,
    pub expires_at_ms: i64,
    pub claimed_device_id: Option<DeviceId>,
    pub created_at_ms: i64,
    pub claimed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreatePairingCodeRequest {
    pub permission_level: RemoteDevicePermissionLevel,
    pub ttl_ms: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreatePairingCodeResponse {
    pub pairing: RemotePairingCode,
    pub pairing_code: String,
}

impl fmt::Debug for RemoteCreatePairingCodeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteCreatePairingCodeResponse")
            .field("pairing", &self.pairing)
            .field("has_pairing_code", &!self.pairing_code.is_empty())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaimPairingCodeRequest {
    pub pairing_code: String,
    pub display_name: String,
    pub public_key: Option<String>,
}

impl fmt::Debug for RemoteClaimPairingCodeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteClaimPairingCodeRequest")
            .field("has_pairing_code", &!self.pairing_code.is_empty())
            .field("display_name", &self.display_name)
            .field("has_public_key", &self.public_key.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaimPairingCodeResponse {
    pub device: RemoteDeviceDetail,
    pub auth_token: String,
}

impl fmt::Debug for RemoteClaimPairingCodeResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteClaimPairingCodeResponse")
            .field("device", &self.device)
            .field("has_auth_token", &!self.auth_token.is_empty())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRevokeDeviceRequest {
    pub device_id: DeviceId,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAuthContext {
    pub device_id: DeviceId,
    pub display_name: String,
    pub permission_level: RemoteDevicePermissionLevel,
    pub authenticated_at_ms: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAuthProof {
    pub device_id: DeviceId,
    pub auth_token: String,
}

impl fmt::Debug for RemoteAuthProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteAuthProof")
            .field("device_id", &self.device_id)
            .field("has_auth_token", &!self.auth_token.is_empty())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteActionClass {
    ReadProject,
    ReadAgentSession,
    ResolvePermission,
    ResolveElicitation,
    MutateAgentSession,
    MutateAgentAuthentication,
    MutateFile,
    MutateGit,
    MutateTerminal,
    ReadProviderSettings,
    MutateProviderSettings,
    ReadDeviceManagement,
    MutateDeviceManagement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAuditAction {
    PairingCodeCreated,
    PairingCodeClaimed,
    PairingCodeRejected,
    PairingOfferCreated,
    PairingOfferClaimed,
    PairingOfferCanceled,
    PairingOfferRejected,
    DeviceAuthenticated,
    DeviceAuthFailed,
    DeviceRevoked,
    PermissionAllowed,
    PermissionDenied,
    MutationAllowed,
    MutationDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAuditTargetKind {
    PairingCode,
    PairingOffer,
    Device,
    Permission,
    Elicitation,
    AgentSession,
    AgentAuthentication,
    WorkspaceFile,
    Git,
    Terminal,
    ProviderSettings,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAuditOutcome {
    Allowed,
    Denied,
    Failed,
    Created,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAuditRecord {
    pub audit_id: RequestId,
    pub device_id: Option<DeviceId>,
    pub action: RemoteAuditAction,
    pub target_kind: RemoteAuditTargetKind,
    pub target_id: Option<String>,
    pub outcome: RemoteAuditOutcome,
    pub redacted_summary: String,
    pub request_id: Option<RequestId>,
    pub correlation_id: Option<CorrelationId>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAuditListRequest {
    pub device_id: Option<DeviceId>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAuditListResponse {
    pub records: Vec<RemoteAuditRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAgentOperationKind {
    ListSessions,
    GetSession,
    CreateSession,
    RenameSession,
    ArchiveSession,
    DeleteSession,
    ForkSession,
    FetchTimeline,
    GetTimelineDisplaySettings,
    ResolveOpaqueLocator,
    ListRuntimeOptions,
    ListAuthContexts,
    EnsureDefaultAuthContext,
    RefreshAuthMethods,
    UpdateAuthEnvironment,
    AuthenticateAgent,
    CancelAuthentication,
    ListAuthMethods,
    AuthenticateContext,
    GetAuthenticationOperation,
    CancelContextAuthentication,
    VerifyAuthContext,
    RefreshAuthModels,
    PreviewAuthLogout,
    LogoutAuthContext,
    GetRuntimeSelection,
    SetDesiredRuntime,
    CancelRuntimeSwitch,
    GetMessageSubmission,
    DiscoverCommands,
    ExecuteCommand,
    LogoutAgent,
    ReplaceUserMessage,
    SendMessage,
    ContinueTurn,
    Interrupt,
    ResolvePermission,
    ResolveElicitation,
    CatchUp,
    GetRuntimeSnapshot,
    GetRuntimeProcessSnapshot,
    GetSessionTokenUsage,
    UsageStatistics,
    GetRuntimeEvents,
    AttachRuntime,
    DetachRuntime,
    GetSidebarOrganization,
    MutateSidebarOrganization,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionListRequest {
    pub auth: RemoteAuthProof,
    pub include_archived: Option<bool>,
    pub timeline_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionListResponse {
    pub sessions: Vec<AgentSessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionDetailRequest {
    pub auth: RemoteAuthProof,
    pub session_id: VibexSessionId,
    pub timeline_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionDetailResponse {
    pub session: AgentSession,
    pub latest_timeline: TimelinePage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCreateSessionRequest {
    pub auth: RemoteAuthProof,
    pub request: CreateAgentSessionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCreateSessionResponse {
    pub session: AgentSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentForkSessionRequest {
    pub auth: RemoteAuthProof,
    pub request: ForkAgentSessionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentForkSessionResponse {
    pub session: AgentSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentTimelineDisplaySettingsRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentTimelineDisplaySettingsResponse {
    pub settings: AgentTimelineDisplaySettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRenameSessionRequest {
    pub auth: RemoteAuthProof,
    pub request: RenameAgentSessionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRenameSessionResponse {
    pub session: AgentSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionActionRequest {
    pub auth: RemoteAuthProof,
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionActionResponse {
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentTimelineFetchRequest {
    pub auth: RemoteAuthProof,
    pub request: FetchTimelineRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentTimelineFetchResponse {
    pub page: TimelinePage,
}

/// The desktop resolves a push/deep-link locator after the client has
/// authenticated.  The locator is deliberately not interpreted by the Web
/// host; only the PC knows how it maps to an authoritative session or request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDeepLinkResolutionStatus {
    Resolved,
    NotFound,
    Expired,
    Revoked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeepLinkResolution {
    pub notification_id: String,
    pub status: RemoteDeepLinkResolutionStatus,
    pub session_id: Option<VibexSessionId>,
    pub permission_request_id: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDeepLinkResolveRequest {
    pub auth: RemoteAuthProof,
    pub notification_id: String,
    pub opaque_locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDeepLinkResolveResponse {
    pub resolution: RemoteDeepLinkResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeSelectionRequest {
    pub auth: RemoteAuthProof,
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeSelectionResponse {
    pub state: AgentSessionRuntimeSelectionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeOptionsRequest {
    pub auth: RemoteAuthProof,
    /// Older clients omit this field and receive a Provider-only projection so
    /// they never deserialize an authentication-source variant they do not know.
    #[serde(default)]
    pub supports_agent_account_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeOptionsResponse {
    pub catalog: SessionRuntimeOptionCatalog,
}

/// Seeds the per-Agent authentication context if the authority has none.
///
/// The Config Center needs a context before it can show or change credentials,
/// and the authority owns that row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentEnsureDefaultAuthContextRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentEnsureDefaultAuthContextResponse {
    pub context: AgentAuthContext,
}

/// Re-probes the authentication methods an Agent advertises.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRefreshAuthMethodsRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRefreshAuthMethodsResponse {
    pub catalog: AgentAuthCatalog,
}

/// Carries the credentials an Agent sign-in method collected to the
/// authoritative runtime, which owns the Provider profile they belong to.
///
/// Same posture as [`RemoteProviderCredentialSecretMutationRequest`]: this is a
/// wire type that may contain Secret values, the transport is always E2EE or
/// TLS, `Debug` never prints a value, and the runtime stores them in its own
/// secret store before answering with the redacted profile.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUpdateAuthEnvironmentRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
    pub provider_profile_id: crate::ProviderProfileId,
    pub method_id: String,
    pub values: Vec<RemoteAgentAuthEnvironmentValue>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthEnvironmentValue {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub clear: bool,
}

impl fmt::Debug for RemoteAgentAuthEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteAgentAuthEnvironmentValue")
            .field("name", &self.name)
            .field("has_value", &self.value.is_some())
            .field("secret", &self.secret)
            .field("optional", &self.optional)
            .field("clear", &self.clear)
            .finish()
    }
}

impl fmt::Debug for RemoteAgentUpdateAuthEnvironmentRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteAgentUpdateAuthEnvironmentRequest")
            .field("auth", &self.auth)
            .field("agent_id", &self.agent_id)
            .field("provider_profile_id", &self.provider_profile_id)
            .field("method_id", &self.method_id)
            .field("value_count", &self.values.len())
            .finish()
    }
}

impl RemoteAgentUpdateAuthEnvironmentRequest {
    pub fn from_request(
        auth: RemoteAuthProof,
        request: crate::AgentAuthEnvironmentUpdateRequest,
    ) -> Self {
        Self {
            auth,
            agent_id: request.agent_id,
            provider_profile_id: request.provider_profile_id,
            method_id: request.method_id,
            values: request
                .values
                .into_iter()
                .map(|value| RemoteAgentAuthEnvironmentValue {
                    name: value.name,
                    value: value.value,
                    secret: value.secret,
                    optional: value.optional,
                    clear: value.clear,
                })
                .collect(),
        }
    }

    pub fn into_request(self) -> crate::AgentAuthEnvironmentUpdateRequest {
        crate::AgentAuthEnvironmentUpdateRequest {
            agent_id: self.agent_id,
            provider_profile_id: self.provider_profile_id,
            method_id: self.method_id,
            values: self
                .values
                .into_iter()
                .map(|value| crate::AgentAuthEnvironmentValue {
                    name: value.name,
                    value: value.value,
                    secret: value.secret,
                    optional: value.optional,
                    clear: value.clear,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUpdateAuthEnvironmentResponse {
    pub profile: crate::ProviderProfile,
}

/// Runs the legacy per-Agent interactive sign-in for a Provider profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthenticateRequest {
    pub auth: RemoteAuthProof,
    pub operation_id: crate::AgentAuthenticationOperationId,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile_id: Option<crate::ProviderProfileId>,
    pub method_id: String,
}

impl RemoteAgentAuthenticateRequest {
    pub fn from_request(auth: RemoteAuthProof, request: crate::AgentAuthenticateRequest) -> Self {
        Self {
            auth,
            operation_id: request.operation_id,
            agent_id: request.agent_id,
            provider_profile_id: request.provider_profile_id,
            method_id: request.method_id,
        }
    }

    pub fn into_request(self) -> crate::AgentAuthenticateRequest {
        crate::AgentAuthenticateRequest {
            operation_id: self.operation_id,
            agent_id: self.agent_id,
            provider_profile_id: self.provider_profile_id,
            method_id: self.method_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthenticateResponse {
    pub method_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<crate::TerminalAuthActionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCancelAuthenticationRequest {
    pub auth: RemoteAuthProof,
    pub operation_id: crate::AgentAuthenticationOperationId,
    pub agent_id: AgentId,
}

impl RemoteAgentCancelAuthenticationRequest {
    pub fn from_request(
        auth: RemoteAuthProof,
        request: crate::AgentAuthenticationCancelRequest,
    ) -> Self {
        Self {
            auth,
            operation_id: request.operation_id,
            agent_id: request.agent_id,
        }
    }

    pub fn into_request(self) -> crate::AgentAuthenticationCancelRequest {
        crate::AgentAuthenticationCancelRequest {
            operation_id: self.operation_id,
            agent_id: self.agent_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCancelAuthenticationResponse {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthContextListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthContextListResponse {
    pub contexts: Vec<AgentAuthContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthMethodListRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: crate::AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthMethodListResponse {
    pub catalog: AgentAuthCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthenticateContextRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentAuthContextAuthenticateRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthenticateContextResponse {
    pub result: AgentAuthContextAuthenticateResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthenticationOperationRequest {
    pub auth: RemoteAuthProof,
    pub operation_id: crate::AgentAuthenticationOperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthenticationOperationResponse {
    pub operation: AgentAuthenticationOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCancelContextAuthenticationRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentAuthContextCancelAuthenticationRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthContextMutationResponse {
    pub result: AgentAuthContextMutationResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentVerifyAuthContextRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentAuthContextVerifyRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRefreshAuthModelsRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentAuthContextRefreshModelsRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthLogoutPreviewRequest {
    pub auth: RemoteAuthProof,
    pub auth_context_id: AgentAuthContextId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAuthLogoutPreviewResponse {
    pub preview: AgentAuthContextLogoutPreview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentLogoutAuthContextRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentAuthContextLogoutRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSetDesiredRuntimeRequest {
    pub auth: RemoteAuthProof,
    pub request: SetDesiredAgentSessionRuntimeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSetDesiredRuntimeResponse {
    pub state: AgentSessionRuntimeSelectionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCancelRuntimeSwitchRequest {
    pub auth: RemoteAuthProof,
    pub request: CancelAgentSessionRuntimeSwitchRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCancelRuntimeSwitchResponse {
    pub state: AgentSessionRuntimeSelectionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeSnapshotRequest {
    pub auth: RemoteAuthProof,
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeSnapshotResponse {
    pub snapshot: AgentSessionRuntimeSnapshot,
}

/// Live token counters for one session. The authority merges its persisted
/// usage facts with the currently attached runtime, so a paired client sees
/// the same snapshot the local workbench does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionTokenUsageRequest {
    pub auth: RemoteAuthProof,
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSessionTokenUsageResponse {
    pub usage: Option<crate::AgentTokenUsage>,
}

/// Aggregated Agent usage for the Usage view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUsageStatisticsRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentUsageStatisticsRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUsageStatisticsResponse {
    pub statistics: crate::AgentUsageStatistics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProcessSnapshotRequest {
    pub auth: RemoteAuthProof,
    pub process_id: RuntimeProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProcessSnapshotResponse {
    pub snapshot: RuntimeProcessSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeEventsRequest {
    pub auth: RemoteAuthProof,
    pub request: GetRuntimeEventsRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeEventsResponse {
    pub batch: RuntimeEventBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAttachRuntimeRequest {
    pub auth: RemoteAuthProof,
    pub request: AttachRuntimeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentAttachRuntimeResponse {
    pub response: AttachRuntimeResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDetachRuntimeRequest {
    pub auth: RemoteAuthProof,
    pub request: DetachRuntimeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDetachRuntimeResponse {
    pub response: DetachRuntimeResponse,
}

/// Releases the credentials stored for one Agent and Provider profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentLogoutRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile_id: Option<crate::ProviderProfileId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentLogoutResponse {
    pub logged_out: bool,
}

/// Resolves one composer trigger against the authority's Agent catalogue,
/// workspace file tree and Skills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDiscoverCommandsRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentCommandDiscoverRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDiscoverCommandsResponse {
    pub discovery: crate::AgentCommandDiscovery,
}

/// Executes one composer command resolved against the authority's catalogue,
/// so a remote client never runs a command against a runtime it does not own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentExecuteCommandRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentCommandExecuteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentExecuteCommandResponse {
    pub result: crate::AgentCommandExecuteResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentMessageSubmissionRequest {
    pub auth: RemoteAuthProof,
    pub request: GetMessageSubmissionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentMessageSubmissionResponse {
    pub submission: MessageSubmissionState,
}

/// Replaces the latest user message of a session and re-runs the turn.
///
/// The desktop timeline editor rewrites the last user message, so the mutation
/// has to reach the authority that owns the session timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentReplaceUserMessageRequest {
    pub auth: RemoteAuthProof,
    #[serde(flatten)]
    pub payload: ReplaceUserMessagePayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentReplaceUserMessageResponse {
    pub items: Vec<TimelineItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSendMessageRequest {
    pub auth: RemoteAuthProof,
    pub request: SendAgentMessageRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSendMessageResponse {
    pub appended_items: Vec<TimelineItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentContinueTurnRequest {
    pub auth: RemoteAuthProof,
    pub request: ContinueAgentTurnRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentContinueTurnResponse {
    pub appended_items: Vec<TimelineItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentInterruptRequest {
    pub auth: RemoteAuthProof,
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentInterruptResponse {
    pub interrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentResolvePermissionRequest {
    pub auth: RemoteAuthProof,
    pub request: ResolvePermissionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentResolvePermissionResponse {
    pub item: TimelineItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentResolveElicitationRequest {
    pub auth: RemoteAuthProof,
    pub request: ResolveElicitationRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentResolveElicitationResponse {
    pub item: TimelineItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentTimelineCursor {
    pub session_id: VibexSessionId,
    pub after_sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCatchUpRequest {
    pub auth: RemoteAuthProof,
    pub cursors: Vec<RemoteAgentTimelineCursor>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCatchUpResponse {
    pub events: Vec<RemoteLiveEventEnvelope>,
    pub next_cursors: Vec<RemoteAgentTimelineCursor>,
    pub compacted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteAgentRequest {
    ListSessions(RemoteAgentSessionListRequest),
    GetSession(RemoteAgentSessionDetailRequest),
    CreateSession(RemoteAgentCreateSessionRequest),
    ForkSession(RemoteAgentForkSessionRequest),
    RenameSession(RemoteAgentRenameSessionRequest),
    ArchiveSession(RemoteAgentSessionActionRequest),
    DeleteSession(RemoteAgentSessionActionRequest),
    FetchTimeline(RemoteAgentTimelineFetchRequest),
    GetTimelineDisplaySettings(RemoteAgentTimelineDisplaySettingsRequest),
    ResolveOpaqueLocator(RemoteAgentDeepLinkResolveRequest),
    ListRuntimeOptions(RemoteAgentRuntimeOptionsRequest),
    ListAuthContexts(RemoteAgentAuthContextListRequest),
    EnsureDefaultAuthContext(RemoteAgentEnsureDefaultAuthContextRequest),
    RefreshAuthMethods(RemoteAgentRefreshAuthMethodsRequest),
    UpdateAuthEnvironment(RemoteAgentUpdateAuthEnvironmentRequest),
    AuthenticateAgent(RemoteAgentAuthenticateRequest),
    CancelAuthentication(RemoteAgentCancelAuthenticationRequest),
    ListAuthMethods(RemoteAgentAuthMethodListRequest),
    AuthenticateContext(RemoteAgentAuthenticateContextRequest),
    GetAuthenticationOperation(RemoteAgentAuthenticationOperationRequest),
    CancelContextAuthentication(RemoteAgentCancelContextAuthenticationRequest),
    VerifyAuthContext(RemoteAgentVerifyAuthContextRequest),
    RefreshAuthModels(RemoteAgentRefreshAuthModelsRequest),
    PreviewAuthLogout(RemoteAgentAuthLogoutPreviewRequest),
    LogoutAuthContext(RemoteAgentLogoutAuthContextRequest),
    GetRuntimeSelection(RemoteAgentRuntimeSelectionRequest),
    SetDesiredRuntime(RemoteAgentSetDesiredRuntimeRequest),
    CancelRuntimeSwitch(RemoteAgentCancelRuntimeSwitchRequest),
    GetMessageSubmission(RemoteAgentMessageSubmissionRequest),
    DiscoverCommands(RemoteAgentDiscoverCommandsRequest),
    ExecuteCommand(RemoteAgentExecuteCommandRequest),
    LogoutAgent(RemoteAgentLogoutRequest),
    ReplaceUserMessage(RemoteAgentReplaceUserMessageRequest),
    SendMessage(RemoteAgentSendMessageRequest),
    ContinueTurn(RemoteAgentContinueTurnRequest),
    Interrupt(RemoteAgentInterruptRequest),
    ResolvePermission(RemoteAgentResolvePermissionRequest),
    ResolveElicitation(RemoteAgentResolveElicitationRequest),
    CatchUp(RemoteAgentCatchUpRequest),
    GetRuntimeSnapshot(RemoteAgentRuntimeSnapshotRequest),
    GetRuntimeProcessSnapshot(RemoteAgentRuntimeProcessSnapshotRequest),
    GetSessionTokenUsage(RemoteAgentSessionTokenUsageRequest),
    UsageStatistics(RemoteAgentUsageStatisticsRequest),
    GetRuntimeEvents(RemoteAgentRuntimeEventsRequest),
    AttachRuntime(RemoteAgentAttachRuntimeRequest),
    DetachRuntime(RemoteAgentDetachRuntimeRequest),
    GetSidebarOrganization(RemoteSidebarOrganizationRequest),
    MutateSidebarOrganization(RemoteSidebarOrganizationMutateRequest),
}

impl RemoteAgentRequest {
    pub const fn operation_kind(&self) -> RemoteAgentOperationKind {
        match self {
            Self::ListSessions(_) => RemoteAgentOperationKind::ListSessions,
            Self::GetSession(_) => RemoteAgentOperationKind::GetSession,
            Self::CreateSession(_) => RemoteAgentOperationKind::CreateSession,
            Self::ForkSession(_) => RemoteAgentOperationKind::ForkSession,
            Self::RenameSession(_) => RemoteAgentOperationKind::RenameSession,
            Self::ArchiveSession(_) => RemoteAgentOperationKind::ArchiveSession,
            Self::DeleteSession(_) => RemoteAgentOperationKind::DeleteSession,
            Self::FetchTimeline(_) => RemoteAgentOperationKind::FetchTimeline,
            Self::GetTimelineDisplaySettings(_) => {
                RemoteAgentOperationKind::GetTimelineDisplaySettings
            }
            Self::ResolveOpaqueLocator(_) => RemoteAgentOperationKind::ResolveOpaqueLocator,
            Self::ListRuntimeOptions(_) => RemoteAgentOperationKind::ListRuntimeOptions,
            Self::ListAuthContexts(_) => RemoteAgentOperationKind::ListAuthContexts,
            Self::EnsureDefaultAuthContext(_) => RemoteAgentOperationKind::EnsureDefaultAuthContext,
            Self::RefreshAuthMethods(_) => RemoteAgentOperationKind::RefreshAuthMethods,
            Self::UpdateAuthEnvironment(_) => RemoteAgentOperationKind::UpdateAuthEnvironment,
            Self::AuthenticateAgent(_) => RemoteAgentOperationKind::AuthenticateAgent,
            Self::CancelAuthentication(_) => RemoteAgentOperationKind::CancelAuthentication,
            Self::ListAuthMethods(_) => RemoteAgentOperationKind::ListAuthMethods,
            Self::AuthenticateContext(_) => RemoteAgentOperationKind::AuthenticateContext,
            Self::GetAuthenticationOperation(_) => {
                RemoteAgentOperationKind::GetAuthenticationOperation
            }
            Self::CancelContextAuthentication(_) => {
                RemoteAgentOperationKind::CancelContextAuthentication
            }
            Self::VerifyAuthContext(_) => RemoteAgentOperationKind::VerifyAuthContext,
            Self::RefreshAuthModels(_) => RemoteAgentOperationKind::RefreshAuthModels,
            Self::PreviewAuthLogout(_) => RemoteAgentOperationKind::PreviewAuthLogout,
            Self::LogoutAuthContext(_) => RemoteAgentOperationKind::LogoutAuthContext,
            Self::GetRuntimeSelection(_) => RemoteAgentOperationKind::GetRuntimeSelection,
            Self::SetDesiredRuntime(_) => RemoteAgentOperationKind::SetDesiredRuntime,
            Self::CancelRuntimeSwitch(_) => RemoteAgentOperationKind::CancelRuntimeSwitch,
            Self::GetMessageSubmission(_) => RemoteAgentOperationKind::GetMessageSubmission,
            Self::DiscoverCommands(_) => RemoteAgentOperationKind::DiscoverCommands,
            Self::ExecuteCommand(_) => RemoteAgentOperationKind::ExecuteCommand,
            Self::LogoutAgent(_) => RemoteAgentOperationKind::LogoutAgent,
            Self::ReplaceUserMessage(_) => RemoteAgentOperationKind::ReplaceUserMessage,
            Self::SendMessage(_) => RemoteAgentOperationKind::SendMessage,
            Self::ContinueTurn(_) => RemoteAgentOperationKind::ContinueTurn,
            Self::Interrupt(_) => RemoteAgentOperationKind::Interrupt,
            Self::ResolvePermission(_) => RemoteAgentOperationKind::ResolvePermission,
            Self::ResolveElicitation(_) => RemoteAgentOperationKind::ResolveElicitation,
            Self::CatchUp(_) => RemoteAgentOperationKind::CatchUp,
            Self::GetRuntimeSnapshot(_) => RemoteAgentOperationKind::GetRuntimeSnapshot,
            Self::GetRuntimeProcessSnapshot(_) => {
                RemoteAgentOperationKind::GetRuntimeProcessSnapshot
            }
            Self::GetSessionTokenUsage(_) => RemoteAgentOperationKind::GetSessionTokenUsage,
            Self::UsageStatistics(_) => RemoteAgentOperationKind::UsageStatistics,
            Self::GetRuntimeEvents(_) => RemoteAgentOperationKind::GetRuntimeEvents,
            Self::AttachRuntime(_) => RemoteAgentOperationKind::AttachRuntime,
            Self::DetachRuntime(_) => RemoteAgentOperationKind::DetachRuntime,
            Self::GetSidebarOrganization(_) => RemoteAgentOperationKind::GetSidebarOrganization,
            Self::MutateSidebarOrganization(_) => {
                RemoteAgentOperationKind::MutateSidebarOrganization
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteWorkbenchOperationKind {
    ListWorkspaces,
    OpenWorkspace,
    EnsureTemporarySessionRoot,
    DeleteWorkspace,
    DeleteProject,
    FileListTree,
    FileRead,
    FileSearch,
    FileWrite,
    FileDelete,
    FileRename,
    FileCreateDirectory,
    FileCopy,
    GitStatus,
    GitDiff,
    GitStage,
    GitUnstage,
    GitRevert,
    GitCommit,
    GitHistory,
    GitCommitDetail,
    GitBlame,
    GitBranchList,
    GitBranchCreate,
    GitBranchCheckout,
    GitRemoteAction,
    GitWorktreeEligibility,
    GitWorktreeSnapshot,
    GitWorktreeRenameBranch,
    GitWorktreeLifecycle,
    TerminalList,
    TerminalCreate,
    TerminalSnapshot,
    TerminalWrite,
    TerminalResize,
    TerminalKill,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchListWorkspacesRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchListWorkspacesResponse {
    pub workspaces: Vec<ProjectWorkspaceSummary>,
}

/// Asks the authority to create and resolve its own temporary session root, so
/// a client without a published workspace never proposes a path that only
/// exists on the client machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchTemporarySessionRootRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchTemporarySessionRootResponse {
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchOpenWorkspaceRequest {
    pub auth: RemoteAuthProof,
    pub request: OpenWorkspaceRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchOpenWorkspaceResponse {
    pub summary: ProjectWorkspaceSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchDeleteWorkspaceRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchDeleteWorkspaceResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchDeleteProjectRequest {
    pub auth: RemoteAuthProof,
    pub project_id: crate::ids::ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkbenchDeleteProjectResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileTreeRequest {
    pub auth: RemoteAuthProof,
    pub request: FileTreeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileTreeResponse {
    pub entries: Vec<FileTreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileReadRequest {
    pub auth: RemoteAuthProof,
    pub request: FileReadRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileReadResponse {
    pub file: FileReadResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileSearchRequest {
    pub auth: RemoteAuthProof,
    pub request: FileSearchRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileSearchResponse {
    pub results: Vec<FileSearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileWriteRequest {
    pub auth: RemoteAuthProof,
    pub request: FileWriteRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileWriteResponse {
    pub file: FileReadResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileMutationRequest {
    pub auth: RemoteAuthProof,
    pub request: FileMutationRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileRenameResponse {
    pub entry: FileTreeEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileCreateDirectoryResponse {
    pub entry: FileTreeEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileCopyResponse {
    pub entry: FileTreeEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitStatusRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitStatusResponse {
    pub status: GitStatusSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeEligibilityRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeEligibilityResponse {
    pub eligibility: GitProjectEligibility,
}

/// Renames the branch backing a managed worktree.
///
/// The desktop sidebar renames the worktree and its Git branch together, and
/// the authority owns both, so the rename travels over Remote v2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeRenameBranchRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
    pub new_branch: String,
}

/// Managed-worktree lifecycle over Remote v2.
///
/// `expected_revision` and `idempotency_key` travel in the create payload
/// because the v2 envelope's mutation metadata is not visible to the workbench
/// dispatcher, and the authority records the same operation journal entry a
/// local create would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeCreateRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeCreateResponse {
    pub result: crate::GitWorktreeCreateResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeReadinessRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeReadinessRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeReadinessResponse {
    pub record: crate::GitWorktreeReadinessRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeMergePlanRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeMergeRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeMergePlanResponse {
    pub plan: crate::GitWorktreeMergePlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeMergeRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeMergeRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeConflictResolveRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeConflictResolveRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeConflictStageRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeConflictStageRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeAssistanceSessionRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeAssistanceSessionRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeOperationRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeOperationRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeOperationResponse {
    pub record: crate::GitWorktreeOperationRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeArchiveRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeArchiveRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeRestoreRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeRestoreRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeDiscardRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::GitWorktreeDiscardRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreePreflightResponse {
    pub preflight: crate::GitWorktreeDestructivePreflight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeRenameBranchResponse {
    pub renamed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeSnapshotRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitWorktreeSnapshotResponse {
    pub snapshot: GitWorktreeLifecycleSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitDiffRequest {
    pub auth: RemoteAuthProof,
    pub request: GitDiffRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitDiffResponse {
    pub diff: GitDiffResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitStageRequest {
    pub auth: RemoteAuthProof,
    pub request: GitStageRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitStatusMutationResponse {
    pub status: GitStatusSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitCommitRequest {
    pub auth: RemoteAuthProof,
    pub request: GitCommitRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitCommitResponse {
    pub result: GitCommitResult,
    pub status_after: GitStatusSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitHistoryRequest {
    pub auth: RemoteAuthProof,
    pub request: GitHistoryRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitHistoryResponse {
    pub history: GitHistoryResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitCommitDetailRequest {
    pub auth: RemoteAuthProof,
    pub request: GitCommitDetailRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitCommitDetailResponse {
    pub detail: GitCommitDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitBlameRequest {
    pub auth: RemoteAuthProof,
    pub request: GitBlameRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitBlameResponse {
    pub blame: GitBlameResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitBranchListRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitBranchListResponse {
    pub branches: GitBranchListResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitBranchCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: GitBranchCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitBranchCheckoutRequest {
    pub auth: RemoteAuthProof,
    pub request: GitBranchCheckoutRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitRemoteActionRequest {
    pub auth: RemoteAuthProof,
    pub request: GitRemoteActionRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGitRemoteActionResponse {
    pub result: GitRemoteActionResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalListRequest {
    pub auth: RemoteAuthProof,
    pub workspace_id: crate::ids::WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalListResponse {
    pub terminals: Vec<TerminalSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: TerminalCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalCreateResponse {
    pub terminal: TerminalSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalSnapshotRequest {
    pub auth: RemoteAuthProof,
    pub terminal_id: TerminalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalSnapshotResponse {
    pub snapshot: TerminalSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalWriteRequest {
    pub auth: RemoteAuthProof,
    pub request: TerminalWriteRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalWriteResponse {
    pub written: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalResizeRequest {
    pub auth: RemoteAuthProof,
    pub request: TerminalResizeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalResizeResponse {
    pub terminal: TerminalSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalKillRequest {
    pub auth: RemoteAuthProof,
    pub terminal_id: TerminalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTerminalKillResponse {
    pub terminal: TerminalSession,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteWorkbenchRequest {
    ListWorkspaces(RemoteWorkbenchListWorkspacesRequest),
    OpenWorkspace(RemoteWorkbenchOpenWorkspaceRequest),
    EnsureTemporarySessionRoot(RemoteWorkbenchTemporarySessionRootRequest),
    DeleteWorkspace(RemoteWorkbenchDeleteWorkspaceRequest),
    DeleteProject(RemoteWorkbenchDeleteProjectRequest),
    FileListTree(RemoteFileTreeRequest),
    FileRead(RemoteFileReadRequest),
    FileSearch(RemoteFileSearchRequest),
    FileWrite(RemoteFileWriteRequest),
    FileDelete(RemoteFileMutationRequest),
    FileRename(RemoteFileMutationRequest),
    FileCreateDirectory(RemoteFileMutationRequest),
    FileCopy(RemoteFileMutationRequest),
    GitStatus(RemoteGitStatusRequest),
    GitDiff(RemoteGitDiffRequest),
    GitStage(RemoteGitStageRequest),
    GitUnstage(RemoteGitStageRequest),
    GitRevert(RemoteGitStageRequest),
    GitCommit(RemoteGitCommitRequest),
    GitHistory(RemoteGitHistoryRequest),
    GitCommitDetail(RemoteGitCommitDetailRequest),
    GitBlame(RemoteGitBlameRequest),
    GitBranchList(RemoteGitBranchListRequest),
    GitBranchCreate(RemoteGitBranchCreateRequest),
    GitBranchCheckout(RemoteGitBranchCheckoutRequest),
    GitRemoteAction(RemoteGitRemoteActionRequest),
    GitWorktreeEligibility(RemoteGitWorktreeEligibilityRequest),
    GitWorktreeSnapshot(RemoteGitWorktreeSnapshotRequest),
    GitWorktreeRenameBranch(RemoteGitWorktreeRenameBranchRequest),
    GitWorktreeCreate(RemoteGitWorktreeCreateRequest),
    GitWorktreeSetReadiness(RemoteGitWorktreeReadinessRequest),
    GitWorktreeMergePlan(RemoteGitWorktreeMergePlanRequest),
    GitWorktreeMerge(RemoteGitWorktreeMergeRequest),
    GitWorktreeResolveConflict(RemoteGitWorktreeConflictResolveRequest),
    GitWorktreeStageConflicts(RemoteGitWorktreeConflictStageRequest),
    GitWorktreeBindAssistanceSession(RemoteGitWorktreeAssistanceSessionRequest),
    GitWorktreeContinueMerge(RemoteGitWorktreeOperationRequest),
    GitWorktreeAbortMerge(RemoteGitWorktreeOperationRequest),
    GitWorktreeArchivePreflight(RemoteGitWorktreeArchiveRequest),
    GitWorktreeArchive(RemoteGitWorktreeArchiveRequest),
    GitWorktreeRestorePreflight(RemoteGitWorktreeRestoreRequest),
    GitWorktreeRestore(RemoteGitWorktreeRestoreRequest),
    GitWorktreeDiscardPreflight(RemoteGitWorktreeDiscardRequest),
    GitWorktreeDiscard(RemoteGitWorktreeDiscardRequest),
    TerminalList(RemoteTerminalListRequest),
    TerminalCreate(RemoteTerminalCreateRequest),
    TerminalSnapshot(RemoteTerminalSnapshotRequest),
    TerminalWrite(RemoteTerminalWriteRequest),
    TerminalResize(RemoteTerminalResizeRequest),
    TerminalKill(RemoteTerminalKillRequest),
}

impl RemoteWorkbenchRequest {
    pub const fn operation_kind(&self) -> RemoteWorkbenchOperationKind {
        match self {
            Self::ListWorkspaces(_) => RemoteWorkbenchOperationKind::ListWorkspaces,
            Self::OpenWorkspace(_) => RemoteWorkbenchOperationKind::OpenWorkspace,
            Self::EnsureTemporarySessionRoot(_) => {
                RemoteWorkbenchOperationKind::EnsureTemporarySessionRoot
            }
            Self::DeleteWorkspace(_) => RemoteWorkbenchOperationKind::DeleteWorkspace,
            Self::DeleteProject(_) => RemoteWorkbenchOperationKind::DeleteProject,
            Self::FileListTree(_) => RemoteWorkbenchOperationKind::FileListTree,
            Self::FileRead(_) => RemoteWorkbenchOperationKind::FileRead,
            Self::FileSearch(_) => RemoteWorkbenchOperationKind::FileSearch,
            Self::FileWrite(_) => RemoteWorkbenchOperationKind::FileWrite,
            Self::FileDelete(_) => RemoteWorkbenchOperationKind::FileDelete,
            Self::FileRename(_) => RemoteWorkbenchOperationKind::FileRename,
            Self::FileCreateDirectory(_) => RemoteWorkbenchOperationKind::FileCreateDirectory,
            Self::FileCopy(_) => RemoteWorkbenchOperationKind::FileCopy,
            Self::GitStatus(_) => RemoteWorkbenchOperationKind::GitStatus,
            Self::GitDiff(_) => RemoteWorkbenchOperationKind::GitDiff,
            Self::GitStage(_) => RemoteWorkbenchOperationKind::GitStage,
            Self::GitUnstage(_) => RemoteWorkbenchOperationKind::GitUnstage,
            Self::GitRevert(_) => RemoteWorkbenchOperationKind::GitRevert,
            Self::GitCommit(_) => RemoteWorkbenchOperationKind::GitCommit,
            Self::GitHistory(_) => RemoteWorkbenchOperationKind::GitHistory,
            Self::GitCommitDetail(_) => RemoteWorkbenchOperationKind::GitCommitDetail,
            Self::GitBlame(_) => RemoteWorkbenchOperationKind::GitBlame,
            Self::GitBranchList(_) => RemoteWorkbenchOperationKind::GitBranchList,
            Self::GitBranchCreate(_) => RemoteWorkbenchOperationKind::GitBranchCreate,
            Self::GitBranchCheckout(_) => RemoteWorkbenchOperationKind::GitBranchCheckout,
            Self::GitRemoteAction(_) => RemoteWorkbenchOperationKind::GitRemoteAction,
            Self::GitWorktreeEligibility(_) => RemoteWorkbenchOperationKind::GitWorktreeEligibility,
            Self::GitWorktreeSnapshot(_) => RemoteWorkbenchOperationKind::GitWorktreeSnapshot,
            Self::GitWorktreeRenameBranch(_) => {
                RemoteWorkbenchOperationKind::GitWorktreeRenameBranch
            }
            Self::GitWorktreeCreate(_)
            | Self::GitWorktreeSetReadiness(_)
            | Self::GitWorktreeMergePlan(_)
            | Self::GitWorktreeMerge(_)
            | Self::GitWorktreeResolveConflict(_)
            | Self::GitWorktreeStageConflicts(_)
            | Self::GitWorktreeBindAssistanceSession(_)
            | Self::GitWorktreeContinueMerge(_)
            | Self::GitWorktreeAbortMerge(_)
            | Self::GitWorktreeArchivePreflight(_)
            | Self::GitWorktreeArchive(_)
            | Self::GitWorktreeRestorePreflight(_)
            | Self::GitWorktreeRestore(_)
            | Self::GitWorktreeDiscardPreflight(_)
            | Self::GitWorktreeDiscard(_) => RemoteWorkbenchOperationKind::GitWorktreeLifecycle,
            Self::TerminalList(_) => RemoteWorkbenchOperationKind::TerminalList,
            Self::TerminalCreate(_) => RemoteWorkbenchOperationKind::TerminalCreate,
            Self::TerminalSnapshot(_) => RemoteWorkbenchOperationKind::TerminalSnapshot,
            Self::TerminalWrite(_) => RemoteWorkbenchOperationKind::TerminalWrite,
            Self::TerminalResize(_) => RemoteWorkbenchOperationKind::TerminalResize,
            Self::TerminalKill(_) => RemoteWorkbenchOperationKind::TerminalKill,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProviderOperationKind {
    ListAgentSummaries,
    ListProfiles,
    PreviewInjection,
    ProjectionCapability,
    ProjectionPreview,
    StartRuntimeProbe,
    GetRuntimeProbe,
    ListRuntimeProbes,
    CancelRuntimeProbe,
    ListHealthSummaries,
    RunHealthProbes,
    ListUsageSummaries,
    ListFailoverRecommendations,
    ListAgents,
    UpdateAgentConfig,
    RefreshAgentSnapshot,
    ProbeAgentRuntimeOptions,
    DiscoverOwnedModelCatalog,
    InstallManagedAgent,
    CheckManagedAgentUpdate,
    UninstallManagedAgent,
    DeleteAgentAuthCatalog,
    CreateCustomAgent,
    DeleteCustomAgent,
    ListModelProviderProfiles,
    CreateModelProviderProfile,
    UpdateModelProviderProfile,
    ListAgentRuntimeProfiles,
    CreateAgentRuntimeProfile,
    UpdateAgentRuntimeProfile,
    ListAgentModelProviderBindings,
    CreateAgentModelProviderBinding,
    UpdateAgentModelProviderBinding,
    MutateProviderCredentialSecret,
    SetAgentModelProviderDefault,
    DeleteAgentModelProviderProfile,
    CreateAgentModelProviderProfile,
    UpdateAgentModelProviderProfile,
    MutateAgentModelProviderProfileSecret,
    GetAgentModelProviderDisplayOrder,
    SetAgentModelProviderDisplayOrder,
    TestAgentModelProviderProfile,
    FetchAgentModelProviderProfileModels,
    ListCapabilitySummaries,
    RunCapabilityProbes,
    ListMcpServers,
    CreateMcpServer,
    UpdateMcpServer,
    DeleteMcpServer,
    SetMcpServerAgentMatrix,
    ListMcpServerAgentMatrix,
    DiscoverMcpSources,
    ImportMcpServers,
    ValidateMcpServer,
    ManagementSnapshot,
    RefreshDetectedAgentVersions,
    ListAgentCatalog,
    ListAcpCatalogPresets,
    GetAcpProfileConfig,
    UpdateAcpProfileConfig,
    PreviewNativeImport,
    CreateProfileFromImport,
    PreviewNativeExport,
    ApplyNativeExport,
    RollbackNativeExport,
    ListNativeExports,
    SkillList,
    SkillCreate,
    SkillUpdate,
    SkillDelete,
    SkillAgentMatrixSet,
    SkillAgentMatrixList,
    SkillDiscover,
    SkillImport,
    SkillValidate,
    PromptList,
    PromptCreate,
    PromptUpdate,
    PromptDelete,
    PromptValidate,
    HookList,
    HookCreate,
    HookUpdate,
    HookDelete,
    HookPreviewInstall,
}

/// Redacted Agent configuration state for remote management surfaces. Command
/// lines, environment values, native paths, parameters, and diagnostics are
/// intentionally absent from this wire type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentConfigSummary {
    pub id: AgentId,
    pub label: String,
    pub enabled: bool,
    pub installed: bool,
    pub configured: bool,
    pub config_status: AgentConfigStatus,
    pub runtime_status: AgentRuntimeStatus,
    pub model_count: usize,
    pub updated_at_ms: Option<i64>,
}

impl From<&AgentSnapshotEntry> for RemoteAgentConfigSummary {
    fn from(agent: &AgentSnapshotEntry) -> Self {
        Self {
            id: agent.id.clone(),
            label: agent.label.clone(),
            enabled: agent.enabled,
            installed: agent.installed,
            configured: agent.configured,
            config_status: agent.config_status,
            runtime_status: agent.runtime_status,
            model_count: agent.models.len(),
            updated_at_ms: agent.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentConfigSummaryListRequest {
    pub auth: RemoteAuthProof,
    pub include_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentConfigSummaryListResponse {
    pub agents: Vec<RemoteAgentConfigSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderProfileListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderProfileListResponse {
    pub profiles: Vec<ProviderProfileSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderInjectionPreviewRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderInjectionPreviewRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderInjectionPreviewResponse {
    pub preview: ProviderInjectionPreview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProjectionCapabilityRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentProviderProjectionCapabilityRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProjectionCapabilityResponse {
    pub capability: AgentProviderProjectionCapability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProjectionPreviewRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentProviderProjectionPreviewRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProjectionPreviewResponse {
    pub preview: AgentProviderProjectionPreview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeStartRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentRuntimeProbeStartRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeStartResponse {
    pub probe: AgentRuntimeProbeRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeGetRequest {
    pub auth: RemoteAuthProof,
    pub probe_id: AgentRuntimeProbeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeGetResponse {
    pub probe: Option<AgentRuntimeProbeRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeListRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentRuntimeProbeListRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeListResponse {
    pub probes: Vec<AgentRuntimeProbeRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeCancelRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentRuntimeProbeCancelRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProbeCancelResponse {
    pub probe: AgentRuntimeProbeRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHealthSummaryListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHealthSummaryListResponse {
    pub summaries: Vec<ProviderHealthSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRunHealthProbesRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderRunHealthProbesRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRunHealthProbesResponse {
    pub result: ProviderRunHealthProbesResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderUsageSummaryListRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderUsageListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderUsageSummaryListResponse {
    pub summaries: Vec<ProviderUsageSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderFailoverRecommendationListRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderFailoverRecommendationRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderFailoverRecommendationListResponse {
    pub recommendations: Vec<ProviderFailoverRecommendation>,
}

/// Full Agent snapshot list for remote management surfaces. Environment
/// values are blanked, and native configuration paths and diagnostics are
/// dropped before the entries leave the runtime; see
/// [`redact_agent_snapshot_for_remote`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRefreshAgentSnapshotRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentRefreshSnapshotRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRefreshAgentSnapshotResponse {
    pub response: AgentRefreshSnapshotResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProbeRuntimeOptionsRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentRuntimeOptionProbeRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProbeRuntimeOptionsResponse {
    pub result: AgentRuntimeOptionProbeResult,
}

/// Discovers the model catalogue an Agent's own CLI advertises.
///
/// The Agent bridge runs on the authority, so the client asks it for the list
/// instead of launching a local process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentOwnedModelCatalogRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
    pub provider_profile_id: crate::ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentOwnedModelCatalogResponse {
    pub models: Vec<crate::ProviderConfiguredModel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentInstallManagedAgentRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentInstallManagedAgentResponse {
    pub state: AgentManagedInstallState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCheckManagedAgentUpdateRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentCheckManagedAgentUpdateResponse {
    pub state: AgentManagedInstallState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUninstallManagedAgentRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUninstallManagedAgentResponse {
    pub state: AgentManagedInstallState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDeleteAgentAuthCatalogRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDeleteAgentAuthCatalogResponse {
    pub deleted: bool,
}

/// Updates an Agent's workspace membership, enabled flag, or overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUpdateConfigRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentUpdateConfigRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentUpdateConfigResponse {
    pub agent: AgentSnapshotEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentListRequest {
    pub auth: RemoteAuthProof,
    pub include_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentListResponse {
    pub agents: Vec<crate::AgentSnapshotEntry>,
}

/// Strips host-private Agent configuration before an entry is sent to a paired
/// device: environment values are replaced by empty strings (keys remain so a
/// client can show which variables exist), native config paths and diagnostics
/// are removed. Command lines stay because custom Agent editing needs them.
pub fn redact_agent_snapshot_for_remote(
    mut entry: crate::AgentSnapshotEntry,
) -> crate::AgentSnapshotEntry {
    for value in entry.env.values_mut() {
        value.clear();
    }
    entry.native_config_paths.clear();
    entry.diagnostics.clear();
    entry
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCustomAgentCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::CustomAgentCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCustomAgentCreateResponse {
    pub agent: crate::AgentSnapshotEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCustomAgentDeleteRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::CustomAgentDeleteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCustomAgentDeleteResponse {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelProviderProfileListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelProviderProfileListResponse {
    pub profiles: Vec<crate::ModelProviderProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelProviderProfileCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::ModelProviderProfileCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelProviderProfileUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::ModelProviderProfileUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelProviderProfileResponse {
    pub profile: crate::ModelProviderProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProfileListRequest {
    pub auth: RemoteAuthProof,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProfileListResponse {
    pub profiles: Vec<crate::AgentRuntimeProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProfileCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentRuntimeProfileCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProfileUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentRuntimeProfileUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeProfileResponse {
    pub profile: crate::AgentRuntimeProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderBindingListRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderBindingListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderBindingListResponse {
    pub bindings: Vec<crate::AgentModelProviderBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderBindingCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderBindingCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderBindingUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderBindingUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderBindingResponse {
    pub binding: crate::AgentModelProviderBinding,
}

/// Carries a Secret value to the authoritative runtime. The transport is
/// always E2EE or TLS, `Debug` never prints the value, and the runtime stores
/// it in its own secret store before answering with the redacted profile.
/// This is the only wire type that may contain a Secret value; it is never
/// persisted, logged, or echoed back.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderCredentialSecretMutationRequest {
    pub auth: RemoteAuthProof,
    pub model_provider_profile_id: crate::ModelProviderProfileId,
    pub credential_id: RequestId,
    pub touched: bool,
    pub clear: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl RemoteProviderCredentialSecretMutationRequest {
    pub fn from_request(
        auth: RemoteAuthProof,
        request: crate::ProviderCredentialSecretMutationRequest,
    ) -> Self {
        Self {
            auth,
            model_provider_profile_id: request.model_provider_profile_id,
            credential_id: request.credential_id,
            touched: request.touched,
            clear: request.clear,
            value: request.value,
        }
    }

    pub fn into_request(
        self,
    ) -> (
        RemoteAuthProof,
        crate::ProviderCredentialSecretMutationRequest,
    ) {
        (
            self.auth,
            crate::ProviderCredentialSecretMutationRequest {
                model_provider_profile_id: self.model_provider_profile_id,
                credential_id: self.credential_id,
                touched: self.touched,
                clear: self.clear,
                value: self.value,
            },
        )
    }
}

impl fmt::Debug for RemoteProviderCredentialSecretMutationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteProviderCredentialSecretMutationRequest")
            .field("auth", &self.auth)
            .field("model_provider_profile_id", &self.model_provider_profile_id)
            .field("credential_id", &self.credential_id)
            .field("touched", &self.touched)
            .field("clear", &self.clear)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderDefaultRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderSetDefaultRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillListResponse {
    pub skills: Vec<Skill>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillCreateResponse {
    pub skill: Skill,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillUpdateResponse {
    pub skill: Skill,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillDeleteRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillDeleteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillAgentMatrixSetRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillSetAgentMatrixRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillAgentMatrixSetResponse {
    pub skill: Skill,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillAgentMatrixListRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillAgentMatrixListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillAgentMatrixListResponse {
    pub matrix: Vec<SkillAgentMatrix>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillDiscoverRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillDiscoverRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillDiscoverResponse {
    pub discovery: SkillDiscoveryResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillImportRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillImportRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillImportResponse {
    pub result: SkillImportResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillValidateRequest {
    pub auth: RemoteAuthProof,
    pub request: SkillValidateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderSkillValidateResponse {
    pub result: SkillValidationResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptListResponse {
    pub prompts: Vec<Prompt>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: PromptCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptCreateResponse {
    pub prompt: Prompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: PromptUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptUpdateResponse {
    pub prompt: Prompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptDeleteRequest {
    pub auth: RemoteAuthProof,
    pub request: PromptDeleteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptValidateRequest {
    pub auth: RemoteAuthProof,
    pub request: PromptValidateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPromptValidateResponse {
    pub result: PromptValidationResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookListResponse {
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: HookCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookCreateResponse {
    pub hook: Hook,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: HookUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookUpdateResponse {
    pub hook: Hook,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookDeleteRequest {
    pub auth: RemoteAuthProof,
    pub request: HookDeleteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookPreviewInstallRequest {
    pub auth: RemoteAuthProof,
    pub request: HookInstallPreviewRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderHookPreviewInstallResponse {
    pub preview: HookInstallPreview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRefreshDetectedAgentVersionsRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRefreshDetectedAgentVersionsResponse {
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderListAgentCatalogRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderListAgentCatalogResponse {
    pub catalog: AgentCatalogListResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderListAcpCatalogPresetsRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderListAcpCatalogPresetsResponse {
    pub catalog: AcpProviderCatalogListResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderGetAcpProfileConfigRequest {
    pub auth: RemoteAuthProof,
    pub provider_profile_id: crate::ids::ProviderProfileId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderGetAcpProfileConfigResponse {
    pub config: AcpProviderConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderUpdateAcpProfileConfigRequest {
    pub auth: RemoteAuthProof,
    pub request: AcpProviderProfileUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderUpdateAcpProfileConfigResponse {
    pub profile: crate::provider::ProviderProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPreviewNativeImportRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderNativeImportPreviewRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPreviewNativeImportResponse {
    pub preview: ProviderNativeImportPreview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderCreateProfileFromImportRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderNativeImportCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderCreateProfileFromImportResponse {
    pub result: ProviderNativeImportCreateResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPreviewNativeExportRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderNativeExportPreviewRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderPreviewNativeExportResponse {
    pub preview: ProviderNativeExportPreview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderApplyNativeExportRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderNativeExportApplyRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderApplyNativeExportResponse {
    pub result: ProviderNativeExportApplyResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRollbackNativeExportRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderNativeExportRollbackRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRollbackNativeExportResponse {
    pub result: ProviderNativeExportRollbackResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderListNativeExportsRequest {
    pub auth: RemoteAuthProof,
    pub request: ProviderNativeExportListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderListNativeExportsResponse {
    pub exports: Vec<ProviderNativeExportRecordSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpListResponse {
    pub servers: Vec<McpServer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpDeleteRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerDeleteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpAgentMatrixSetRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerSetAgentMatrixRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpAgentMatrixListRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerAgentMatrixListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpAgentMatrixListResponse {
    pub matrix: Vec<McpServerAgentMatrix>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpDiscoverRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerDiscoverRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpDiscoverResponse {
    pub discovery: McpServerDiscoveryResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpImportRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerImportRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpImportResponse {
    pub result: McpServerImportResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpValidateRequest {
    pub auth: RemoteAuthProof,
    pub request: McpServerValidateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpValidateResponse {
    pub result: McpServerValidationResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpMutationResponse {
    pub server: McpServer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderMcpDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileSecretMutationRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentModelProviderProfileSecretValueUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileSecretMutationResponse {
    pub response: AgentModelProviderProfileSecretValueResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentModelProviderProfileCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileCreateResponse {
    pub profile: ProviderProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: AgentModelProviderProfileUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileUpdateResponse {
    pub profile: ProviderProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileDeleteRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderProfileDeleteRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderDisplayOrderGetRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderDisplayOrderListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderDisplayOrderSetRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderDisplayOrderSetRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileTestRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderProfileTestRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileFetchModelsRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::AgentModelProviderProfileFetchModelsRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderCapabilitySummaryListRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderCapabilitySummaryListResponse {
    pub summaries: Vec<ProviderCapabilitySummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRunCapabilityProbesRequest {
    pub auth: RemoteAuthProof,
    pub request: crate::ProviderRunCapabilityProbesRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderRunCapabilityProbesResponse {
    pub result: crate::ProviderRunCapabilityProbesResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderDisplayOrderGetResponse {
    pub order: crate::AgentModelProviderDisplayOrderListResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderDisplayOrderSetResponse {
    pub order: crate::AgentModelProviderDisplayOrderSetResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileTestResponse {
    pub result: crate::AgentModelProviderProfileTestResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderProfileFetchModelsResponse {
    pub response: crate::AgentModelProviderProfileFetchModelsResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentModelProviderDefaultResponse {
    pub selection: crate::AgentModelProviderDefaultSelection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteProviderRequest {
    ListAgentSummaries(RemoteAgentConfigSummaryListRequest),
    ListProfiles(RemoteProviderProfileListRequest),
    PreviewInjection(RemoteProviderInjectionPreviewRequest),
    ProjectionCapability(RemoteAgentProjectionCapabilityRequest),
    ProjectionPreview(RemoteAgentProjectionPreviewRequest),
    StartRuntimeProbe(RemoteAgentRuntimeProbeStartRequest),
    GetRuntimeProbe(RemoteAgentRuntimeProbeGetRequest),
    ListRuntimeProbes(RemoteAgentRuntimeProbeListRequest),
    CancelRuntimeProbe(RemoteAgentRuntimeProbeCancelRequest),
    ListHealthSummaries(RemoteProviderHealthSummaryListRequest),
    RunHealthProbes(RemoteProviderRunHealthProbesRequest),
    ListUsageSummaries(RemoteProviderUsageSummaryListRequest),
    ListFailoverRecommendations(RemoteProviderFailoverRecommendationListRequest),
    ListAgents(RemoteAgentListRequest),
    UpdateAgentConfig(RemoteAgentUpdateConfigRequest),
    RefreshAgentSnapshot(RemoteAgentRefreshAgentSnapshotRequest),
    ProbeAgentRuntimeOptions(RemoteAgentProbeRuntimeOptionsRequest),
    DiscoverOwnedModelCatalog(RemoteAgentOwnedModelCatalogRequest),
    InstallManagedAgent(RemoteAgentInstallManagedAgentRequest),
    CheckManagedAgentUpdate(RemoteAgentCheckManagedAgentUpdateRequest),
    UninstallManagedAgent(RemoteAgentUninstallManagedAgentRequest),
    DeleteAgentAuthCatalog(RemoteAgentDeleteAgentAuthCatalogRequest),
    CreateCustomAgent(RemoteCustomAgentCreateRequest),
    DeleteCustomAgent(RemoteCustomAgentDeleteRequest),
    ListModelProviderProfiles(RemoteModelProviderProfileListRequest),
    CreateModelProviderProfile(RemoteModelProviderProfileCreateRequest),
    UpdateModelProviderProfile(RemoteModelProviderProfileUpdateRequest),
    ListAgentRuntimeProfiles(RemoteAgentRuntimeProfileListRequest),
    CreateAgentRuntimeProfile(RemoteAgentRuntimeProfileCreateRequest),
    UpdateAgentRuntimeProfile(RemoteAgentRuntimeProfileUpdateRequest),
    ListAgentModelProviderBindings(RemoteAgentModelProviderBindingListRequest),
    CreateAgentModelProviderBinding(RemoteAgentModelProviderBindingCreateRequest),
    UpdateAgentModelProviderBinding(RemoteAgentModelProviderBindingUpdateRequest),
    MutateProviderCredentialSecret(RemoteProviderCredentialSecretMutationRequest),
    SetAgentModelProviderDefault(RemoteAgentModelProviderDefaultRequest),
    DeleteAgentModelProviderProfile(RemoteAgentModelProviderProfileDeleteRequest),
    CreateAgentModelProviderProfile(RemoteAgentModelProviderProfileCreateRequest),
    UpdateAgentModelProviderProfile(RemoteAgentModelProviderProfileUpdateRequest),
    MutateAgentModelProviderProfileSecret(RemoteAgentModelProviderProfileSecretMutationRequest),
    GetAgentModelProviderDisplayOrder(RemoteAgentModelProviderDisplayOrderGetRequest),
    SetAgentModelProviderDisplayOrder(RemoteAgentModelProviderDisplayOrderSetRequest),
    TestAgentModelProviderProfile(RemoteAgentModelProviderProfileTestRequest),
    FetchAgentModelProviderProfileModels(RemoteAgentModelProviderProfileFetchModelsRequest),
    ListCapabilitySummaries(RemoteProviderCapabilitySummaryListRequest),
    RunCapabilityProbes(RemoteProviderRunCapabilityProbesRequest),
    ListMcpServers(RemoteProviderMcpListRequest),
    CreateMcpServer(RemoteProviderMcpCreateRequest),
    UpdateMcpServer(RemoteProviderMcpUpdateRequest),
    DeleteMcpServer(RemoteProviderMcpDeleteRequest),
    SetMcpServerAgentMatrix(RemoteProviderMcpAgentMatrixSetRequest),
    ListMcpServerAgentMatrix(RemoteProviderMcpAgentMatrixListRequest),
    DiscoverMcpSources(RemoteProviderMcpDiscoverRequest),
    ImportMcpServers(RemoteProviderMcpImportRequest),
    ValidateMcpServer(RemoteProviderMcpValidateRequest),
    ManagementSnapshot(RemoteProviderManagementSnapshotRequest),
    RefreshDetectedAgentVersions(RemoteProviderRefreshDetectedAgentVersionsRequest),
    ListAgentCatalog(RemoteProviderListAgentCatalogRequest),
    ListAcpCatalogPresets(RemoteProviderListAcpCatalogPresetsRequest),
    GetAcpProfileConfig(RemoteProviderGetAcpProfileConfigRequest),
    UpdateAcpProfileConfig(RemoteProviderUpdateAcpProfileConfigRequest),
    PreviewNativeImport(RemoteProviderPreviewNativeImportRequest),
    CreateProfileFromImport(RemoteProviderCreateProfileFromImportRequest),
    PreviewNativeExport(RemoteProviderPreviewNativeExportRequest),
    ApplyNativeExport(RemoteProviderApplyNativeExportRequest),
    RollbackNativeExport(RemoteProviderRollbackNativeExportRequest),
    ListNativeExports(RemoteProviderListNativeExportsRequest),
    SkillList(RemoteProviderSkillListRequest),
    SkillCreate(RemoteProviderSkillCreateRequest),
    SkillUpdate(RemoteProviderSkillUpdateRequest),
    SkillDelete(RemoteProviderSkillDeleteRequest),
    SkillAgentMatrixSet(RemoteProviderSkillAgentMatrixSetRequest),
    SkillAgentMatrixList(RemoteProviderSkillAgentMatrixListRequest),
    SkillDiscover(RemoteProviderSkillDiscoverRequest),
    SkillImport(RemoteProviderSkillImportRequest),
    SkillValidate(RemoteProviderSkillValidateRequest),
    PromptList(RemoteProviderPromptListRequest),
    PromptCreate(RemoteProviderPromptCreateRequest),
    PromptUpdate(RemoteProviderPromptUpdateRequest),
    PromptDelete(RemoteProviderPromptDeleteRequest),
    PromptValidate(RemoteProviderPromptValidateRequest),
    HookList(RemoteProviderHookListRequest),
    HookCreate(RemoteProviderHookCreateRequest),
    HookUpdate(RemoteProviderHookUpdateRequest),
    HookDelete(RemoteProviderHookDeleteRequest),
    HookPreviewInstall(RemoteProviderHookPreviewInstallRequest),
}

impl RemoteProviderRequest {
    /// Whether the request changes Provider state. Mutations require
    /// `MutateProviderSettings`, an idempotency key, and an audit record.
    pub const fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::StartRuntimeProbe(_)
                | Self::CancelRuntimeProbe(_)
                | Self::RunHealthProbes(_)
                | Self::CreateCustomAgent(_)
                | Self::DeleteCustomAgent(_)
                | Self::CreateModelProviderProfile(_)
                | Self::UpdateModelProviderProfile(_)
                | Self::CreateAgentRuntimeProfile(_)
                | Self::UpdateAgentRuntimeProfile(_)
                | Self::CreateAgentModelProviderBinding(_)
                | Self::UpdateAgentModelProviderBinding(_)
                | Self::MutateProviderCredentialSecret(_)
                | Self::SetAgentModelProviderDefault(_)
                | Self::RefreshDetectedAgentVersions(_)
                | Self::UpdateAcpProfileConfig(_)
                | Self::CreateProfileFromImport(_)
                | Self::ApplyNativeExport(_)
                | Self::RollbackNativeExport(_)
                | Self::CreateAgentModelProviderProfile(_)
                | Self::UpdateAgentModelProviderProfile(_)
                | Self::MutateAgentModelProviderProfileSecret(_)
                | Self::UpdateAgentConfig(_)
                | Self::RefreshAgentSnapshot(_)
                | Self::ProbeAgentRuntimeOptions(_)
                | Self::InstallManagedAgent(_)
                | Self::CheckManagedAgentUpdate(_)
                | Self::UninstallManagedAgent(_)
                | Self::DeleteAgentAuthCatalog(_)
        )
    }

    pub const fn operation_kind(&self) -> RemoteProviderOperationKind {
        match self {
            Self::ListAgentSummaries(_) => RemoteProviderOperationKind::ListAgentSummaries,
            Self::ListProfiles(_) => RemoteProviderOperationKind::ListProfiles,
            Self::PreviewInjection(_) => RemoteProviderOperationKind::PreviewInjection,
            Self::ProjectionCapability(_) => RemoteProviderOperationKind::ProjectionCapability,
            Self::ProjectionPreview(_) => RemoteProviderOperationKind::ProjectionPreview,
            Self::StartRuntimeProbe(_) => RemoteProviderOperationKind::StartRuntimeProbe,
            Self::GetRuntimeProbe(_) => RemoteProviderOperationKind::GetRuntimeProbe,
            Self::ListRuntimeProbes(_) => RemoteProviderOperationKind::ListRuntimeProbes,
            Self::CancelRuntimeProbe(_) => RemoteProviderOperationKind::CancelRuntimeProbe,
            Self::ListHealthSummaries(_) => RemoteProviderOperationKind::ListHealthSummaries,
            Self::RunHealthProbes(_) => RemoteProviderOperationKind::RunHealthProbes,
            Self::ListUsageSummaries(_) => RemoteProviderOperationKind::ListUsageSummaries,
            Self::ListFailoverRecommendations(_) => {
                RemoteProviderOperationKind::ListFailoverRecommendations
            }
            Self::ListAgents(_) => RemoteProviderOperationKind::ListAgents,
            Self::UpdateAgentConfig(_) => RemoteProviderOperationKind::UpdateAgentConfig,
            Self::RefreshAgentSnapshot(_) => RemoteProviderOperationKind::RefreshAgentSnapshot,
            Self::ProbeAgentRuntimeOptions(_) => {
                RemoteProviderOperationKind::ProbeAgentRuntimeOptions
            }
            Self::DiscoverOwnedModelCatalog(_) => {
                RemoteProviderOperationKind::DiscoverOwnedModelCatalog
            }
            Self::InstallManagedAgent(_) => RemoteProviderOperationKind::InstallManagedAgent,
            Self::CheckManagedAgentUpdate(_) => {
                RemoteProviderOperationKind::CheckManagedAgentUpdate
            }
            Self::UninstallManagedAgent(_) => RemoteProviderOperationKind::UninstallManagedAgent,
            Self::DeleteAgentAuthCatalog(_) => RemoteProviderOperationKind::DeleteAgentAuthCatalog,
            Self::CreateCustomAgent(_) => RemoteProviderOperationKind::CreateCustomAgent,
            Self::DeleteCustomAgent(_) => RemoteProviderOperationKind::DeleteCustomAgent,
            Self::ListModelProviderProfiles(_) => {
                RemoteProviderOperationKind::ListModelProviderProfiles
            }
            Self::CreateModelProviderProfile(_) => {
                RemoteProviderOperationKind::CreateModelProviderProfile
            }
            Self::UpdateModelProviderProfile(_) => {
                RemoteProviderOperationKind::UpdateModelProviderProfile
            }
            Self::ListAgentRuntimeProfiles(_) => {
                RemoteProviderOperationKind::ListAgentRuntimeProfiles
            }
            Self::CreateAgentRuntimeProfile(_) => {
                RemoteProviderOperationKind::CreateAgentRuntimeProfile
            }
            Self::UpdateAgentRuntimeProfile(_) => {
                RemoteProviderOperationKind::UpdateAgentRuntimeProfile
            }
            Self::ListAgentModelProviderBindings(_) => {
                RemoteProviderOperationKind::ListAgentModelProviderBindings
            }
            Self::CreateAgentModelProviderBinding(_) => {
                RemoteProviderOperationKind::CreateAgentModelProviderBinding
            }
            Self::UpdateAgentModelProviderBinding(_) => {
                RemoteProviderOperationKind::UpdateAgentModelProviderBinding
            }
            Self::MutateProviderCredentialSecret(_) => {
                RemoteProviderOperationKind::MutateProviderCredentialSecret
            }
            Self::SetAgentModelProviderDefault(_) => {
                RemoteProviderOperationKind::SetAgentModelProviderDefault
            }
            Self::DeleteAgentModelProviderProfile(_) => {
                RemoteProviderOperationKind::DeleteAgentModelProviderProfile
            }
            Self::CreateAgentModelProviderProfile(_) => {
                RemoteProviderOperationKind::CreateAgentModelProviderProfile
            }
            Self::UpdateAgentModelProviderProfile(_) => {
                RemoteProviderOperationKind::UpdateAgentModelProviderProfile
            }
            Self::MutateAgentModelProviderProfileSecret(_) => {
                RemoteProviderOperationKind::MutateAgentModelProviderProfileSecret
            }
            Self::GetAgentModelProviderDisplayOrder(_) => {
                RemoteProviderOperationKind::GetAgentModelProviderDisplayOrder
            }
            Self::SetAgentModelProviderDisplayOrder(_) => {
                RemoteProviderOperationKind::SetAgentModelProviderDisplayOrder
            }
            Self::TestAgentModelProviderProfile(_) => {
                RemoteProviderOperationKind::TestAgentModelProviderProfile
            }
            Self::FetchAgentModelProviderProfileModels(_) => {
                RemoteProviderOperationKind::FetchAgentModelProviderProfileModels
            }
            Self::ListCapabilitySummaries(_) => {
                RemoteProviderOperationKind::ListCapabilitySummaries
            }
            Self::RunCapabilityProbes(_) => RemoteProviderOperationKind::RunCapabilityProbes,
            Self::ListMcpServers(_) => RemoteProviderOperationKind::ListMcpServers,
            Self::CreateMcpServer(_) => RemoteProviderOperationKind::CreateMcpServer,
            Self::UpdateMcpServer(_) => RemoteProviderOperationKind::UpdateMcpServer,
            Self::DeleteMcpServer(_) => RemoteProviderOperationKind::DeleteMcpServer,
            Self::SetMcpServerAgentMatrix(_) => {
                RemoteProviderOperationKind::SetMcpServerAgentMatrix
            }
            Self::ListMcpServerAgentMatrix(_) => {
                RemoteProviderOperationKind::ListMcpServerAgentMatrix
            }
            Self::DiscoverMcpSources(_) => RemoteProviderOperationKind::DiscoverMcpSources,
            Self::ImportMcpServers(_) => RemoteProviderOperationKind::ImportMcpServers,
            Self::ValidateMcpServer(_) => RemoteProviderOperationKind::ValidateMcpServer,
            Self::ManagementSnapshot(_) => RemoteProviderOperationKind::ManagementSnapshot,
            Self::RefreshDetectedAgentVersions(_) => {
                RemoteProviderOperationKind::RefreshDetectedAgentVersions
            }
            Self::ListAgentCatalog(_) => RemoteProviderOperationKind::ListAgentCatalog,
            Self::ListAcpCatalogPresets(_) => RemoteProviderOperationKind::ListAcpCatalogPresets,
            Self::GetAcpProfileConfig(_) => RemoteProviderOperationKind::GetAcpProfileConfig,
            Self::UpdateAcpProfileConfig(_) => RemoteProviderOperationKind::UpdateAcpProfileConfig,
            Self::PreviewNativeImport(_) => RemoteProviderOperationKind::PreviewNativeImport,
            Self::CreateProfileFromImport(_) => {
                RemoteProviderOperationKind::CreateProfileFromImport
            }
            Self::PreviewNativeExport(_) => RemoteProviderOperationKind::PreviewNativeExport,
            Self::ApplyNativeExport(_) => RemoteProviderOperationKind::ApplyNativeExport,
            Self::RollbackNativeExport(_) => RemoteProviderOperationKind::RollbackNativeExport,
            Self::ListNativeExports(_) => RemoteProviderOperationKind::ListNativeExports,
            Self::SkillList(_) => RemoteProviderOperationKind::SkillList,
            Self::SkillCreate(_) => RemoteProviderOperationKind::SkillCreate,
            Self::SkillUpdate(_) => RemoteProviderOperationKind::SkillUpdate,
            Self::SkillDelete(_) => RemoteProviderOperationKind::SkillDelete,
            Self::SkillAgentMatrixSet(_) => RemoteProviderOperationKind::SkillAgentMatrixSet,
            Self::SkillAgentMatrixList(_) => RemoteProviderOperationKind::SkillAgentMatrixList,
            Self::SkillDiscover(_) => RemoteProviderOperationKind::SkillDiscover,
            Self::SkillImport(_) => RemoteProviderOperationKind::SkillImport,
            Self::SkillValidate(_) => RemoteProviderOperationKind::SkillValidate,
            Self::PromptList(_) => RemoteProviderOperationKind::PromptList,
            Self::PromptCreate(_) => RemoteProviderOperationKind::PromptCreate,
            Self::PromptUpdate(_) => RemoteProviderOperationKind::PromptUpdate,
            Self::PromptDelete(_) => RemoteProviderOperationKind::PromptDelete,
            Self::PromptValidate(_) => RemoteProviderOperationKind::PromptValidate,
            Self::HookList(_) => RemoteProviderOperationKind::HookList,
            Self::HookCreate(_) => RemoteProviderOperationKind::HookCreate,
            Self::HookUpdate(_) => RemoteProviderOperationKind::HookUpdate,
            Self::HookDelete(_) => RemoteProviderOperationKind::HookDelete,
            Self::HookPreviewInstall(_) => RemoteProviderOperationKind::HookPreviewInstall,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHandshakeRequest {
    pub client_name: String,
    pub client_version: Option<String>,
    pub device_id: Option<DeviceId>,
    pub last_seen: Vec<RemoteCatchUpCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHandshakeResponse {
    pub protocol_version: RemoteProtocolVersion,
    pub server_name: String,
    pub server_version: String,
    pub capabilities: RemoteCapabilitySummary,
    pub server_time_ms: i64,
}

impl RemoteHandshakeResponse {
    pub fn foundation(server_name: impl Into<String>, server_version: impl Into<String>) -> Self {
        Self {
            protocol_version: RemoteProtocolVersion::foundation(),
            server_name: server_name.into(),
            server_version: server_version.into(),
            capabilities: RemoteCapabilitySummary::foundation(),
            server_time_ms: unix_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteEnvelopeStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOperationKind {
    Handshake,
    Health,
    Info,
    CatchUp,
    AgentSession,
    WorkspaceFile,
    Git,
    Terminal,
    ProviderSettings,
    DeviceManagement,
    ScheduledTasks,
    Automation,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAutomationOperationKind {
    ListGraphs,
    CreateGraph,
    UpdateGraph,
    ReplaceDefinition,
    SetStatus,
    ArchiveGraph,
    ListRuns,
    ListSteps,
    StartRun,
    ResumeRun,
    CancelRun,
}

/// Automation graph and run management requests.
///
/// The authority owns the graph store and the run engine, so the Management
/// Center's Automation section drives both through these requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteAutomationRequest {
    ListGraphs(RemoteAutomationGraphListRequest),
    CreateGraph(RemoteAutomationGraphCreateRequest),
    UpdateGraph(RemoteAutomationGraphUpdateRequest),
    ReplaceDefinition(RemoteAutomationDefinitionUpdateRequest),
    SetStatus(RemoteAutomationSetStatusRequest),
    ArchiveGraph(RemoteAutomationArchiveRequest),
    ListRuns(RemoteAutomationRunListRequest),
    ListSteps(RemoteAutomationStepListRequest),
    StartRun(RemoteAutomationRunStartRequest),
    ResumeRun(RemoteAutomationRunResumeRequest),
    CancelRun(RemoteAutomationRunCancelRequest),
}

impl RemoteAutomationRequest {
    pub const fn operation_kind(&self) -> RemoteAutomationOperationKind {
        match self {
            Self::ListGraphs(_) => RemoteAutomationOperationKind::ListGraphs,
            Self::CreateGraph(_) => RemoteAutomationOperationKind::CreateGraph,
            Self::UpdateGraph(_) => RemoteAutomationOperationKind::UpdateGraph,
            Self::ReplaceDefinition(_) => RemoteAutomationOperationKind::ReplaceDefinition,
            Self::SetStatus(_) => RemoteAutomationOperationKind::SetStatus,
            Self::ArchiveGraph(_) => RemoteAutomationOperationKind::ArchiveGraph,
            Self::ListRuns(_) => RemoteAutomationOperationKind::ListRuns,
            Self::ListSteps(_) => RemoteAutomationOperationKind::ListSteps,
            Self::StartRun(_) => RemoteAutomationOperationKind::StartRun,
            Self::ResumeRun(_) => RemoteAutomationOperationKind::ResumeRun,
            Self::CancelRun(_) => RemoteAutomationOperationKind::CancelRun,
        }
    }

    /// Whether the request mutates the authoritative automation store or
    /// advances a run.
    pub const fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::CreateGraph(_)
                | Self::UpdateGraph(_)
                | Self::ReplaceDefinition(_)
                | Self::SetStatus(_)
                | Self::ArchiveGraph(_)
                | Self::StartRun(_)
                | Self::ResumeRun(_)
                | Self::CancelRun(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationGraphListRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationGraphListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationGraphListResponse {
    pub graphs: Vec<AutomationGraph>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationGraphCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationGraphCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationGraphUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationGraphUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationDefinitionUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationGraphDefinitionUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationSetStatusRequest {
    pub auth: RemoteAuthProof,
    pub graph_id: crate::ids::AutomationGraphId,
    pub status: AutomationGraphStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationArchiveRequest {
    pub auth: RemoteAuthProof,
    pub graph_id: crate::ids::AutomationGraphId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationGraphResponse {
    pub graph: AutomationGraph,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationRunListRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationRunListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationRunListResponse {
    pub runs: Vec<AutomationRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationStepListRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationRunStepListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationStepListResponse {
    pub steps: Vec<AutomationRunStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationRunStartRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationRunStartRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationRunResumeRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationRunResumeRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationRunCancelRequest {
    pub auth: RemoteAuthProof,
    pub request: AutomationRunCancelRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAutomationRunResponse {
    pub run: AutomationRun,
}

/// One authoritative Config Center read bundle.
///
/// The desktop Config Center used to issue roughly two dozen sequential reads
/// against a local runtime. Over Remote v2 that would become two dozen round
/// trips, so the authority assembles the bundle once and the client derives its
/// projections from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderManagementSnapshotRequest {
    pub auth: RemoteAuthProof,
    #[serde(flatten)]
    pub payload: ManagementSnapshotPayload,
}

/// The Config Center snapshot request without an auth proof.
///
/// Backend adapters inject their own device proof, so the shared payload keeps
/// the two fields the loader actually chooses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementSnapshotPayload {
    pub default_scope: ProviderProfileDefaultScope,
    #[serde(default)]
    pub refresh_agent_versions: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderManagementSnapshotResponse {
    pub snapshot: RemoteProviderManagementSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProviderManagementSnapshot {
    pub agents: Vec<AgentSnapshotEntry>,
    pub catalog: AgentCatalogListResponse,
    pub profiles: Vec<ProviderProfile>,
    pub native_import_preview: Option<ProviderNativeImportPreview>,
    pub acp_configs: Vec<RemoteAcpProfileConfigEntry>,
    pub agent_details: Vec<RemoteManagementAgentDetail>,
    pub mcp_servers: Vec<McpServer>,
    pub skills: Vec<Skill>,
    pub prompts: Vec<Prompt>,
    pub hooks: Vec<Hook>,
    pub health_summaries: Vec<ProviderHealthSummary>,
    pub capability_summaries: Vec<ProviderCapabilitySummary>,
    pub usage_summaries: Vec<ProviderUsageSummary>,
    pub native_exports: Vec<ProviderNativeExportRecordSummary>,
    pub scheduled: Vec<ScheduledTask>,
    pub scheduled_runs: Vec<ScheduledTaskRun>,
    pub scheduled_attention: Vec<ScheduledTaskAttentionSummary>,
    pub scheduled_audit: Vec<ScheduledTaskAuditRecord>,
    pub automation_graphs: Vec<AutomationGraph>,
    pub automation_runs: Vec<AutomationRun>,
    pub automation_steps: Vec<AutomationRunStep>,
    pub devices: Vec<RemoteDeviceDetail>,
    pub audit_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAcpProfileConfigEntry {
    pub provider_profile_id: crate::ids::ProviderProfileId,
    pub config: AcpProviderConfig,
}

/// Per-Agent inputs the Config Center needs to build its provider projections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteManagementAgentDetail {
    pub agent_id: AgentId,
    pub profiles: Vec<AgentModelProviderProfile>,
    pub default_profile_id: Option<crate::ids::ProviderProfileId>,
    pub runtime_profiles: Vec<AgentRuntimeProfile>,
    pub bindings: Vec<AgentModelProviderBinding>,
    /// Capability and preview entries in the same order the desktop builds
    /// them locally, so the client derivation stays identical.
    pub projections: Vec<RemoteAgentProjectionEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentProjectionEntry {
    pub runtime_profile_id: crate::ids::AgentRuntimeProfileId,
    pub binding_id: Option<crate::ids::AgentModelProviderBindingId>,
    pub capability: AgentProviderProjectionCapability,
    pub preview: Option<AgentProviderProjectionPreview>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteScheduledOperationKind {
    List,
    Create,
    Update,
    SetStatus,
    Delete,
    ListRuns,
    ListAttention,
    ListAudit,
    ClaimDue,
}

/// Scheduled-task management requests.
///
/// The authority owns the task store, so the Management Center's Scheduled
/// section reads and mutates it through these requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteScheduledRequest {
    List(RemoteScheduledListRequest),
    Create(RemoteScheduledCreateRequest),
    Update(RemoteScheduledUpdateRequest),
    SetStatus(RemoteScheduledSetStatusRequest),
    Delete(RemoteScheduledDeleteRequest),
    ListRuns(RemoteScheduledRunListRequest),
    ListAttention(RemoteScheduledAttentionListRequest),
    ListAudit(RemoteScheduledAuditListRequest),
    ClaimDue(RemoteScheduledClaimDueRequest),
}

impl RemoteScheduledRequest {
    pub const fn operation_kind(&self) -> RemoteScheduledOperationKind {
        match self {
            Self::List(_) => RemoteScheduledOperationKind::List,
            Self::Create(_) => RemoteScheduledOperationKind::Create,
            Self::Update(_) => RemoteScheduledOperationKind::Update,
            Self::SetStatus(_) => RemoteScheduledOperationKind::SetStatus,
            Self::Delete(_) => RemoteScheduledOperationKind::Delete,
            Self::ListRuns(_) => RemoteScheduledOperationKind::ListRuns,
            Self::ListAttention(_) => RemoteScheduledOperationKind::ListAttention,
            Self::ListAudit(_) => RemoteScheduledOperationKind::ListAudit,
            Self::ClaimDue(_) => RemoteScheduledOperationKind::ClaimDue,
        }
    }

    /// Whether the request mutates the authoritative task store.
    pub const fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Create(_)
                | Self::Update(_)
                | Self::SetStatus(_)
                | Self::Delete(_)
                | Self::ClaimDue(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledListRequest {
    pub auth: RemoteAuthProof,
    pub request: ScheduledTaskListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledListResponse {
    pub tasks: Vec<ScheduledTask>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledCreateRequest {
    pub auth: RemoteAuthProof,
    pub request: ScheduledTaskCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledUpdateRequest {
    pub auth: RemoteAuthProof,
    pub request: ScheduledTaskUpdateRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledSetStatusRequest {
    pub auth: RemoteAuthProof,
    pub task_id: crate::ids::ScheduledTaskId,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledDeleteRequest {
    pub auth: RemoteAuthProof,
    pub task_id: crate::ids::ScheduledTaskId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledRunListRequest {
    pub auth: RemoteAuthProof,
    pub request: ScheduledTaskRunListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledRunListResponse {
    pub runs: Vec<ScheduledTaskRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledAttentionListRequest {
    pub auth: RemoteAuthProof,
    pub request: ScheduledTaskAttentionListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledAttentionListResponse {
    pub attention: Vec<ScheduledTaskAttentionSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledAuditListRequest {
    pub auth: RemoteAuthProof,
    pub request: ScheduledTaskAuditListRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledAuditListResponse {
    pub audit: Vec<ScheduledTaskAuditRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledClaimDueRequest {
    pub auth: RemoteAuthProof,
    pub task_id: crate::ids::ScheduledTaskId,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledTaskResponse {
    pub task: ScheduledTask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledClaimDueResponse {
    pub run: Option<ScheduledTaskRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteScheduledDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRequestEnvelope {
    pub protocol_version: RemoteProtocolVersion,
    pub request_id: RequestId,
    pub correlation_id: Option<CorrelationId>,
    pub device_id: Option<DeviceId>,
    pub operation: RemoteOperationKind,
    pub created_at_ms: i64,
    pub payload: Option<JsonValue>,
}

impl RemoteRequestEnvelope {
    pub fn new(operation: RemoteOperationKind) -> Self {
        Self {
            protocol_version: RemoteProtocolVersion::foundation(),
            request_id: RequestId::new(),
            correlation_id: None,
            device_id: None,
            operation,
            created_at_ms: unix_timestamp_ms(),
            payload: None,
        }
    }

    pub fn with_payload(mut self, payload: JsonValue) -> Self {
        self.payload = Some(payload);
        self
    }

    pub fn with_correlation_id(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteResponseEnvelope {
    pub protocol_version: RemoteProtocolVersion,
    pub request_id: RequestId,
    pub correlation_id: Option<CorrelationId>,
    pub status: RemoteEnvelopeStatus,
    pub payload: Option<JsonValue>,
    pub error: Option<VibexError>,
    pub completed_at_ms: i64,
}

impl RemoteResponseEnvelope {
    pub fn ok(
        request_id: RequestId,
        correlation_id: Option<CorrelationId>,
        payload: JsonValue,
    ) -> Self {
        Self {
            protocol_version: RemoteProtocolVersion::foundation(),
            request_id,
            correlation_id,
            status: RemoteEnvelopeStatus::Ok,
            payload: Some(payload),
            error: None,
            completed_at_ms: unix_timestamp_ms(),
        }
    }

    pub fn error(
        request_id: RequestId,
        correlation_id: Option<CorrelationId>,
        mut error: VibexError,
    ) -> Self {
        if let Some(correlation_id) = correlation_id.clone() {
            error = error.with_correlation_id(correlation_id);
        }

        Self {
            protocol_version: RemoteProtocolVersion::foundation(),
            request_id,
            correlation_id,
            status: RemoteEnvelopeStatus::Error,
            payload: None,
            error: Some(error),
            completed_at_ms: unix_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteLiveEventChannel {
    System,
    AgentSession,
    Terminal,
    Git,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteLiveEventEnvelope {
    pub protocol_version: RemoteProtocolVersion,
    pub event_id: EventId,
    pub correlation_id: Option<CorrelationId>,
    pub channel: RemoteLiveEventChannel,
    pub sequence: u64,
    pub payload: Option<JsonValue>,
    pub emitted_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCatchUpCursor {
    pub channel: RemoteLiveEventChannel,
    pub after_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCatchUpRequest {
    pub cursors: Vec<RemoteCatchUpCursor>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCatchUpResponse {
    pub events: Vec<RemoteLiveEventEnvelope>,
    pub next_cursors: Vec<RemoteCatchUpCursor>,
    pub compacted: bool,
}

// ---------------------------------------------------------------------------
// Sidebar organization
// ---------------------------------------------------------------------------
//
// The Desktop owns the sidebar tree — folders, nesting, ordering, and the
// collapsed/pinned flags. These are the wire mirrors of that state so a paired
// compact client can render the same shape the user arranged on the Desktop,
// and can move items without inventing a second source of truth.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSidebarItemKind {
    Folder,
    Project,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarItemRef {
    pub kind: RemoteSidebarItemKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// When present, the folder belongs to one workspace/worktree within the
    /// project. `None` keeps the legacy project-level folder behavior.
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub auto_archive_after_days: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarPlacement {
    pub item: RemoteSidebarItemRef,
    #[serde(default)]
    pub parent_folder_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarOrganizationSnapshot {
    /// Bumped by the Desktop on every accepted change so a client can tell a
    /// stale snapshot from a current one without diffing the whole tree.
    pub revision: u64,
    pub folders: Vec<RemoteSidebarFolder>,
    pub placements: Vec<RemoteSidebarPlacement>,
    #[serde(default)]
    pub collapsed_folder_ids: Vec<String>,
    #[serde(default)]
    pub collapsed_project_ids: Vec<String>,
    #[serde(default)]
    pub collapsed_workspace_ids: Vec<String>,
    #[serde(default)]
    pub pinned_session_ids: Vec<String>,
    #[serde(default)]
    pub session_order: Vec<String>,
    /// Wall-clock (ms) of the last manual session arrangement on the Desktop.
    /// Sessions in `session_order` active after this anchor sort by recency
    /// instead of their manual position. Older servers omit the field and
    /// clients must treat it as `0` (every entry sorts by recency).
    #[serde(default)]
    pub session_order_anchored_at_ms: i64,
    /// The persisted Desktop hierarchy selector. Older servers omit this and
    /// compact clients must keep using Compact as the compatibility default.
    #[serde(default)]
    pub hierarchy_mode: RemoteSidebarHierarchyMode,
    #[serde(default)]
    pub project_order: Vec<String>,
    #[serde(default)]
    pub workspace_order: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub project_appearances: std::collections::BTreeMap<String, RemoteSidebarProjectAppearance>,
    #[serde(default)]
    pub worktree_titles: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub project_new_session_locations:
        std::collections::BTreeMap<String, RemoteSidebarNewSessionLocation>,
    #[serde(default)]
    pub auto_continue_project_ids: Vec<String>,
    #[serde(default)]
    pub auto_continue_session_overrides: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    pub auto_continue_session_ids: Vec<String>,
    /// Sessions whose auto-continue is suspended by an explicit user action on
    /// the Desktop. Compacts clients only render the paused badge; the
    /// preference itself stays authoritative in `autoContinueSessionOverrides`.
    #[serde(default)]
    pub auto_continue_paused_session_ids: Vec<String>,
    #[serde(default)]
    pub unread_session_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSidebarHierarchyMode {
    Detailed,
    #[default]
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSidebarNewSessionLocation {
    NewWorktree,
    #[default]
    CurrentCheckout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarProjectAppearance {
    #[serde(default)]
    pub logo: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub custom_logo_file: Option<String>,
}

/// Where a move drops its payload relative to an anchor item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSidebarDropPosition {
    Before,
    After,
    Into,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteSidebarOrganizationMutation {
    /// Drag and drop: reparent and/or reorder `items` around `anchor`. A `None`
    /// anchor appends to the end of the scope root.
    MoveItems {
        items: Vec<RemoteSidebarItemRef>,
        #[serde(default)]
        anchor: Option<RemoteSidebarItemRef>,
        position: RemoteSidebarDropPosition,
        /// Scope the move to a project subtree; `None` is the sidebar root.
        #[serde(default)]
        project_id: Option<String>,
    },
    /// Drag and drop managed Worktree rows within one project's workspace list.
    /// Workspaces are not organization items, so their order travels separately
    /// from the folder/session placement tree.
    MoveWorkspaces {
        project_id: String,
        workspace_ids: Vec<String>,
        #[serde(default)]
        anchor_workspace_id: Option<String>,
        position: RemoteSidebarDropPosition,
    },
    CreateFolder {
        name: String,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        workspace_id: Option<String>,
        #[serde(default)]
        parent_folder_id: Option<String>,
    },
    RenameFolder {
        folder_id: String,
        name: String,
    },
    DeleteFolder {
        folder_id: String,
    },
    SetFolderCollapsed {
        folder_id: String,
        collapsed: bool,
    },
    SetProjectCollapsed {
        project_id: String,
        collapsed: bool,
    },
    SetWorkspaceCollapsed {
        workspace_id: String,
        collapsed: bool,
    },
    SetSessionPinned {
        session_id: String,
        pinned: bool,
    },
    SetSessionAutoContinue {
        session_id: String,
        enabled: bool,
    },
    SetWorktreeTitle {
        workspace_id: String,
        title: String,
    },
    SetHierarchyMode {
        mode: RemoteSidebarHierarchyMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarOrganizationRequest {
    pub auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarOrganizationMutateRequest {
    pub auth: RemoteAuthProof,
    pub mutation: RemoteSidebarOrganizationMutation,
    /// Snapshot revision the client rendered when the user acted. The Desktop
    /// rejects the mutation when it no longer matches, so a stale drag cannot
    /// reorder a tree the user has since changed elsewhere.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSidebarOrganizationResponse {
    pub snapshot: RemoteSidebarOrganizationSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteHealthState {
    Ok,
    Disabled,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHealthStatus {
    pub status: RemoteHealthState,
    pub protocol_version: RemoteProtocolVersion,
    pub service_name: String,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServiceInfo {
    pub service_name: String,
    pub server_version: String,
    pub protocol_version: RemoteProtocolVersion,
    pub capabilities: RemoteCapabilitySummary,
    pub remote_enabled: bool,
    pub bind_addr: String,
    pub public_listener_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentId, ProviderProfileId, RuntimeSelectionInteraction, RuntimeSwitchId,
        SessionRuntimeSelection, WorkspaceMode,
    };

    #[test]
    fn request_envelope_round_trips_payload_and_correlation() {
        let correlation_id = CorrelationId::new();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::Handshake)
            .with_correlation_id(correlation_id.clone())
            .with_payload(serde_json::json!({"clientName": "web"}));

        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: RemoteRequestEnvelope = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.operation, RemoteOperationKind::Handshake);
        assert_eq!(decoded.correlation_id, Some(correlation_id));
        assert_eq!(decoded.payload.unwrap()["clientName"], "web");
    }

    #[test]
    fn response_envelope_preserves_correlation_in_error() {
        let request_id = RequestId::new();
        let correlation_id = CorrelationId::new();
        let response = RemoteResponseEnvelope::error(
            request_id.clone(),
            Some(correlation_id.clone()),
            VibexError::capability("remote_unsupported_operation", "operation is not supported"),
        );

        let error = response.error.unwrap();
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.status, RemoteEnvelopeStatus::Error);
        assert_eq!(error.code, "remote_unsupported_operation");
        assert_eq!(error.correlation_id, Some(correlation_id));
    }

    #[test]
    fn legacy_capabilities_default_new_runtime_features_to_unsupported() {
        let capabilities: RemoteCapabilitySummary = serde_json::from_value(serde_json::json!({
            "protocolVersion": { "major": 0, "minor": 4 },
            "supportsPairing": true,
            "supportsAuth": true,
            "supportsCatchUp": true,
            "supportsAgentSessions": true,
            "supportsWorkspaceFiles": false,
            "supportsGit": false,
            "supportsTerminal": false,
            "supportsProviderSettings": false,
            "liveEventChannels": ["system", "agent_session"]
        }))
        .unwrap();

        assert!(!capabilities.supports_runtime_lifecycle);
        assert!(!capabilities.supports_seamless_runtime_selection);
    }

    #[test]
    fn device_and_audit_contracts_serialize_with_stable_variants() {
        let device = RemoteDeviceDetail {
            device_id: DeviceId::new(),
            display_name: "Phone".to_string(),
            public_key: Some("public-key".to_string()),
            grant_revision: 1,
            permission_level: RemoteDevicePermissionLevel::ApproveOnly,
            status: RemoteDeviceStatus::Active,
            paired_at_ms: Some(1),
            last_seen_at_ms: Some(2),
            revoked_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let audit = RemoteAuditRecord {
            audit_id: RequestId::new(),
            device_id: Some(device.device_id.clone()),
            action: RemoteAuditAction::PermissionDenied,
            target_kind: RemoteAuditTargetKind::Git,
            target_id: Some("git_status".to_string()),
            outcome: RemoteAuditOutcome::Denied,
            redacted_summary: "Denied Git mutation".to_string(),
            request_id: None,
            correlation_id: None,
            created_at_ms: 3,
        };

        let device_json = serde_json::to_value(&device).unwrap();
        let audit_json = serde_json::to_value(&audit).unwrap();

        assert_eq!(device_json["permissionLevel"], "approve_only");
        assert_eq!(device_json["status"], "active");
        assert_eq!(audit_json["action"], "permission_denied");
        assert_eq!(audit_json["targetKind"], "git");
    }

    #[test]
    fn remote_agent_request_serializes_with_stable_tag_and_auth_boundary() {
        let session_id = VibexSessionId::new();
        let request = RemoteAgentRequest::FetchTimeline(RemoteAgentTimelineFetchRequest {
            auth: RemoteAuthProof {
                device_id: DeviceId::new(),
                auth_token: "auth-token-returned-once".to_string(),
            },
            request: FetchTimelineRequest {
                session_id,
                after_sequence: Some(3),
                limit: 20,
            },
        });

        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(
            request.operation_kind(),
            RemoteAgentOperationKind::FetchTimeline
        );
        assert_eq!(json["type"], "fetch_timeline");
        assert_eq!(json["data"]["request"]["afterSequence"], 3);
        assert_eq!(
            json["data"]["auth"]["authToken"],
            "auth-token-returned-once"
        );
        assert!(!format!("{request:?}").contains("auth-token-returned-once"));
    }

    #[test]
    fn remote_agent_session_mutations_use_stable_tags_and_operation_kinds() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "session-mutation-token".to_string(),
        };
        let session_id = VibexSessionId::new();
        let requests = [
            (
                RemoteAgentRequest::CreateSession(RemoteAgentCreateSessionRequest {
                    auth: auth.clone(),
                    request: CreateAgentSessionRequest {
                        session_id: None,
                        defer_runtime_materialization: false,
                        runtime: SessionRuntimeSelection::provider(
                            AgentId::parse("codex").unwrap(),
                            ProviderProfileId::new(),
                            "gpt-5",
                        ),
                        workspace_root: "/published/workspace".to_string(),
                        workspace_mode: WorkspaceMode::CurrentCheckout,
                        title: Some("Mobile session".to_string()),
                        safety: None,
                    },
                }),
                RemoteAgentOperationKind::CreateSession,
                "create_session",
            ),
            (
                RemoteAgentRequest::RenameSession(RemoteAgentRenameSessionRequest {
                    auth: auth.clone(),
                    request: RenameAgentSessionRequest {
                        session_id: session_id.clone(),
                        title: "Renamed".to_string(),
                    },
                }),
                RemoteAgentOperationKind::RenameSession,
                "rename_session",
            ),
            (
                RemoteAgentRequest::ArchiveSession(RemoteAgentSessionActionRequest {
                    auth: auth.clone(),
                    session_id: session_id.clone(),
                }),
                RemoteAgentOperationKind::ArchiveSession,
                "archive_session",
            ),
            (
                RemoteAgentRequest::DeleteSession(RemoteAgentSessionActionRequest {
                    auth,
                    session_id,
                }),
                RemoteAgentOperationKind::DeleteSession,
                "delete_session",
            ),
        ];

        for (request, operation, tag) in requests {
            assert_eq!(request.operation_kind(), operation);
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["type"], tag);
            let decoded: RemoteAgentRequest = serde_json::from_value(value).unwrap();
            assert_eq!(decoded, request);
            assert!(!format!("{request:?}").contains("session-mutation-token"));
        }
    }

    #[test]
    fn timeline_settings_and_fork_requests_use_stable_tags() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "timeline-settings-token".to_string(),
        };
        let fork = RemoteAgentRequest::ForkSession(RemoteAgentForkSessionRequest {
            auth: auth.clone(),
            request: ForkAgentSessionRequest {
                source_session_id: VibexSessionId::new(),
                through_sequence: 12,
                expected_source_end_sequence: Some(12),
            },
        });
        let settings = RemoteAgentRequest::GetTimelineDisplaySettings(
            RemoteAgentTimelineDisplaySettingsRequest { auth },
        );

        let fork_json = serde_json::to_value(&fork).unwrap();
        assert_eq!(fork.operation_kind(), RemoteAgentOperationKind::ForkSession);
        assert_eq!(fork_json["type"], "fork_session");
        assert_eq!(fork_json["data"]["request"]["throughSequence"], 12);
        assert!(!format!("{fork:?}").contains("timeline-settings-token"));

        let settings_json = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            settings.operation_kind(),
            RemoteAgentOperationKind::GetTimelineDisplaySettings
        );
        assert_eq!(settings_json["type"], "get_timeline_display_settings");
        assert!(!format!("{settings:?}").contains("timeline-settings-token"));
    }

    #[test]
    fn remote_authentication_operation_query_uses_canonical_contract() {
        let operation_id = crate::AgentAuthenticationOperationId::new();
        let request = RemoteAgentRequest::GetAuthenticationOperation(
            RemoteAgentAuthenticationOperationRequest {
                auth: RemoteAuthProof {
                    device_id: DeviceId::new(),
                    auth_token: "authentication-operation-token".to_string(),
                },
                operation_id: operation_id.clone(),
            },
        );

        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(
            request.operation_kind(),
            RemoteAgentOperationKind::GetAuthenticationOperation
        );
        assert_eq!(json["type"], "get_authentication_operation");
        assert_eq!(
            json["data"]["operationId"],
            serde_json::Value::String(operation_id.as_str().to_string())
        );
        assert!(!format!("{request:?}").contains("authentication-operation-token"));
    }

    #[test]
    fn opaque_deep_link_request_keeps_locator_inside_the_authenticated_agent_rpc() {
        let session_id = VibexSessionId::new();
        let request = RemoteAgentRequest::ResolveOpaqueLocator(RemoteAgentDeepLinkResolveRequest {
            auth: RemoteAuthProof {
                device_id: DeviceId::new(),
                auth_token: "auth-token-returned-once".to_string(),
            },
            notification_id: "notification-a".to_string(),
            opaque_locator: session_id.as_str().to_string(),
        });

        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(
            request.operation_kind(),
            RemoteAgentOperationKind::ResolveOpaqueLocator
        );
        assert_eq!(json["type"], "resolve_opaque_locator");
        assert_eq!(json["data"]["opaqueLocator"], session_id.as_str());
        assert!(!format!("{request:?}").contains("auth-token-returned-once"));
    }

    #[test]
    fn remote_agent_runtime_and_submission_queries_use_canonical_contracts() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "auth-token-returned-once".to_string(),
        };
        let session_id = VibexSessionId::new();
        let runtime = RemoteAgentRequest::GetRuntimeSelection(RemoteAgentRuntimeSelectionRequest {
            auth: auth.clone(),
            session_id: session_id.clone(),
        });
        let submission =
            RemoteAgentRequest::GetMessageSubmission(RemoteAgentMessageSubmissionRequest {
                auth,
                request: GetMessageSubmissionRequest {
                    session_id,
                    message_idempotency_key: "message-1".to_string(),
                },
            });

        assert_eq!(
            runtime.operation_kind(),
            RemoteAgentOperationKind::GetRuntimeSelection
        );
        assert_eq!(
            submission.operation_kind(),
            RemoteAgentOperationKind::GetMessageSubmission
        );
        assert_eq!(
            serde_json::to_value(runtime).unwrap()["type"],
            "get_runtime_selection"
        );
        let submission_json = serde_json::to_value(submission).unwrap();
        assert_eq!(submission_json["type"], "get_message_submission");
        assert_eq!(
            submission_json["data"]["request"]["messageIdempotencyKey"],
            "message-1"
        );
    }

    #[test]
    fn remote_agent_seamless_runtime_requests_use_canonical_contracts() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "auth-token-returned-once".to_string(),
        };
        let session_id = VibexSessionId::new();
        let desired = SessionRuntimeSelection {
            reasoning_effort: Some("high".to_string()),
            mode_id: Some("plan".to_string()),
            ..SessionRuntimeSelection::provider(
                AgentId::parse("codex").unwrap(),
                ProviderProfileId::new(),
                "model-a",
            )
        };
        let catalog = RemoteAgentRequest::ListRuntimeOptions(RemoteAgentRuntimeOptionsRequest {
            auth: auth.clone(),
            supports_agent_account_auth: true,
        });
        let set_desired =
            RemoteAgentRequest::SetDesiredRuntime(RemoteAgentSetDesiredRuntimeRequest {
                auth: auth.clone(),
                request: SetDesiredAgentSessionRuntimeRequest {
                    session_id: session_id.clone(),
                    idempotency_key: "runtime-selection-1".to_string(),
                    expected_revision: 7,
                    expected_selection_revision: 3,
                    desired,
                    interaction: RuntimeSelectionInteraction::Seamless,
                },
            });
        let switch_id = RuntimeSwitchId::new();
        let cancel =
            RemoteAgentRequest::CancelRuntimeSwitch(RemoteAgentCancelRuntimeSwitchRequest {
                auth,
                request: CancelAgentSessionRuntimeSwitchRequest {
                    session_id,
                    switch_id: switch_id.clone(),
                },
            });

        assert_eq!(
            catalog.operation_kind(),
            RemoteAgentOperationKind::ListRuntimeOptions
        );
        assert_eq!(
            set_desired.operation_kind(),
            RemoteAgentOperationKind::SetDesiredRuntime
        );
        assert_eq!(
            cancel.operation_kind(),
            RemoteAgentOperationKind::CancelRuntimeSwitch
        );
        assert_eq!(
            serde_json::to_value(catalog).unwrap()["type"],
            "list_runtime_options"
        );
        let set_desired_json = serde_json::to_value(set_desired).unwrap();
        assert_eq!(set_desired_json["type"], "set_desired_runtime");
        assert_eq!(
            set_desired_json["data"]["request"]["expectedSelectionRevision"],
            3
        );
        assert_eq!(
            set_desired_json["data"]["request"]["desired"]["modelId"],
            "model-a"
        );
        let cancel_json = serde_json::to_value(cancel).unwrap();
        assert_eq!(cancel_json["type"], "cancel_runtime_switch");
        assert_eq!(
            cancel_json["data"]["request"]["switchId"],
            switch_id.as_str()
        );
    }

    #[test]
    fn remote_workbench_request_serializes_with_stable_tag_and_auth_boundary() {
        let workspace_id =
            crate::ids::WorkspaceId::parse("workspace_00000000000000000000000000000000").unwrap();
        let request = RemoteWorkbenchRequest::FileRead(RemoteFileReadRequest {
            auth: RemoteAuthProof {
                device_id: DeviceId::new(),
                auth_token: "auth-token-returned-once".to_string(),
            },
            request: FileReadRequest {
                workspace_id,
                path: "src/lib.rs".to_string(),
                max_bytes: Some(4096),
            },
        });

        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(
            request.operation_kind(),
            RemoteWorkbenchOperationKind::FileRead
        );
        assert_eq!(json["type"], "file_read");
        assert_eq!(json["data"]["request"]["path"], "src/lib.rs");
        assert_eq!(
            json["data"]["auth"]["authToken"],
            "auth-token-returned-once"
        );
    }

    #[test]
    fn remote_workbench_delete_workspace_serializes_with_stable_tag() {
        let workspace_id = crate::ids::WorkspaceId::new();
        let request =
            RemoteWorkbenchRequest::DeleteWorkspace(RemoteWorkbenchDeleteWorkspaceRequest {
                auth: RemoteAuthProof {
                    device_id: DeviceId::new(),
                    auth_token: "workspace-delete-token".to_string(),
                },
                workspace_id: workspace_id.clone(),
            });

        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(
            request.operation_kind(),
            RemoteWorkbenchOperationKind::DeleteWorkspace
        );
        assert_eq!(json["type"], "delete_workspace");
        assert_eq!(json["data"]["workspaceId"], workspace_id.as_str());
    }

    #[test]
    fn remote_worktree_reads_have_distinct_stable_operation_tags() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "auth-token-returned-once".to_string(),
        };
        let workspace_id = crate::ids::WorkspaceId::new();
        let eligibility =
            RemoteWorkbenchRequest::GitWorktreeEligibility(RemoteGitWorktreeEligibilityRequest {
                auth: auth.clone(),
                workspace_id: workspace_id.clone(),
            });
        let snapshot =
            RemoteWorkbenchRequest::GitWorktreeSnapshot(RemoteGitWorktreeSnapshotRequest {
                auth,
                workspace_id,
            });

        assert_eq!(
            eligibility.operation_kind(),
            RemoteWorkbenchOperationKind::GitWorktreeEligibility
        );
        assert_eq!(
            snapshot.operation_kind(),
            RemoteWorkbenchOperationKind::GitWorktreeSnapshot
        );
        assert_eq!(
            serde_json::to_value(&eligibility).unwrap()["type"],
            "git_worktree_eligibility"
        );
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap()["type"],
            "git_worktree_snapshot"
        );
        assert!(!format!("{eligibility:?}{snapshot:?}").contains("auth-token-returned-once"));
    }

    #[test]
    fn remote_provider_request_serializes_with_stable_tag_and_auth_boundary() {
        let request =
            RemoteProviderRequest::RunHealthProbes(RemoteProviderRunHealthProbesRequest {
                auth: RemoteAuthProof {
                    device_id: DeviceId::new(),
                    auth_token: "auth-token-returned-once".to_string(),
                },
                request: ProviderRunHealthProbesRequest {
                    provider_profile_ids: None,
                    probe_kinds: None,
                },
            });

        let json = serde_json::to_value(&request).unwrap();

        assert_eq!(
            request.operation_kind(),
            RemoteProviderOperationKind::RunHealthProbes
        );
        assert_eq!(json["type"], "run_health_probes");
        assert_eq!(
            json["data"]["auth"]["authToken"],
            "auth-token-returned-once"
        );
        assert!(json["data"]["request"]["providerProfileIds"].is_null());
    }

    #[test]
    fn remote_agent_config_summary_excludes_private_agent_fields() {
        let request =
            RemoteProviderRequest::ListAgentSummaries(RemoteAgentConfigSummaryListRequest {
                auth: RemoteAuthProof {
                    device_id: DeviceId::new(),
                    auth_token: "agent-summary-auth-token".to_string(),
                },
                include_disabled: true,
            });
        assert_eq!(
            request.operation_kind(),
            RemoteProviderOperationKind::ListAgentSummaries
        );
        assert_eq!(
            serde_json::to_value(&request).unwrap()["type"],
            "list_agent_summaries"
        );

        let response = RemoteAgentConfigSummaryListResponse {
            agents: vec![RemoteAgentConfigSummary {
                id: AgentId::parse("codex").unwrap(),
                label: "Codex".to_string(),
                enabled: true,
                installed: true,
                configured: true,
                config_status: AgentConfigStatus::Configured,
                runtime_status: AgentRuntimeStatus::Ready,
                model_count: 3,
                updated_at_ms: Some(7),
            }],
        };
        let json = serde_json::to_value(response).unwrap();
        let agent = &json["agents"][0];
        for private_field in [
            "command",
            "env",
            "params",
            "nativeConfigPaths",
            "diagnostics",
        ] {
            assert!(agent.get(private_field).is_none());
        }
    }

    #[test]
    fn remote_runtime_probe_requests_use_stable_tags_and_canonical_contracts() {
        let auth = RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "runtime-probe-auth-token".to_string(),
        };
        let probe_id = AgentRuntimeProbeId::new();
        let requests = [
            (
                RemoteProviderRequest::StartRuntimeProbe(RemoteAgentRuntimeProbeStartRequest {
                    auth: auth.clone(),
                    request: AgentRuntimeProbeStartRequest {
                        runtime_profile_id: crate::AgentRuntimeProfileId::new(),
                        binding_id: None,
                        workspace_key: "remote-probe-workspace".to_string(),
                        timeout_ms: crate::MIN_PROBE_TIMEOUT_MS,
                        minimal_prompt: false,
                    },
                }),
                RemoteProviderOperationKind::StartRuntimeProbe,
                "start_runtime_probe",
            ),
            (
                RemoteProviderRequest::GetRuntimeProbe(RemoteAgentRuntimeProbeGetRequest {
                    auth: auth.clone(),
                    probe_id: probe_id.clone(),
                }),
                RemoteProviderOperationKind::GetRuntimeProbe,
                "get_runtime_probe",
            ),
            (
                RemoteProviderRequest::ListRuntimeProbes(RemoteAgentRuntimeProbeListRequest {
                    auth: auth.clone(),
                    request: AgentRuntimeProbeListRequest::default(),
                }),
                RemoteProviderOperationKind::ListRuntimeProbes,
                "list_runtime_probes",
            ),
            (
                RemoteProviderRequest::CancelRuntimeProbe(RemoteAgentRuntimeProbeCancelRequest {
                    auth,
                    request: AgentRuntimeProbeCancelRequest {
                        probe_id,
                        expected_revision: 7,
                    },
                }),
                RemoteProviderOperationKind::CancelRuntimeProbe,
                "cancel_runtime_probe",
            ),
        ];

        for (request, operation, tag) in requests {
            assert_eq!(request.operation_kind(), operation);
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["type"], tag);
            let decoded: RemoteProviderRequest = serde_json::from_value(value).unwrap();
            assert_eq!(decoded, request);
            assert!(!format!("{request:?}").contains("runtime-probe-auth-token"));
        }
    }
}

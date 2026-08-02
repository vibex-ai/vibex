use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::agent::{
    AgentSession, AgentSessionSummary, ContinueAgentTurnRequest, FetchTimelineRequest,
    GetMessageSubmissionRequest, MessageSubmissionState, ResolvePermissionRequest,
    SendAgentMessageRequest,
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
    CorrelationId, DeviceId, EventId, RequestId, RuntimeProcessId, TerminalId, VibexSessionId,
};
use crate::provider::{
    ProviderFailoverRecommendation, ProviderFailoverRecommendationRequest, ProviderHealthSummary,
    ProviderInjectionPreview, ProviderInjectionPreviewRequest, ProviderProfileSummary,
    ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult, ProviderUsageListRequest,
    ProviderUsageSummary,
};
use crate::runtime::{
    AgentSessionRuntimeSelectionState, AgentSessionRuntimeSnapshot, AttachRuntimeRequest,
    AttachRuntimeResponse, CancelAgentSessionRuntimeSwitchRequest, DetachRuntimeRequest,
    DetachRuntimeResponse, GetRuntimeEventsRequest, RuntimeEventBatch, RuntimeProcessSnapshot,
    SessionRuntimeOptionCatalog, SetDesiredAgentSessionRuntimeRequest,
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
    pub supports_workspace_files: bool,
    pub supports_git: bool,
    #[serde(default)]
    pub supports_worktree_read: bool,
    pub supports_terminal: bool,
    pub supports_provider_settings: bool,
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
            supports_workspace_files: false,
            supports_git: false,
            supports_worktree_read: false,
            supports_terminal: false,
            supports_provider_settings: false,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreatePairingCodeResponse {
    pub pairing: RemotePairingCode,
    pub pairing_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaimPairingCodeRequest {
    pub pairing_code: String,
    pub display_name: String,
    pub public_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteClaimPairingCodeResponse {
    pub device: RemoteDeviceDetail,
    pub auth_token: String,
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
    MutateAgentSession,
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
    AgentSession,
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
    FetchTimeline,
    ResolveOpaqueLocator,
    ListRuntimeOptions,
    GetRuntimeSelection,
    SetDesiredRuntime,
    CancelRuntimeSwitch,
    GetMessageSubmission,
    SendMessage,
    ContinueTurn,
    Interrupt,
    ResolvePermission,
    CatchUp,
    GetRuntimeSnapshot,
    GetRuntimeProcessSnapshot,
    GetRuntimeEvents,
    AttachRuntime,
    DetachRuntime,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentRuntimeOptionsResponse {
    pub catalog: SessionRuntimeOptionCatalog,
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
    FetchTimeline(RemoteAgentTimelineFetchRequest),
    ResolveOpaqueLocator(RemoteAgentDeepLinkResolveRequest),
    ListRuntimeOptions(RemoteAgentRuntimeOptionsRequest),
    GetRuntimeSelection(RemoteAgentRuntimeSelectionRequest),
    SetDesiredRuntime(RemoteAgentSetDesiredRuntimeRequest),
    CancelRuntimeSwitch(RemoteAgentCancelRuntimeSwitchRequest),
    GetMessageSubmission(RemoteAgentMessageSubmissionRequest),
    SendMessage(RemoteAgentSendMessageRequest),
    ContinueTurn(RemoteAgentContinueTurnRequest),
    Interrupt(RemoteAgentInterruptRequest),
    ResolvePermission(RemoteAgentResolvePermissionRequest),
    CatchUp(RemoteAgentCatchUpRequest),
    GetRuntimeSnapshot(RemoteAgentRuntimeSnapshotRequest),
    GetRuntimeProcessSnapshot(RemoteAgentRuntimeProcessSnapshotRequest),
    GetRuntimeEvents(RemoteAgentRuntimeEventsRequest),
    AttachRuntime(RemoteAgentAttachRuntimeRequest),
    DetachRuntime(RemoteAgentDetachRuntimeRequest),
}

impl RemoteAgentRequest {
    pub const fn operation_kind(&self) -> RemoteAgentOperationKind {
        match self {
            Self::ListSessions(_) => RemoteAgentOperationKind::ListSessions,
            Self::GetSession(_) => RemoteAgentOperationKind::GetSession,
            Self::FetchTimeline(_) => RemoteAgentOperationKind::FetchTimeline,
            Self::ResolveOpaqueLocator(_) => RemoteAgentOperationKind::ResolveOpaqueLocator,
            Self::ListRuntimeOptions(_) => RemoteAgentOperationKind::ListRuntimeOptions,
            Self::GetRuntimeSelection(_) => RemoteAgentOperationKind::GetRuntimeSelection,
            Self::SetDesiredRuntime(_) => RemoteAgentOperationKind::SetDesiredRuntime,
            Self::CancelRuntimeSwitch(_) => RemoteAgentOperationKind::CancelRuntimeSwitch,
            Self::GetMessageSubmission(_) => RemoteAgentOperationKind::GetMessageSubmission,
            Self::SendMessage(_) => RemoteAgentOperationKind::SendMessage,
            Self::ContinueTurn(_) => RemoteAgentOperationKind::ContinueTurn,
            Self::Interrupt(_) => RemoteAgentOperationKind::Interrupt,
            Self::ResolvePermission(_) => RemoteAgentOperationKind::ResolvePermission,
            Self::CatchUp(_) => RemoteAgentOperationKind::CatchUp,
            Self::GetRuntimeSnapshot(_) => RemoteAgentOperationKind::GetRuntimeSnapshot,
            Self::GetRuntimeProcessSnapshot(_) => {
                RemoteAgentOperationKind::GetRuntimeProcessSnapshot
            }
            Self::GetRuntimeEvents(_) => RemoteAgentOperationKind::GetRuntimeEvents,
            Self::AttachRuntime(_) => RemoteAgentOperationKind::AttachRuntime,
            Self::DetachRuntime(_) => RemoteAgentOperationKind::DetachRuntime,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteWorkbenchOperationKind {
    ListWorkspaces,
    OpenWorkspace,
    FileListTree,
    FileRead,
    FileSearch,
    FileWrite,
    FileDelete,
    FileRename,
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
    FileListTree(RemoteFileTreeRequest),
    FileRead(RemoteFileReadRequest),
    FileSearch(RemoteFileSearchRequest),
    FileWrite(RemoteFileWriteRequest),
    FileDelete(RemoteFileMutationRequest),
    FileRename(RemoteFileMutationRequest),
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
            Self::FileListTree(_) => RemoteWorkbenchOperationKind::FileListTree,
            Self::FileRead(_) => RemoteWorkbenchOperationKind::FileRead,
            Self::FileSearch(_) => RemoteWorkbenchOperationKind::FileSearch,
            Self::FileWrite(_) => RemoteWorkbenchOperationKind::FileWrite,
            Self::FileDelete(_) => RemoteWorkbenchOperationKind::FileDelete,
            Self::FileRename(_) => RemoteWorkbenchOperationKind::FileRename,
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
    ListProfiles,
    PreviewInjection,
    ListHealthSummaries,
    RunHealthProbes,
    ListUsageSummaries,
    ListFailoverRecommendations,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RemoteProviderRequest {
    ListProfiles(RemoteProviderProfileListRequest),
    PreviewInjection(RemoteProviderInjectionPreviewRequest),
    ListHealthSummaries(RemoteProviderHealthSummaryListRequest),
    RunHealthProbes(RemoteProviderRunHealthProbesRequest),
    ListUsageSummaries(RemoteProviderUsageSummaryListRequest),
    ListFailoverRecommendations(RemoteProviderFailoverRecommendationListRequest),
}

impl RemoteProviderRequest {
    pub const fn operation_kind(&self) -> RemoteProviderOperationKind {
        match self {
            Self::ListProfiles(_) => RemoteProviderOperationKind::ListProfiles,
            Self::PreviewInjection(_) => RemoteProviderOperationKind::PreviewInjection,
            Self::ListHealthSummaries(_) => RemoteProviderOperationKind::ListHealthSummaries,
            Self::RunHealthProbes(_) => RemoteProviderOperationKind::RunHealthProbes,
            Self::ListUsageSummaries(_) => RemoteProviderOperationKind::ListUsageSummaries,
            Self::ListFailoverRecommendations(_) => {
                RemoteProviderOperationKind::ListFailoverRecommendations
            }
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
    Unsupported,
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
        SessionRuntimeSelection,
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
            agent_id: AgentId::parse("codex").unwrap(),
            provider_profile_id: ProviderProfileId::new(),
            model_id: "model-a".to_string(),
            reasoning_effort: Some("high".to_string()),
            mode_id: Some("plan".to_string()),
            config_values: Default::default(),
        };
        let catalog = RemoteAgentRequest::ListRuntimeOptions(RemoteAgentRuntimeOptionsRequest {
            auth: auth.clone(),
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
}

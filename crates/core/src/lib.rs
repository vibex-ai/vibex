//! Shared Vibex contracts.
//!
//! This crate is intentionally dependency-light. It owns domain ids, structured
//! errors, protocol channel metadata, and minimal event envelopes only.

mod acp_catalog;
pub mod agent;
pub mod agent_auth;
pub mod agent_config;
pub mod agent_provider_runtime;
pub mod automation_graph;
pub mod canonical_json;
pub mod delegation;
pub mod diagnostics;
pub mod elicitation;
pub mod error;
pub mod event;
pub mod file;
pub mod git;
pub mod ids;
pub mod permission;
pub mod provider;
pub mod provider_projection;
pub mod relay;
pub mod remote;
pub mod remote_v2;
pub mod runtime;
pub mod scheduled_task;
pub mod session_import;
pub mod terminal;
pub mod time;
pub mod timeline;
pub mod usage;
pub mod workbench;
pub mod workspace;

pub use acp_catalog::{AcpAgentCatalogEntry, acp_agent_catalog_entries};

pub use agent::{
    AgentCommandDiscoverRequest, AgentCommandDiscoverResponse, AgentCommandEntry,
    AgentCommandExecuteRequest, AgentCommandExecuteResult, AgentCommandExecuteStatus,
    AgentCommandExecutionBehavior, AgentCommandSelectionBehavior, AgentCommandSourceKind,
    AgentCommandTrigger, AgentModelCapabilities, AgentModelListRequest, AgentModelListResponse,
    AgentModelListSource, AgentNotificationIntent, AgentNotificationKind, AgentReasoningEffort,
    AgentSession, AgentSessionConfigProbe, AgentSessionSafety, AgentSessionState,
    AgentSessionSummary, ContinueAgentTurnRequest, CreateAgentSessionRequest, FetchTimelineRequest,
    ForkAgentSessionRequest, GetMessageSubmissionRequest, MAX_AGENT_SESSION_TITLE_CHARS,
    MAX_MESSAGE_IDEMPOTENCY_KEY_LEN, MessageSubmissionState, ProviderCapabilitiesResponse,
    RenameAgentSessionRequest, ResolveElicitationRequest, ResolvePermissionRequest,
    SendAgentMessageRequest, agent_session_turn_requires_continuation,
    normalize_agent_session_title,
};
pub use agent_auth::{
    AgentAuthCatalog, AgentAuthContext, AgentAuthContextAuthenticateRequest,
    AgentAuthContextAuthenticateResult, AgentAuthContextCancelAuthenticationRequest,
    AgentAuthContextLogoutPreview, AgentAuthContextLogoutRequest, AgentAuthContextMutationResult,
    AgentAuthContextRefreshModelsRequest, AgentAuthContextStatus, AgentAuthContextVerifyRequest,
    AgentAuthEnvironmentUpdateRequest, AgentAuthEnvironmentValue, AgentAuthEnvironmentVariable,
    AgentAuthExecutionLocation, AgentAuthMethod, AgentAuthMethodEffect, AgentAuthMethodKind,
    AgentAuthModelCatalogSnapshot, AgentAuthModelCatalogStatus, AgentAuthModelDescriptor,
    AgentAuthStatus, AgentAuthenticateRequest, AgentAuthenticateResult,
    AgentAuthenticationCancelRequest, AgentAuthenticationCompleteRequest,
    AgentAuthenticationOperation, AgentAuthenticationOperationState, AgentLogoutRequest,
    AgentModelDiscoverySource,
};
pub use agent_config::{
    AgentCatalogListResponse, AgentCommandConfig, AgentConfig, AgentConfigStatus, AgentDefinition,
    AgentDiscoveryRecord, AgentId, AgentInstallStatus, AgentListRequest, AgentListResponse,
    AgentManagedDistributionKind, AgentManagedInstallState, AgentManagedInstallStatus,
    AgentRefreshSnapshotRequest, AgentRefreshSnapshotResponse, AgentRuntimeKind,
    AgentRuntimeStatus, AgentSnapshotEntry, AgentSourceKind, AgentUpdateConfigRequest,
    CustomAgentCreateRequest, CustomAgentDeleteRequest, acp_registry_agent_id,
    agent_id_for_provider_kind, builtin_agent_definitions, custom_agent_definition,
    is_user_visible_agent,
};
pub use agent_provider_runtime::*;
pub use automation_graph::{
    AutomationAgentPromptConfig, AutomationApprovalGateConfig, AutomationEdge,
    AutomationEdgeCondition, AutomationEdgeConditionKind, AutomationEdgeCreateRequest,
    AutomationFileCheckConfig, AutomationGitCheckConfig, AutomationGraph,
    AutomationGraphCreateRequest, AutomationGraphDefinitionUpdateRequest,
    AutomationGraphListRequest, AutomationGraphScheduledTaskTrigger, AutomationGraphStatus,
    AutomationGraphTrigger, AutomationGraphUpdateRequest, AutomationNode, AutomationNodeConfig,
    AutomationNodeCreateRequest, AutomationNodeKind, AutomationNodePosition, AutomationRun,
    AutomationRunCancelRequest, AutomationRunCreateRequest, AutomationRunListRequest,
    AutomationRunResumeRequest, AutomationRunStartRequest, AutomationRunStatus, AutomationRunStep,
    AutomationRunStepCreateRequest, AutomationRunStepListRequest, AutomationRunStepStatus,
    AutomationRunStepUpdateRequest, AutomationRunTrigger, AutomationRunUpdateRequest,
    AutomationTerminalCheckConfig,
};
pub use canonical_json::canonical_json_vec;
pub use delegation::{
    AgentDelegation, AgentDelegationStatus, CancelAgentDelegationRequest,
    CreateAgentDelegationRequest, GetAgentDelegationRequest,
};
pub use diagnostics::{
    DIAGNOSTIC_BUNDLE_SCHEMA_VERSION, DiagnosticBundle, DiagnosticBundleMetadata,
    DiagnosticBundleRedactionPolicy, DiagnosticBundleRequest, DiagnosticCount,
    DiagnosticDatabasePathKind, DiagnosticErrorSection, DiagnosticExcludedContent,
    DiagnosticProviderCapabilitySummary, DiagnosticProviderHealthProbe,
    DiagnosticProviderHealthSummary, DiagnosticProviderProfileRef, DiagnosticProviderSection,
    DiagnosticProviderUsageSummary, DiagnosticReleaseContext, DiagnosticRuntimeMetric,
    DiagnosticRuntimeSection, DiagnosticScheduledTaskAttentionRecord,
    DiagnosticScheduledTaskAuditRecord, DiagnosticScheduledTaskSection, DiagnosticSmokeCommandKind,
    DiagnosticSmokeCommandReference, DiagnosticSmokeSection, DiagnosticStorageSection,
    DiagnosticWorkbenchSection,
};
pub use elicitation::{
    ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationOption,
    ElicitationRequest, ElicitationRequestStatus, ElicitationResolution,
    ElicitationResolutionAction, ElicitationStringFormat,
};
pub use error::{ErrorCategory, RedactedDiagnostic, VibexError, VibexResult};
pub use event::{
    ChunkDescriptor, EventChannel, EventKind, EventPayload, FoundationStatusPayload,
    ProtocolChannelKind, ProtocolFrameEncoding, ProtocolVersion, VibexEventEnvelope,
};
pub use file::{
    FileEncoding, FileEntryKind, FileLineEnding, FileMutationRequest, FilePreviewKind,
    FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult, FileTreeEntry,
    FileTreeRequest, FileWriteRequest,
};
pub use git::{
    GitBlameLine, GitBlameRequest, GitBlameResponse, GitBranchCheckoutRequest,
    GitBranchCreateRequest, GitBranchListResponse, GitBranchSummary, GitChange, GitChangeKind,
    GitCommitDetail, GitCommitDetailRequest, GitCommitFileChange, GitCommitRequest,
    GitCommitResult, GitCommitSummary, GitDiffRequest, GitDiffResponse, GitHistoryAuthor,
    GitHistoryRequest, GitHistoryResponse, GitManagedWorktreeRecord, GitManagedWorktreeStatus,
    GitPathIdentity, GitProjectEligibility, GitProjectEligibilityState, GitProjectIneligibleReason,
    GitRemoteActionKind, GitRemoteActionRequest, GitRemoteActionResult, GitRemoteSummary,
    GitRepositoryIdentity, GitSnapshot, GitStageRequest, GitStatusSummary,
    GitWorktreeArchiveRequest, GitWorktreeAssistanceSessionRequest, GitWorktreeChangeSummary,
    GitWorktreeCheckOutcome, GitWorktreeCheckRecord, GitWorktreeConflictFile,
    GitWorktreeConflictKind, GitWorktreeConflictResolveRequest, GitWorktreeConflictStageRequest,
    GitWorktreeConflictVersion, GitWorktreeCreateRequest, GitWorktreeCreateResult,
    GitWorktreeDestructiveAction, GitWorktreeDestructivePreflight, GitWorktreeDiagnostic,
    GitWorktreeDiagnosticSeverity, GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot,
    GitWorktreeListResponse, GitWorktreeLockKey, GitWorktreeLockKind, GitWorktreeMergePlan,
    GitWorktreeMergeRequest, GitWorktreeMergeStrategy, GitWorktreeOperationCheckpoint,
    GitWorktreeOperationDetail, GitWorktreeOperationKind, GitWorktreeOperationRecord,
    GitWorktreeOperationRequest, GitWorktreeOperationStatus, GitWorktreeReadinessRecord,
    GitWorktreeReadinessRequest, GitWorktreeReadinessState, GitWorktreeReconcileReport,
    GitWorktreeReconciliationState, GitWorktreeRestoreRequest, GitWorktreeRisk,
    GitWorktreeRiskKind, GitWorktreeRunningConsumers, GitWorktreeSummary,
    managed_worktree_name_slug,
};
pub use ids::{
    AgentAuthContextId, AgentAuthenticationOperationId, AgentConfiguredModelBindingId,
    AgentDelegationId, AgentModelProviderBindingId, AgentProviderProjectionDescriptorId,
    AgentRuntimeProbeId, AgentRuntimeProfileId, AutomationEdgeId, AutomationGraphId,
    AutomationNodeId, AutomationRunId, AutomationRunStepId, ChannelId, CorrelationId, DeviceId,
    EventId, HookId, McpServerId, MessageSubmissionId, ModelProviderProfileId, NativeStateHomeId,
    ProjectId, PromptId, ProviderProfileId, RelayConnectionId, RelayFrameId, RelayPeerId,
    RelayRoomId, RelaySessionId, RequestId, RuntimeBindingId, RuntimeClientId, RuntimeLeaseId,
    RuntimeProcessId, RuntimeStreamId, RuntimeSwitchId, RuntimeSwitchOperationId, ScheduledTaskId,
    ScheduledTaskRunId, SkillId, TerminalId, TimelineItemId, UsageExecutionId, VibexSessionId,
    WorkspaceId,
};
pub use permission::{
    PermissionActionDetail, PermissionMode, PermissionRequest, PermissionRequestStatus,
    PermissionResolution, PermissionResponseKind, PermissionResponseOption, PermissionRiskCategory,
};
pub use provider::{
    AcpProcessStrategy, AcpProviderCatalogListResponse, AcpProviderCatalogPreset,
    AcpProviderConfig, AcpProviderEnvReference, AcpProviderEnvSource,
    AcpProviderProfileCreateRequest, AcpProviderProfileUpdateRequest, AdapterDiagnostic,
    AdapterDiagnosticLevel, AgentModelProviderDefaultRequest, AgentModelProviderDefaultSelection,
    AgentModelProviderDisplayOrderEntry, AgentModelProviderDisplayOrderListRequest,
    AgentModelProviderDisplayOrderListResponse, AgentModelProviderDisplayOrderSetEntry,
    AgentModelProviderDisplayOrderSetRequest, AgentModelProviderDisplayOrderSetResponse,
    AgentModelProviderFailoverEntry, AgentModelProviderFailoverListRequest,
    AgentModelProviderFailoverListResponse, AgentModelProviderFailoverSetEntry,
    AgentModelProviderFailoverSetRequest, AgentModelProviderProfile,
    AgentModelProviderProfileCreateRequest, AgentModelProviderProfileDeleteRequest,
    AgentModelProviderProfileFetchModelsRequest, AgentModelProviderProfileFetchModelsResponse,
    AgentModelProviderProfileListRequest, AgentModelProviderProfileListResponse,
    AgentModelProviderProfileSecretValueRequest, AgentModelProviderProfileSecretValueResponse,
    AgentModelProviderProfileSecretValueUpdateRequest, AgentModelProviderProfileTestRequest,
    AgentModelProviderProfileTestResult, AgentModelProviderProfileUpdateRequest,
    AgentModelProviderSetDefaultRequest, AgentModelProviderTestStatus, Hook, HookCreateRequest,
    HookDeleteRequest, HookEventKind, HookInstallPreview, HookInstallPreviewRequest,
    HookInstallState, HookStatus, HookUpdateRequest, McpSecretTarget, McpServer,
    McpServerAgentMatrix, McpServerAgentMatrixListRequest, McpServerCreateRequest,
    McpServerDeleteRequest, McpServerDiscoverRequest, McpServerDiscovery,
    McpServerDiscoveryResponse, McpServerEnvEntry, McpServerForAgentListRequest,
    McpServerHeaderEntry, McpServerImportRequest, McpServerImportResult, McpServerImportSelection,
    McpServerProviderMatrix, McpServerScopeKind, McpServerSecretReference,
    McpServerSecretReferenceCreateRequest, McpServerSetAgentMatrixRequest,
    McpServerSetProviderMatrixRequest, McpServerStatus, McpServerSummary, McpServerTransportKind,
    McpServerUpdateRequest, McpServerValidateRequest, McpServerValidationResult,
    McpServerValidationStatus, Prompt, PromptCreateRequest, PromptDeleteRequest, PromptKind,
    PromptScopeKind, PromptStatus, PromptSummary, PromptUpdateRequest, PromptValidateRequest,
    PromptValidationResult, PromptValidationStatus, ProviderBinding, ProviderBindingMetadata,
    ProviderCapabilities, ProviderCapabilityProbeRequest, ProviderCapabilityProbeResult,
    ProviderCapabilityProbeStatus, ProviderCapabilitySummary, ProviderConfiguredModel,
    ProviderDefaultScopeKind, ProviderFailoverRecommendation, ProviderFailoverRecommendationReason,
    ProviderFailoverRecommendationRequest, ProviderFailoverRecommendationStatus,
    ProviderHealthProbeKind, ProviderHealthProbeRequest, ProviderHealthProbeResult,
    ProviderHealthStatus, ProviderHealthSummary, ProviderInjectionField,
    ProviderInjectionOverlayFile, ProviderInjectionPreview, ProviderInjectionPreviewRequest,
    ProviderInjectionStrategy, ProviderKind, ProviderModelCapabilities, ProviderModelWireApi,
    ProviderNativeBinding, ProviderNativeConfigFile, ProviderNativeConfigFileKind,
    ProviderNativeConfigFileStatus, ProviderNativeExportApplyRequest,
    ProviderNativeExportApplyResult, ProviderNativeExportApplyStatus, ProviderNativeExportFilePlan,
    ProviderNativeExportFileStatus, ProviderNativeExportListRequest, ProviderNativeExportMode,
    ProviderNativeExportOperationKind, ProviderNativeExportPreview,
    ProviderNativeExportPreviewRequest, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackRequest, ProviderNativeExportRollbackResult,
    ProviderNativeExportRollbackStatus, ProviderNativeExportSource,
    ProviderNativeImportCreateRequest, ProviderNativeImportCreateResult,
    ProviderNativeImportDiagnostic, ProviderNativeImportItem, ProviderNativeImportItemStatus,
    ProviderNativeImportPreview, ProviderNativeImportPreviewRequest,
    ProviderNativeImportRedactedField, ProviderNativeImportSource, ProviderNetworkDefaults,
    ProviderOptions, ProviderPermissionDefaults, ProviderProfile, ProviderProfileCreateRequest,
    ProviderProfileDefaultScope, ProviderProfileDefaultSelection, ProviderProfileDeleteRequest,
    ProviderProfileDuplicateRequest, ProviderProfileSetDefaultRequest, ProviderProfileStatus,
    ProviderProfileSummary, ProviderProfileUpdateRequest, ProviderRunCapabilityProbesRequest,
    ProviderRunCapabilityProbesResult, ProviderRunHealthProbesRequest,
    ProviderRunHealthProbesResult, ProviderSandboxDefaults, ProviderSecretBackend,
    ProviderSecretKind, ProviderSecretReference, ProviderSecretReferenceCreateRequest,
    ProviderSecretSetupState, ProviderSessionConfigOption, ProviderSessionConfigOptionKind,
    ProviderSessionConfigState, ProviderSessionConfigValue, ProviderUsageBalance,
    ProviderUsageListRequest, ProviderUsageRecord, ProviderUsageSummary, ProviderUsageUnit,
    ProviderUsageWindow, ProviderVersionInfo, ResourceAgentMatrixSourceKind,
    ResourceDiscoveryStatus, Skill, SkillAgentMatrix, SkillAgentMatrixListRequest,
    SkillCreateRequest, SkillDeleteRequest, SkillDiscoverRequest, SkillDiscovery,
    SkillDiscoveryResponse, SkillForAgentListRequest, SkillImportRequest, SkillImportResult,
    SkillImportSelection, SkillProviderMatrix, SkillScopeKind, SkillSetAgentMatrixRequest,
    SkillSetProviderMatrixRequest, SkillSourceKind, SkillStatus, SkillSummary, SkillUpdateRequest,
    SkillValidateRequest, SkillValidationResult, SkillValidationStatus,
};
pub use provider_projection::*;
pub use relay::{
    RelayBridgeMessage, RelayControlMessage, RelayDeepLink, RelayEncryptedFrame, RelayError,
    RelayErrorCode, RelayFrameKind, RelayHandshakeHello, RelayHandshakeReady, RelayHeartbeat,
    RelayHeartbeatAck, RelayNotificationProviderKind, RelayOpaqueNotification, RelayPeerMessage,
    RelayPeerRole, RelayPlaintextEnvelope, RelayProtocolVersion, RelayPushDispatchResult,
    RelayPushRegistration, RelayRemoteHandshakeContext, RelayTransportMode,
};
pub use remote::{
    RemoteActionClass, RemoteAgentAttachRuntimeRequest, RemoteAgentAttachRuntimeResponse,
    RemoteAgentAuthContextListRequest, RemoteAgentAuthContextListResponse,
    RemoteAgentAuthContextMutationResponse, RemoteAgentAuthLogoutPreviewRequest,
    RemoteAgentAuthLogoutPreviewResponse, RemoteAgentAuthMethodListRequest,
    RemoteAgentAuthMethodListResponse, RemoteAgentAuthenticateContextRequest,
    RemoteAgentAuthenticateContextResponse, RemoteAgentAuthenticationOperationRequest,
    RemoteAgentAuthenticationOperationResponse, RemoteAgentCancelContextAuthenticationRequest,
    RemoteAgentCancelRuntimeSwitchRequest, RemoteAgentCancelRuntimeSwitchResponse,
    RemoteAgentCatchUpRequest, RemoteAgentCatchUpResponse, RemoteAgentConfigSummary,
    RemoteAgentConfigSummaryListRequest, RemoteAgentConfigSummaryListResponse,
    RemoteAgentContinueTurnRequest, RemoteAgentContinueTurnResponse,
    RemoteAgentCreateSessionRequest, RemoteAgentCreateSessionResponse,
    RemoteAgentDeepLinkResolveRequest, RemoteAgentDeepLinkResolveResponse,
    RemoteAgentDetachRuntimeRequest, RemoteAgentDetachRuntimeResponse, RemoteAgentInterruptRequest,
    RemoteAgentInterruptResponse, RemoteAgentLogoutAuthContextRequest,
    RemoteAgentMessageSubmissionRequest, RemoteAgentMessageSubmissionResponse,
    RemoteAgentOperationKind, RemoteAgentProjectionCapabilityRequest,
    RemoteAgentProjectionCapabilityResponse, RemoteAgentProjectionPreviewRequest,
    RemoteAgentProjectionPreviewResponse, RemoteAgentRefreshAuthModelsRequest,
    RemoteAgentRenameSessionRequest, RemoteAgentRenameSessionResponse, RemoteAgentRequest,
    RemoteAgentResolveElicitationRequest, RemoteAgentResolveElicitationResponse,
    RemoteAgentResolvePermissionRequest, RemoteAgentResolvePermissionResponse,
    RemoteAgentRuntimeEventsRequest, RemoteAgentRuntimeEventsResponse,
    RemoteAgentRuntimeOptionsRequest, RemoteAgentRuntimeOptionsResponse,
    RemoteAgentRuntimeProbeCancelRequest, RemoteAgentRuntimeProbeCancelResponse,
    RemoteAgentRuntimeProbeGetRequest, RemoteAgentRuntimeProbeGetResponse,
    RemoteAgentRuntimeProbeListRequest, RemoteAgentRuntimeProbeListResponse,
    RemoteAgentRuntimeProbeStartRequest, RemoteAgentRuntimeProbeStartResponse,
    RemoteAgentRuntimeProcessSnapshotRequest, RemoteAgentRuntimeProcessSnapshotResponse,
    RemoteAgentRuntimeSelectionRequest, RemoteAgentRuntimeSelectionResponse,
    RemoteAgentRuntimeSnapshotRequest, RemoteAgentRuntimeSnapshotResponse,
    RemoteAgentSendMessageRequest, RemoteAgentSendMessageResponse, RemoteAgentSessionActionRequest,
    RemoteAgentSessionActionResponse, RemoteAgentSessionDetailRequest,
    RemoteAgentSessionDetailResponse, RemoteAgentSessionListRequest,
    RemoteAgentSessionListResponse, RemoteAgentSetDesiredRuntimeRequest,
    RemoteAgentSetDesiredRuntimeResponse, RemoteAgentTimelineCursor,
    RemoteAgentTimelineFetchRequest, RemoteAgentTimelineFetchResponse,
    RemoteAgentVerifyAuthContextRequest, RemoteAuditAction, RemoteAuditListRequest,
    RemoteAuditListResponse, RemoteAuditOutcome, RemoteAuditRecord, RemoteAuditTargetKind,
    RemoteAuthContext, RemoteAuthProof, RemoteCapabilitySummary, RemoteCatchUpCursor,
    RemoteCatchUpRequest, RemoteCatchUpResponse, RemoteClaimPairingCodeRequest,
    RemoteClaimPairingCodeResponse, RemoteCreatePairingCodeRequest,
    RemoteCreatePairingCodeResponse, RemoteDeepLinkResolution, RemoteDeepLinkResolutionStatus,
    RemoteDeviceDetail, RemoteDevicePermissionLevel, RemoteDeviceStatus, RemoteDeviceSummary,
    RemoteEnvelopeStatus, RemoteFileDeleteResponse, RemoteFileMutationRequest,
    RemoteFileReadRequest, RemoteFileReadResponse, RemoteFileRenameResponse,
    RemoteFileSearchRequest, RemoteFileSearchResponse, RemoteFileTreeRequest,
    RemoteFileTreeResponse, RemoteFileWriteRequest, RemoteFileWriteResponse, RemoteGitBlameRequest,
    RemoteGitBlameResponse, RemoteGitBranchCheckoutRequest, RemoteGitBranchCreateRequest,
    RemoteGitBranchListRequest, RemoteGitBranchListResponse, RemoteGitCommitDetailRequest,
    RemoteGitCommitDetailResponse, RemoteGitCommitRequest, RemoteGitCommitResponse,
    RemoteGitDiffRequest, RemoteGitDiffResponse, RemoteGitHistoryRequest, RemoteGitHistoryResponse,
    RemoteGitRemoteActionRequest, RemoteGitRemoteActionResponse, RemoteGitStageRequest,
    RemoteGitStatusMutationResponse, RemoteGitStatusRequest, RemoteGitStatusResponse,
    RemoteGitWorktreeEligibilityRequest, RemoteGitWorktreeEligibilityResponse,
    RemoteGitWorktreeSnapshotRequest, RemoteGitWorktreeSnapshotResponse, RemoteHandshakeRequest,
    RemoteHandshakeResponse, RemoteHealthState, RemoteHealthStatus, RemoteLiveEventChannel,
    RemoteLiveEventEnvelope, RemoteOperationKind, RemotePairingCode, RemoteProtocolVersion,
    RemoteProviderFailoverRecommendationListRequest,
    RemoteProviderFailoverRecommendationListResponse, RemoteProviderHealthSummaryListRequest,
    RemoteProviderHealthSummaryListResponse, RemoteProviderInjectionPreviewRequest,
    RemoteProviderInjectionPreviewResponse, RemoteProviderOperationKind,
    RemoteProviderProfileListRequest, RemoteProviderProfileListResponse, RemoteProviderRequest,
    RemoteProviderRunHealthProbesRequest, RemoteProviderRunHealthProbesResponse,
    RemoteProviderUsageSummaryListRequest, RemoteProviderUsageSummaryListResponse,
    RemoteRequestEnvelope, RemoteResponseEnvelope, RemoteRevokeDeviceRequest, RemoteServiceInfo,
    RemoteSidebarDropPosition, RemoteSidebarFolder, RemoteSidebarHierarchyMode,
    RemoteSidebarItemKind, RemoteSidebarItemRef, RemoteSidebarNewSessionLocation,
    RemoteSidebarOrganizationMutateRequest, RemoteSidebarOrganizationMutation,
    RemoteSidebarOrganizationRequest, RemoteSidebarOrganizationResponse,
    RemoteSidebarOrganizationSnapshot, RemoteSidebarPlacement, RemoteSidebarProjectAppearance,
    RemoteTerminalCreateRequest, RemoteTerminalCreateResponse, RemoteTerminalKillRequest,
    RemoteTerminalKillResponse, RemoteTerminalListRequest, RemoteTerminalListResponse,
    RemoteTerminalResizeRequest, RemoteTerminalResizeResponse, RemoteTerminalSnapshotRequest,
    RemoteTerminalSnapshotResponse, RemoteTerminalWriteRequest, RemoteTerminalWriteResponse,
    RemoteWorkbenchDeleteWorkspaceRequest, RemoteWorkbenchDeleteWorkspaceResponse,
    RemoteWorkbenchListWorkspacesRequest, RemoteWorkbenchListWorkspacesResponse,
    RemoteWorkbenchOpenWorkspaceRequest, RemoteWorkbenchOpenWorkspaceResponse,
    RemoteWorkbenchOperationKind, RemoteWorkbenchRequest,
};
pub use remote_v2::*;
pub use runtime::{
    AcpAdapterId, ActiveWorkKind, AgentRuntimeRouteKey, AgentSessionRestoreAttempt,
    AgentSessionRestoreCompatibility, AgentSessionRestoreCompatibilityKey,
    AgentSessionRestoreMethod, AgentSessionRestoreOutcome, AgentSessionRestoreResult,
    AgentSessionRestoreStrategy, AgentSessionRuntimeSelectionEvent,
    AgentSessionRuntimeSelectionState, AgentSessionRuntimeSnapshot, AgentTokenUsage,
    AttachRuntimeRequest, AttachRuntimeResponse, BindingState, BusyDisposition,
    CancelAgentSessionRuntimeSwitchRequest, DetachRuntimeRequest, DetachRuntimeResponse,
    GetRuntimeEventsRequest, GetRuntimeProcessSnapshotRequest, GetRuntimeSnapshotRequest,
    MAX_RUNTIME_SELECTION_ERROR_CODE_LEN, MAX_RUNTIME_SELECTION_ERROR_MESSAGE_LEN,
    MAX_RUNTIME_SELECTION_RECOVERY_HINT_LEN, MAX_RUNTIME_SWITCH_WAIT_DEADLINE_MS,
    MessageSubmissionStatus, RestoreIncompatibilityReason, RetrySemantics, RuntimeAgentSummary,
    RuntimeAttachmentSnapshot, RuntimeAttachmentStatus, RuntimeAuthSource, RuntimeAuthSourceAction,
    RuntimeAuthSourceAvailability, RuntimeAuthSourceKind, RuntimeAuthSourceSummary, RuntimeBinding,
    RuntimeEventBatch, RuntimeEventCursor, RuntimeEventKind, RuntimeLeaseRole,
    RuntimeLeaseRoleCounts, RuntimeLiveMessageSnapshot, RuntimeLiveToolCallSnapshot,
    RuntimeMaterializationStatus, RuntimeModelSelection, RuntimeOptionAvailability,
    RuntimeProcessConfigStatus, RuntimeProcessSnapshot, RuntimeProcessStatus,
    RuntimeSelectionActionableError, RuntimeSelectionInteraction, RuntimeSessionEvent,
    RuntimeSwitchActiveWorkPolicy, RuntimeSwitchEventKind, RuntimeSwitchEventProjection,
    RuntimeSwitchEventVisibility, RuntimeSwitchPolicy, RuntimeSwitchStatus, SessionConfigValue,
    SessionRuntimeConfigApplyStatus, SessionRuntimeConfigFieldOutcome,
    SessionRuntimeConfigMutationRequest, SessionRuntimeConfigMutationResult,
    SessionRuntimeConfigPatch, SessionRuntimeConfigState, SessionRuntimeConfigValueState,
    SessionRuntimeFeature, SessionRuntimeFeatureKind, SessionRuntimeOption,
    SessionRuntimeOptionCatalog, SessionRuntimeSelection, SessionRuntimeSelectionStatus,
    SetDesiredAgentSessionRuntimeRequest, SwitchAgentSessionRuntimeRequest,
    SwitchAgentSessionRuntimeResponse, SwitchOperationStatus, TransportKind,
};
pub use scheduled_task::{
    ScheduledTask, ScheduledTaskAttentionKind, ScheduledTaskAttentionListRequest,
    ScheduledTaskAttentionSummary, ScheduledTaskAuditListRequest, ScheduledTaskAuditOutcome,
    ScheduledTaskAuditRecord, ScheduledTaskCreateRequest, ScheduledTaskDailySchedule,
    ScheduledTaskIntervalSchedule, ScheduledTaskListRequest, ScheduledTaskOneShotSchedule,
    ScheduledTaskRun, ScheduledTaskRunCreateRequest, ScheduledTaskRunListRequest,
    ScheduledTaskRunStatus, ScheduledTaskRunTrigger, ScheduledTaskRunUpdateRequest,
    ScheduledTaskSchedule, ScheduledTaskStatus, ScheduledTaskUpdateRequest,
};
pub use session_import::{
    ExternalSessionContinuationStatus, ExternalSessionImportCandidate,
    ExternalSessionImportCandidateStatus, ExternalSessionImportDiagnostic,
    ExternalSessionImportPreview, ExternalSessionImportPreviewRequest,
    ExternalSessionImportRequest, ExternalSessionImportResult, ExternalSessionImportSource,
    ExternalSessionImportTimelineItem, ExternalSessionImportedTimelineCount,
    IMPORT_METADATA_CANDIDATE_ID, IMPORT_METADATA_CONTINUATION_REASON,
    IMPORT_METADATA_CONTINUATION_STATUS, IMPORT_METADATA_NATIVE_HISTORY_IMPORT_VERSION,
    IMPORT_METADATA_NATIVE_HISTORY_IMPORTED, IMPORT_METADATA_SOURCE, IMPORT_METADATA_VERSION,
};
pub use terminal::{
    TerminalAuthActionDescriptor, TerminalCreateRequest, TerminalOutputChunk,
    TerminalResizeRequest, TerminalSession, TerminalShell, TerminalSnapshot, TerminalStatus,
    TerminalSwitchShellRequest, TerminalWriteRequest,
};
pub use time::unix_timestamp_ms;
pub use timeline::{
    AgentEventContentBlock, AgentEventLocation, AgentEventRawExtension, AgentEventRawOutput,
    AgentEventRawOutputMode, AgentMessageDeltaPayload, AgentMessagePayload, AgentMessagePhase,
    AgentRetryPayload, CollaborationPayload, CommandPayload, CommandStatus, FileOperationKind,
    FileOperationPatch, FileOperationPatchFormat, FileOperationPayload, GitNoticePayload,
    ImageGenerationPayload, MessageAttachment, PlanPayload, PlanStepPayload, PlanStepStatus,
    ReasoningPayload, RetryKind, RetryPhase, SystemNoticeLevel, SystemNoticePayload,
    TimelineErrorPayload, TimelineItem, TimelineItemKind, TimelineLiveEvent, TimelinePage,
    TimelinePayload, TimelineRedactionState, TimelineSource, TodoUpdatePayload, ToolCallPayload,
    ToolCallStatus, TurnExecutionAttribution, TurnExecutionAttributionView, UserMessagePayload,
    WebSearchPayload, latest_timeline_turn_ended_normally,
};
pub use usage::{
    AgentTurnUsageFact, AgentUsageAggregate, AgentUsageAnnualDay, AgentUsageAnnualProjection,
    AgentUsageCacheHitRate, AgentUsageCounterOrigin, AgentUsageCounterScope, AgentUsageCoverage,
    AgentUsageCoverageSummary, AgentUsageDailyModelUsage, AgentUsageDimension,
    AgentUsageDimensionRow, AgentUsageEffectiveRange, AgentUsageExecution,
    AgentUsageExecutionContext, AgentUsageExecutionStatus, AgentUsageExecutionStatusUpdate,
    AgentUsageFilterOption, AgentUsageFilterOptions, AgentUsageMetricCoverage,
    AgentUsageMetricValue, AgentUsageObservation, AgentUsageObservationSource, AgentUsageRange,
    AgentUsageReportedFields, AgentUsageReportingContract, AgentUsageSortDirection,
    AgentUsageSortMetric, AgentUsageStatistics, AgentUsageStatisticsRequest,
    AgentUsageStreamAttribution, AgentUsageTimeZone, AgentUsageTokenValues, AgentUsageTrendBucket,
    AgentUsageTrendMetric, MAX_AGENT_USAGE_TOKEN_VALUE, agent_usage_counter_scope,
    agent_usage_reporting_contract,
};
pub use workbench::{WorkbenchPanel, WorkbenchTabKind};
pub use workspace::{
    OpenWorkspaceRequest, ProjectRecord, ProjectWorkspaceSummary, WorkspaceAggregateStatus,
    WorkspaceMode, WorkspaceRecord,
};

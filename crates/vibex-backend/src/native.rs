use std::sync::Arc;
use std::time::Duration;

use vibex_core::{
    AcpProviderCatalogListResponse, AcpProviderConfig, AcpProviderProfileUpdateRequest,
    AgentAuthCatalog, AgentAuthContext, AgentAuthContextAuthenticateRequest,
    AgentAuthContextAuthenticateResult, AgentAuthContextCancelAuthenticationRequest,
    AgentAuthContextId, AgentAuthContextLogoutPreview, AgentAuthContextLogoutRequest,
    AgentAuthContextMutationResult, AgentAuthContextRefreshModelsRequest,
    AgentAuthContextVerifyRequest, AgentAuthEnvironmentUpdateRequest, AgentAuthenticateRequest,
    AgentAuthenticateResult, AgentAuthenticationCancelRequest, AgentAuthenticationOperation,
    AgentAuthenticationOperationId, AgentCatalogListResponse, AgentCommandDiscoverRequest,
    AgentCommandDiscovery, AgentCommandExecuteRequest, AgentCommandExecuteResult, AgentId,
    AgentListRequest, AgentListResponse, AgentLogoutRequest, AgentManagedInstallState,
    AgentModelProviderDisplayOrderListRequest, AgentModelProviderDisplayOrderListResponse,
    AgentModelProviderDisplayOrderSetRequest, AgentModelProviderDisplayOrderSetResponse,
    AgentModelProviderDraftFetchModelsRequest, AgentModelProviderDraftFetchModelsResponse,
    AgentModelProviderProfileCreateRequest, AgentModelProviderProfileDeleteRequest,
    AgentModelProviderProfileFetchModelsRequest, AgentModelProviderProfileFetchModelsResponse,
    AgentModelProviderProfileSecretValueRequest, AgentModelProviderProfileSecretValueResponse,
    AgentModelProviderProfileSecretValueUpdateRequest, AgentModelProviderProfileTestRequest,
    AgentModelProviderProfileTestResult, AgentModelProviderProfileUpdateRequest,
    AgentRefreshSnapshotRequest, AgentRefreshSnapshotResponse, AgentRuntimeOptionProbeRequest,
    AgentRuntimeOptionProbeResult, AgentSession, AgentSessionRuntimeSelectionState,
    AgentSnapshotEntry, AgentUpdateConfigRequest, AgentUsageStatistics,
    AgentUsageStatisticsRequest, AutomationGraph, AutomationGraphCreateRequest,
    AutomationGraphDefinitionUpdateRequest, AutomationGraphId, AutomationGraphListRequest,
    AutomationGraphStatus, AutomationGraphUpdateRequest, AutomationRun, AutomationRunCancelRequest,
    AutomationRunListRequest, AutomationRunResumeRequest, AutomationRunStartRequest,
    AutomationRunStep, AutomationRunStepListRequest, BackupCreateOutcome, BackupCreatePayload,
    BackupInspectOutcome, BackupInspectPayload, BackupRestoreOutcome, BackupRestorePayload,
    BrowserActionRecord, BrowserAvailability, BrowserCaptureQuality, BrowserSession,
    BrowserSessionId, BrowserSessionSnapshot, BrowserTab, BrowserTabId,
    CancelAgentSessionRuntimeSwitchRequest, ContinueAgentTurnRequest, CreateAgentSessionRequest,
    DiagnosticExportOutcome, DiagnosticExportPayload, FetchTimelineRequest, FileMutationRequest,
    FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult, FileTreeEntry,
    FileTreeRequest, FileWriteRequest, ForkAgentSessionRequest, GatewayAgentUpdateRequest,
    GatewayCapabilityField, GatewayCapabilitySuggestion, GatewayRequestSummary,
    GatewaySettingsUpdateRequest, GatewaySnapshot, GetMessageSubmissionRequest,
    GitBranchListResponse, GitCommitDetail, GitCommitDetailRequest, GitCommitRequest,
    GitCommitResult, GitDiffRequest, GitDiffResponse, GitHistoryRequest, GitHistoryResponse,
    GitProjectEligibility, GitRemoteActionRequest, GitRemoteActionResult, GitStageRequest,
    GitStatusSummary, GitWorktreeArchiveRequest, GitWorktreeAssistanceSessionRequest,
    GitWorktreeConflictResolveRequest, GitWorktreeConflictStageRequest, GitWorktreeCreateRequest,
    GitWorktreeCreateResult, GitWorktreeDestructivePreflight, GitWorktreeDiscardRequest,
    GitWorktreeLifecycleSnapshot, GitWorktreeMergePlan, GitWorktreeMergeRequest,
    GitWorktreeOperationRecord, GitWorktreeOperationRequest, GitWorktreeReadinessRecord,
    GitWorktreeReadinessRequest, GitWorktreeRestoreRequest, Hook, HookCreateRequest,
    HookDeleteRequest, HookInstallPreview, HookInstallPreviewRequest, HookUpdateRequest,
    ManagementSnapshotPayload, McpMarketInstallRequest, McpMarketInstallResult,
    McpMarketSearchRequest, McpMarketSearchResponse, McpServer, McpServerAgentMatrix,
    McpServerAgentMatrixListRequest, McpServerCreateRequest, McpServerDeleteRequest,
    McpServerDiscoverRequest, McpServerDiscoveryResponse, McpServerImportRequest,
    McpServerImportResult, McpServerSetAgentMatrixRequest, McpServerUpdateRequest,
    McpServerValidateRequest, McpServerValidationResult, MessageSubmissionState,
    ModelCapabilitySuggestionRequest, OpenWorkspaceRequest, ProjectId, Prompt, PromptCreateRequest,
    PromptDeleteRequest, PromptUpdateRequest, PromptUsage, PromptUsageRecordRequest,
    PromptValidateRequest, PromptValidationResult, ProviderCapabilitySummary,
    ProviderConfiguredModel, ProviderHealthSummary, ProviderNativeExportApplyRequest,
    ProviderNativeExportApplyResult, ProviderNativeExportListRequest, ProviderNativeExportPreview,
    ProviderNativeExportPreviewRequest, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackRequest, ProviderNativeExportRollbackResult,
    ProviderNativeImportCreateRequest, ProviderNativeImportCreateResult,
    ProviderNativeImportPreview, ProviderNativeImportPreviewRequest, ProviderProfile,
    ProviderProfileId, ProviderRunCapabilityProbesRequest, ProviderRunCapabilityProbesResult,
    ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult, ProviderUsageListRequest,
    ProviderUsageSummary, RcImportOutcome, RcImportPayload, RemoteAuditListRequest,
    RemoteAuditRecord, RemoteCreatePairingCodeRequest, RemoteCreatePairingCodeResponse,
    RemoteCreatePairingOfferRequest, RemoteCreatePairingOfferResponse, RemoteDeviceDetail,
    RemoteProviderManagementSnapshot, RemoteRenameDeviceRequest, RemoteRevokeDeviceRequest,
    RemoteSidebarOrganizationMutation, RemoteSidebarOrganizationSnapshot,
    RenameAgentSessionRequest, ReplaceUserMessagePayload, ResolveElicitationRequest,
    ResolvePermissionRequest, ScheduledTaskAttentionListRequest, ScheduledTaskAttentionSummary,
    ScheduledTaskAuditListRequest, ScheduledTaskAuditRecord, ScheduledTaskCreateRequest,
    ScheduledTaskId, ScheduledTaskListRequest, ScheduledTaskRun, ScheduledTaskRunListRequest,
    ScheduledTaskUpdateRequest, SendAgentMessageRequest, SessionRuntimeOptionCatalog,
    SetDesiredAgentSessionRuntimeRequest, Skill, SkillAgentMatrix, SkillAgentMatrixListRequest,
    SkillCreateRequest, SkillDeleteRequest, SkillDiscoverRequest, SkillDiscoveryResponse,
    SkillImportRequest, SkillImportResult, SkillMarketDocument, SkillMarketDocumentRequest,
    SkillMarketInstallRequest, SkillMarketInstallResult, SkillMarketSearchRequest,
    SkillMarketSearchResponse, SkillSetAgentMatrixRequest, SkillUpdateRequest,
    SkillValidateRequest, SkillValidationResult, SteerAgentMessageRequest, SteerAgentMessageResult,
    TerminalCreateRequest, TerminalId, TerminalResizeRequest, TerminalSession, TerminalSnapshot,
    TerminalStatus, TerminalWriteRequest, TimelineItem, TimelinePage, VibexSessionId, WorkspaceId,
};
use vibex_desktop_runtime::{
    AuthoritativeRefetch, DesktopEvent, DesktopEventReceiver, DesktopEventStream, DesktopRuntime,
    DesktopRuntimeFacade, RelayClientConnectionState, TerminalHandle, WorktreeCreateContext,
};

use crate::{
    AgentBackend, AgentModelOwnedCatalogRequest, BackendCapabilitySnapshot, BackendError,
    BackendEvent, BackendEventStream, BackendEventSubscription, BackendFacade, BackendFuture,
    BackendOperation, BackendProjection, BackendRefetch, BackendResult, BrowserBackend,
    BrowserDialogResolution, BrowserFrameBatch, BrowserFrameSubscription, BrowserInputRequest,
    BrowserSessionOpenRequest, BrowserTabOpenRequest, BrowserTabSelection, BrowserViewportRequest,
    DeviceBackend, FileBackend, GitBackend, ManagementBackend, ManagementProfileSelectionRequest,
    MutationRequest, RelayConnectionState, RelayStatusSummary, SidebarBackend, TerminalBackend,
    TerminalFrame, TerminalFrameBatch, TerminalFrameSubscription, WorkspaceBackend,
    WorkspaceSummary,
};
use vibex_desktop_model::{SidebarMutationOutcome, SidebarOrganizationView, UiStateStore};

#[derive(Clone)]
pub struct NativeBackend {
    runtime: Arc<DesktopRuntime>,
}

impl NativeBackend {
    pub fn new(runtime: Arc<DesktopRuntime>) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &Arc<DesktopRuntime> {
        &self.runtime
    }

    pub fn capability_snapshot(&self) -> BackendCapabilitySnapshot {
        let mut snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        if !self
            .runtime
            .management()
            .remote()
            .gateway()
            .pairing_routes_available()
        {
            // Both pairing shapes are offers underneath: without a route to
            // offer, neither a scanned QR nor a discovered window could be
            // claimed, so the runtime stops claiming it can pair at all.
            snapshot
                .device
                .operations
                .remove(&BackendOperation::DevicePairing);
            snapshot
                .device
                .operations
                .remove(&BackendOperation::DeviceLanPairing);
        }
        snapshot
    }

    pub fn facade(self: &Arc<Self>) -> BackendFacade {
        BackendFacade::new(
            self.capability_snapshot(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
        )
    }
}

struct NativeEventSubscription {
    receiver: DesktopEventReceiver,
}

impl BackendEventSubscription for NativeEventSubscription {
    fn next(&mut self) -> BackendFuture<'_, Option<BackendEvent>> {
        Box::pin(async move {
            match self.receiver.recv().await {
                Ok(event) => Ok(Some(map_desktop_event(event))),
                Err(_) => Ok(None),
            }
        })
    }
}

fn map_desktop_event(event: DesktopEvent) -> BackendEvent {
    match event {
        DesktopEvent::Timeline(event) => BackendEvent::Timeline(event),
        DesktopEvent::SessionUpdated(session) => BackendEvent::SessionUpdated(session),
        DesktopEvent::SessionsRemoved(session_ids) => BackendEvent::SessionsRemoved(session_ids),
        DesktopEvent::Runtime(event) => BackendEvent::Runtime(event),
        DesktopEvent::RuntimeSelection(event) => BackendEvent::RuntimeSelection(event),
        DesktopEvent::ProviderConfigChanged(_) => {
            BackendEvent::ProjectionInvalidated(BackendProjection::Management)
        }
        DesktopEvent::ModelGateway(event) => BackendEvent::ModelGateway(event),
        DesktopEvent::UsageInvalidated => {
            BackendEvent::ProjectionInvalidated(BackendProjection::Usage)
        }
        DesktopEvent::Lagged {
            stream,
            skipped,
            refetch,
        } => BackendEvent::Lagged {
            stream: map_event_stream(stream),
            skipped,
            refetch: map_refetch(refetch),
            observed_live: false,
        },
        DesktopEvent::Shutdown => BackendEvent::Disconnected,
    }
}

fn map_event_stream(stream: DesktopEventStream) -> BackendEventStream {
    match stream {
        DesktopEventStream::Timeline => BackendEventStream::Timeline,
        DesktopEventStream::Runtime => BackendEventStream::Runtime,
        DesktopEventStream::RuntimeSelection => BackendEventStream::RuntimeSelection,
        DesktopEventStream::Usage => BackendEventStream::Usage,
        DesktopEventStream::Fanout => BackendEventStream::Fanout,
    }
}

fn map_refetch(refetch: AuthoritativeRefetch) -> BackendRefetch {
    BackendRefetch {
        session_id: refetch.session_id,
        timeline: refetch.timeline,
        runtime: refetch.runtime,
        runtime_selection: refetch.runtime_selection,
        projection: refetch.usage.then_some(BackendProjection::Usage),
    }
}

impl AgentBackend for NativeBackend {
    fn team_snapshot(
        &self,
        request: vibex_core::TeamSnapshotRequest,
    ) -> BackendFuture<'_, vibex_core::TeamSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime
                .vibex_use()
                .human_team_snapshot(request)
                .await
                .map_err(Into::into)
        })
    }

    fn session_tree(
        &self,
        request: vibex_core::SessionTreeRequest,
    ) -> BackendFuture<'_, vibex_core::SessionTreePage> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime
                .vibex_use()
                .human_session_tree(request)
                .await
                .map_err(Into::into)
        })
    }

    fn send_message_with_mentions(
        &self,
        request: MutationRequest<vibex_core::HumanAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<vibex_core::TimelineItem>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .vibex_use()
                .human_send_message_with_mentions(request.payload, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn set_session_access(
        &self,
        request: MutationRequest<vibex_core::SessionAccessRequest>,
    ) -> BackendFuture<'_, vibex_core::SessionAccessResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let key = request
                .idempotency_key
                .clone()
                .unwrap_or_else(|| request.request_id.to_string());
            runtime
                .vibex_use()
                .human_set_session_access(request.payload, key, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn delegate_session(
        &self,
        request: MutationRequest<vibex_core::HumanDelegationRequest>,
    ) -> BackendFuture<'_, vibex_core::HumanDelegationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let key = request
                .idempotency_key
                .clone()
                .unwrap_or_else(|| request.request_id.to_string());
            runtime
                .vibex_use()
                .human_delegate_session(request.payload, key, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn control_team_task(
        &self,
        request: MutationRequest<vibex_core::TeamTaskControlRequest>,
    ) -> BackendFuture<'_, vibex_core::DelegationTaskView> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let key = request
                .idempotency_key
                .clone()
                .unwrap_or_else(|| request.request_id.to_string());
            runtime
                .vibex_use()
                .human_control_team_task(request.payload, key, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn present_team(
        &self,
        request: MutationRequest<vibex_core::TeamPresentationRequest>,
    ) -> BackendFuture<'_, vibex_core::PresentationOutcome> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let key = request
                .idempotency_key
                .clone()
                .unwrap_or_else(|| request.request_id.to_string());
            runtime
                .vibex_use()
                .human_present_team(request.payload, key, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn acknowledge_team_events(
        &self,
        request: MutationRequest<vibex_core::TeamAcknowledgeRequest>,
    ) -> BackendFuture<'_, usize> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .vibex_use()
                .human_acknowledge_team_events(request.payload, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
        Ok(Box::new(NativeEventSubscription {
            receiver: self.runtime.subscribe(),
        }))
    }

    fn agent_tool_preferences(&self) -> BackendFuture<'_, vibex_core::AgentToolPreferences> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.agent().manager().agent_tool_preferences())
        })
    }

    fn set_agent_tool_enabled(
        &self,
        request: MutationRequest<vibex_core::AgentToolPreferenceUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentToolPreferences> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let manager = runtime.agent().manager();
            manager.set_agent_tool_enabled(
                request.payload.agent_id.as_str(),
                request.payload.tool,
                request.payload.enabled,
            );
            // The whole answer comes back rather than the one row that
            // changed: the panels are stored as the disabled sets, and a
            // client that patched its own copy would be guessing at the other
            // one's state.
            Ok(manager.agent_tool_preferences())
        })
    }

    fn list_sessions(&self, include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .list_root_sessions(include_archived)
                .await
                .map_err(Into::into)
        })
    }

    fn open_session(&self, session_id: VibexSessionId) -> BackendFuture<'_, AgentSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .get_session(&session_id)
                .await
                .map_err(Into::into)
        })
    }

    fn create_session(
        &self,
        request: MutationRequest<CreateAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .create_session(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .fetch_timeline(request)
                .await
                .map_err(Into::into)
        })
    }

    fn fetch_timeline_item(
        &self,
        request: vibex_core::FetchTimelineItemRequest,
    ) -> BackendFuture<'_, TimelineItem> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .fetch_timeline_item(request)
                .await
                .map_err(|error| {
                    if error.code == "timeline_item_not_found" {
                        crate::BackendError::conflict(
                            "agent_timeline_item_missing",
                            "the Agent timeline item is no longer available",
                        )
                    } else {
                        error.into()
                    }
                })
        })
    }

    fn get_timeline_display_settings(
        &self,
    ) -> BackendFuture<'_, vibex_core::AgentTimelineDisplaySettings> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.timeline_display_settings())
        })
    }

    fn fork_session(
        &self,
        request: MutationRequest<ForkAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .fork_session(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn usage_statistics(
        &self,
        request: AgentUsageStatisticsRequest,
    ) -> BackendFuture<'_, AgentUsageStatistics> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.usage();
            tokio::task::spawn_blocking(move || service.query_statistics(request))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "agent_usage_query_task_failed",
                        "Agent usage query task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn send_message(
        &self,
        request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .send_message(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn continue_turn(
        &self,
        request: MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .continue_turn(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn interrupt(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .interrupt(&request.payload)
                .await?;
            Ok(true)
        })
    }

    fn native_steering_supported(&self, session_id: VibexSessionId) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .native_steering_supported(&session_id)
                .await
                .map_err(Into::into)
        })
    }

    fn steer_message(
        &self,
        request: MutationRequest<SteerAgentMessageRequest>,
    ) -> BackendFuture<'_, SteerAgentMessageResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .steer_message(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .resolve_permission(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn resolve_elicitation(
        &self,
        request: MutationRequest<ResolveElicitationRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .resolve_elicitation(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn replace_user_message(
        &self,
        request: MutationRequest<ReplaceUserMessagePayload>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .vibex_use()
                .human_replace_user_message(request.payload, "human:local".to_string())
                .await
                .map_err(Into::into)
        })
    }

    fn rename_session(
        &self,
        request: MutationRequest<RenameAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .rename_session(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn archive_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .archive_session(&request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn unarchive_session(
        &self,
        request: MutationRequest<VibexSessionId>,
    ) -> BackendFuture<'_, AgentSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .unarchive_session(&request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn list_archived_sessions(&self, limit: usize) -> BackendFuture<'_, Vec<AgentSession>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .list_archived_sessions(limit)
                .await
                .map_err(Into::into)
        })
    }

    fn delete_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .delete_session(&request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn list_runtime_options(&self) -> BackendFuture<'_, SessionRuntimeOptionCatalog> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .runtime_catalog()
                .list()
                .await
                .map_err(Into::into)
        })
    }

    fn probe_agent_runtime_options(
        &self,
        request: MutationRequest<AgentRuntimeOptionProbeRequest>,
    ) -> BackendFuture<'_, AgentRuntimeOptionProbeResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let result = runtime
                .agent()
                .runtime_catalog()
                .probe_agent(&request.payload.agent_id)
                .await?;
            Ok(AgentRuntimeOptionProbeResult {
                probed_agent_ids: result.probed_agent_ids,
                failed_agent_ids: result.failed_agent_ids,
                cached_agent_ids: result.cached_agent_ids,
            })
        })
    }

    fn list_agent_auth_contexts(&self) -> BackendFuture<'_, Vec<AgentAuthContext>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.agent().list_auth_contexts().map_err(Into::into)
        })
    }

    fn ensure_default_agent_auth_context(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentAuthContext> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .ensure_default_auth_context(&request.payload)
                .map_err(Into::into)
        })
    }

    fn refresh_agent_auth_methods(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentAuthCatalog> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .refresh_auth_methods(request.payload, None)
                .await
                .map_err(Into::into)
        })
    }

    fn list_agent_auth_methods(&self, agent_id: AgentId) -> BackendFuture<'_, AgentAuthCatalog> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .list_auth_methods(agent_id, None)
                .await
                .map_err(Into::into)
        })
    }

    fn logout_agent(&self, request: MutationRequest<AgentLogoutRequest>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .logout(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn discover_agent_owned_model_catalog(
        &self,
        request: AgentModelOwnedCatalogRequest,
    ) -> BackendFuture<'_, Vec<ProviderConfiguredModel>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .runtime_catalog()
                .discover_agent_owned_model_catalog(&request.agent_id, &request.provider_profile_id)
                .await
                .map_err(Into::into)
        })
    }

    fn discover_agent_commands(
        &self,
        request: AgentCommandDiscoverRequest,
    ) -> BackendFuture<'_, AgentCommandDiscovery> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            vibex_desktop_runtime::discover_composer_commands(
                &runtime.agent(),
                &runtime.files(),
                &runtime.providers(),
                Some(&runtime.vibex_use()),
                request,
            )
            .await
            .map_err(BackendError::from)
        })
    }

    fn session_token_usage(
        &self,
        session_id: VibexSessionId,
    ) -> BackendFuture<'_, Option<vibex_core::AgentTokenUsage>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent_token_usage_snapshot(&session_id)
                .map_err(Into::into)
        })
    }

    fn execute_agent_command(
        &self,
        request: MutationRequest<AgentCommandExecuteRequest>,
    ) -> BackendFuture<'_, AgentCommandExecuteResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .manager()
                .execute_command(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn agent_message_submission(
        &self,
        request: GetMessageSubmissionRequest,
    ) -> BackendFuture<'_, MessageSubmissionState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .message_submission()
                .get_submission(&request)
                .map_err(Into::into)
        })
    }

    fn authenticate_agent(
        &self,
        request: MutationRequest<AgentAuthenticateRequest>,
    ) -> BackendFuture<'_, AgentAuthenticateResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .authenticate(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn cancel_agent_authentication(
        &self,
        request: MutationRequest<AgentAuthenticationCancelRequest>,
    ) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .cancel_authentication(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn update_agent_auth_environment(
        &self,
        request: MutationRequest<AgentAuthEnvironmentUpdateRequest>,
    ) -> BackendFuture<'_, ProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_agent_auth_environment(request.payload)
                .map_err(Into::into)
        })
    }

    fn authenticate_agent_context(
        &self,
        request: MutationRequest<AgentAuthContextAuthenticateRequest>,
    ) -> BackendFuture<'_, AgentAuthContextAuthenticateResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .authenticate_context(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn get_agent_authentication_operation(
        &self,
        operation_id: AgentAuthenticationOperationId,
    ) -> BackendFuture<'_, AgentAuthenticationOperation> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .authentication_operation(&operation_id)
                .map_err(Into::into)
        })
    }

    fn cancel_agent_context_authentication(
        &self,
        request: MutationRequest<AgentAuthContextCancelAuthenticationRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .cancel_context_authentication(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn verify_agent_auth_context(
        &self,
        request: MutationRequest<AgentAuthContextVerifyRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .verify_auth_context(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn refresh_agent_auth_models(
        &self,
        request: MutationRequest<AgentAuthContextRefreshModelsRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .refresh_auth_context_models(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn preview_agent_auth_logout(
        &self,
        auth_context_id: AgentAuthContextId,
    ) -> BackendFuture<'_, AgentAuthContextLogoutPreview> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .preview_auth_context_logout(&auth_context_id)
                .map_err(Into::into)
        })
    }

    fn logout_agent_auth_context(
        &self,
        request: MutationRequest<AgentAuthContextLogoutRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .logout_auth_context(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn runtime_selection(
        &self,
        session_id: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .runtime_selection()
                .get_selection_state(&session_id)
                .map_err(Into::into)
        })
    }

    fn set_desired_runtime(
        &self,
        request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .runtime_selection()
                .set_desired_runtime(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn cancel_runtime_switch(
        &self,
        request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .runtime_selection()
                .cancel_switch(request.payload)
                .await
                .map_err(Into::into)
        })
    }
}

/// A backend error for a sidebar change the authority would not make.
///
/// The stable code matches the one the Desktop's remote service answers with,
/// so a client recognises both without a second vocabulary.
/// Builds the gateway surface the Config Center renders.
///
/// The gateway owns no Provider list, so both the upstream summary and the
/// model list are read from the enabled route groups rather than from settings.
fn read_gateway_snapshot(
    runtime: &vibex_desktop_runtime::DesktopRuntime,
) -> vibex_core::VibexResult<GatewaySnapshot> {
    let service = runtime.model_gateway();
    let provider = runtime.providers().service();
    let settings = service.settings().map_err(vibex_core::VibexError::from)?;
    let runtime_state = service.runtime_state();
    let profiles = provider.list_model_provider_profiles()?;

    let mut upstreams = Vec::new();
    for upstream_id in provider.gateway_route_members().unwrap_or_default() {
        let Some(profile) = profiles.iter().find(|profile| profile.id == upstream_id) else {
            continue;
        };
        let keys = profile
            .credentials
            .iter()
            .map(|credential| vibex_core::GatewayKeySummary {
                credential_id: credential.id.clone(),
                display_name: credential.display_label(),
                masked_hint: credential.masked_hint.clone(),
                state: vibex_core::GatewayKeyState::Ready,
                used_tokens: 0,
                limits: settings.policy.account_limits.get(&credential.id).copied(),
                models: credential.models.clone(),
                proxy_url: credential.proxy_url.clone(),
            })
            .collect::<Vec<_>>();
        upstreams.push(vibex_core::GatewayUpstreamSummary {
            model_provider_profile_id: upstream_id.clone(),
            display_name: profile.display_name.clone(),
            model_count: profile
                .configured_models
                .iter()
                .filter(|model| model.enabled)
                .count(),
            keys,
        });
    }

    let models = provider
        .gateway_model_catalog()
        .unwrap_or_default()
        .into_iter()
        .map(|model| vibex_core::GatewayModelSummary {
            model_provider_profile_id: vibex_core::ModelProviderProfileId::new(),
            provider_name: model.display_name.clone(),
            model_id: model.model.clone(),
            display_id: model.display_id(),
            // The gateway decides what protocol to call a Provider; the
            // published entry keeps the capability namespace empty rather than
            // inventing one.
            wire_protocol_id: String::new(),
            route_id: model.route_id.clone(),
            source_count: model.source_count,
            model_count: model.model_count,
        })
        .collect::<Vec<_>>();

    Ok(GatewaySnapshot {
        settings,
        runtime: runtime_state,
        upstreams,
        models,
    })
}

/// Reads the gateway model catalogue as display ids.
fn gateway_catalog(
    provider: &vibex_config_switch::ProviderConfigService,
) -> vibex_core::VibexResult<Vec<String>> {
    Ok(provider
        .gateway_model_catalog()?
        .into_iter()
        .map(|model| model.display_id())
        .collect())
}

/// Re-publishes the gateway catalogue into every bound Agent's hidden Profile.
///
/// The Profile is where a session reads its selectable models, and it is a copy
/// of the catalogue rather than a live view. A route write therefore reaches a
/// session only once this runs: without it a group exists in the database, is
/// bound to its Agents, and still never appears in the model picker, because the
/// Profile it would be selected through was last written before the group did.
///
/// A runtime with no bound listener has nothing to publish, so its hidden
/// Profiles are removed instead — the same rule the gateway switch follows.
fn refresh_gateway_catalog(
    runtime: &vibex_desktop_runtime::DesktopRuntime,
) -> vibex_core::VibexResult<()> {
    let service = runtime.model_gateway();
    let provider = runtime.providers().service();
    service.invalidate_snapshot();
    match service.base_url() {
        Some(base_url) => {
            provider.refresh_gateway_profiles(&base_url, &gateway_catalog(&provider)?)
        }
        None => provider.deactivate_gateway_profiles(),
    }
    .map(|_| ())
}

fn sidebar_mutation_error(code: &'static str, message: &'static str) -> BackendError {
    BackendError::failed(code, message)
}

fn sidebar_state_error(error: vibex_desktop_model::UiStateError) -> BackendError {
    BackendError::failed("sidebar_organization_unreadable", error.to_string())
}

impl SidebarBackend for NativeBackend {
    fn sidebar_organization(&self) -> BackendFuture<'_, RemoteSidebarOrganizationSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            // The arrangement belongs to the Desktop shell, which is not
            // running here: this process holds the home lock, so the file the
            // shell writes is the user's arrangement and nobody is editing it
            // behind our back.
            let store = UiStateStore::new(runtime.ui_state_path());
            let state = store.load_read_only().map_err(sidebar_state_error)?;
            Ok(SidebarOrganizationView::from_desktop_ui_state(&state.state).to_remote())
        })
    }

    fn mutate_sidebar_organization(
        &self,
        mutation: RemoteSidebarOrganizationMutation,
        expected_revision: Option<u64>,
    ) -> BackendFuture<'_, RemoteSidebarOrganizationSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let store = UiStateStore::new(runtime.ui_state_path());
            let mut state = store.load_read_only().map_err(sidebar_state_error)?.state;
            let mut view = SidebarOrganizationView::from_desktop_ui_state(&state);
            if expected_revision.is_some_and(|revision| revision != view.revision) {
                return Err(sidebar_mutation_error(
                    "remote_sidebar_organization_stale_revision",
                    "the sidebar changed since this client rendered it",
                ));
            }
            // Folding only changes UI-state flags. Listing sessions also
            // scans their timelines for recency, so it must not delay a fold
            // just to build membership maps the mutation never reads. A send
            // time is the same kind of change: it names the queue entry and
            // nothing about the tree.
            let sessions = match &mutation {
                RemoteSidebarOrganizationMutation::SetFolderCollapsed { .. }
                | RemoteSidebarOrganizationMutation::SetProjectCollapsed { .. }
                | RemoteSidebarOrganizationMutation::SetWorkspaceCollapsed { .. }
                | RemoteSidebarOrganizationMutation::SetGroupCollapsed { .. }
                | RemoteSidebarOrganizationMutation::SetScheduledSend { .. }
                | RemoteSidebarOrganizationMutation::SetQueuedSend { .. }
                | RemoteSidebarOrganizationMutation::RemoveQueuedSend { .. }
                | RemoteSidebarOrganizationMutation::SetQueuePaused { .. }
                | RemoteSidebarOrganizationMutation::SetQueueManual { .. }
                | RemoteSidebarOrganizationMutation::EnqueueQueuedSend { .. }
                | RemoteSidebarOrganizationMutation::MoveQueuedSend { .. }
                | RemoteSidebarOrganizationMutation::SendQueuedSendNow { .. } => Vec::new(),
                _ => runtime.agent().list_sessions(false).await?,
            };
            let session_projects = sessions
                .iter()
                .map(|session| {
                    (
                        session.id.as_str().to_string(),
                        session.project_id.as_str().to_string(),
                    )
                })
                .collect();
            let session_workspaces = sessions
                .iter()
                .map(|session| {
                    (
                        session.id.as_str().to_string(),
                        session.workspace_id.as_str().to_string(),
                    )
                })
                .collect();
            let folder_id = vibex_core::RequestId::new().to_string();
            let group_id = vibex_core::RequestId::new().to_string();
            match view.apply_remote(
                &mutation,
                &session_projects,
                &session_workspaces,
                &folder_id,
                &group_id,
            ) {
                // A deadline lands in the queue the file holds rather than in
                // the tree, so it is written where it lives. Nobody is running
                // the clock that would release it here: the shell that owns the
                // queue keeps its own, and this is the arrangement it will read
                // when it comes back.
                Ok(SidebarMutationOutcome::Applied(effect)) => {
                    if effect.scheduled
                        && let RemoteSidebarOrganizationMutation::SetScheduledSend {
                            session_id,
                            message_id,
                            scheduled_at_ms,
                        } = &mutation
                    {
                        vibex_desktop_model::set_queued_send_schedule(
                            &mut state.composer.queue,
                            session_id,
                            *message_id,
                            *scheduled_at_ms,
                        );
                    }
                }
                Ok(SidebarMutationOutcome::AlreadyApplied) => {}
                Err(rejection) => {
                    return Err(sidebar_mutation_error(
                        rejection.stable_code(),
                        rejection.message(),
                    ));
                }
            }
            // A removal is the one queue change that cannot be expressed by
            // rewriting the published entries: the entry is gone, so writing it
            // back with edited fields would resurrect the message the reader
            // deleted. Pauses are live state and have no persisted form; the
            // send mode is written back by `apply_to_desktop_ui_state` from the
            // published set, which is where a shell that took the home over can
            // find it.
            match &mutation {
                RemoteSidebarOrganizationMutation::RemoveQueuedSend {
                    session_id,
                    message_id,
                } => {
                    state.composer.queue.retain(|entry| {
                        !(entry.session_id == *session_id && entry.id == *message_id)
                    });
                }
                // A message a client handed over has to be written where this
                // shell's queue is read from, or the next launch would lose it
                // and the reader would have written a message nothing sends.
                RemoteSidebarOrganizationMutation::EnqueueQueuedSend {
                    session_id,
                    text,
                    attachments,
                    mentions,
                } => {
                    // The runtime selection is the one thing a client cannot
                    // supply, so it comes from the shell's own default. Without
                    // one there is nothing to send the message with, and a
                    // half-written entry would fail at dispatch time instead of
                    // here.
                    if let Some(desired_runtime) = state.composer.default_runtime_selection.clone()
                    {
                        let id = state
                            .composer
                            .queue
                            .iter()
                            .map(|entry| entry.id)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1);
                        state
                            .composer
                            .queue
                            .push(vibex_desktop_model::ComposerQueueEntry {
                                id,
                                session_id: session_id.clone(),
                                desired_runtime,
                                text: text.clone(),
                                attachments: attachments.clone(),
                                mentions: mentions.clone(),
                                command: None,
                                scheduled_at_ms: None,
                                // Minted with the entry, so a hand-back that
                                // replays this message is the same submission
                                // instead of a second one.
                                dispatch_key: Some(format!(
                                    "queued:{}",
                                    vibex_core::RequestId::new().as_str()
                                )),
                            });
                    }
                }
                RemoteSidebarOrganizationMutation::MoveQueuedSend {
                    session_id,
                    message_id,
                    before_message_id,
                } => {
                    vibex_desktop_model::move_queued_send_relative(
                        &mut state.composer.queue,
                        session_id,
                        *message_id,
                        *before_message_id,
                    );
                }
                _ => {}
            }
            view.apply_to_desktop_ui_state(&mut state);
            store.save(&state).map_err(sidebar_state_error)?;
            // Answer with the arrangement as it now reads, not with the one
            // this call happened to hold: saving normalizes, and a snapshot
            // carrying a pre-normalization revision would be refused as stale
            // by the very next change — a reader would have to press twice for
            // every collapse.
            let saved = store.load_read_only().map_err(sidebar_state_error)?;
            Ok(SidebarOrganizationView::from_desktop_ui_state(&saved.state).to_remote())
        })
    }
}

impl WorkspaceBackend for NativeBackend {
    fn list_workspaces(&self) -> BackendFuture<'_, Vec<WorkspaceSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .workspace()
                .list()
                .map(|workspaces| {
                    workspaces
                        .into_iter()
                        .map(|(project, workspace)| WorkspaceSummary {
                            project,
                            workspace,
                            git_branch: None,
                        })
                        .collect()
                })
                .map_err(Into::into)
        })
    }

    fn open_workspace(
        &self,
        request: MutationRequest<OpenWorkspaceRequest>,
    ) -> BackendFuture<'_, WorkspaceSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .workspace()
                .open(&request.payload)
                .map(|(project, workspace)| WorkspaceSummary {
                    project,
                    workspace,
                    git_branch: None,
                })
                .map_err(Into::into)
        })
    }

    fn get_workspace(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, WorkspaceSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .workspace()
                .get(&workspace_id)
                .map(|(project, workspace)| WorkspaceSummary {
                    project,
                    workspace,
                    git_branch: None,
                })
                .map_err(Into::into)
        })
    }

    fn delete_workspace(&self, request: MutationRequest<WorkspaceId>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .workspace()
                .delete_workspace(&request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_project(&self, request: MutationRequest<ProjectId>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .workspace()
                .delete_project(&request.payload)
                .map_err(Into::into)
        })
    }

    fn ensure_temporary_session_root(&self) -> BackendFuture<'_, String> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            temporary_session_root().map_err(BackendError::from)
        })
    }
}

/// Creates and resolves the authority's temporary session root.
fn temporary_session_root() -> vibex_core::VibexResult<String> {
    let root = std::env::temp_dir().join("vibex").join("sessions");
    std::fs::create_dir_all(&root).map_err(|error| {
        vibex_core::VibexError::storage(
            "temporary_workspace_create_failed",
            "failed to create temporary session workspace",
        )
        .with_diagnostic("path", root.display().to_string())
        .with_diagnostic("error", error.to_string())
    })?;
    root.canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| {
            vibex_core::VibexError::storage(
                "temporary_workspace_canonicalize_failed",
                "failed to resolve temporary session workspace",
            )
            .with_diagnostic("path", root.display().to_string())
            .with_diagnostic("error", error.to_string())
        })
}

impl FileBackend for NativeBackend {
    fn resolve_local_path(
        &self,
        workspace_id: WorkspaceId,
        path: String,
    ) -> BackendFuture<'_, Option<std::path::PathBuf>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .files()
                .resolve_existing_path(&workspace_id, &path)
                .map(Some)
                .map_err(Into::into)
        })
    }

    fn file_tree(&self, request: FileTreeRequest) -> BackendFuture<'_, Vec<FileTreeEntry>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.files().list_tree(&request).map_err(Into::into)
        })
    }

    fn search_files(&self, request: FileSearchRequest) -> BackendFuture<'_, Vec<FileSearchResult>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.files().search(&request).map_err(Into::into)
        })
    }

    fn read_file(&self, request: FileReadRequest) -> BackendFuture<'_, FileReadResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.files().read(&request).map_err(Into::into)
        })
    }

    fn read_file_bytes(
        &self,
        workspace_id: WorkspaceId,
        path: String,
        max_bytes: usize,
    ) -> BackendFuture<'_, Vec<u8>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .files()
                .read_bytes(&workspace_id, &path, max_bytes)
                .map_err(Into::into)
        })
    }

    fn write_file(
        &self,
        request: MutationRequest<FileWriteRequest>,
    ) -> BackendFuture<'_, FileReadResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.files().write(&request.payload).map_err(Into::into)
        })
    }

    fn create_directory(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .files()
                .create_directory(&request.payload)
                .map_err(Into::into)
        })
    }

    fn copy_path(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.files().copy(&request.payload).map_err(Into::into)
        })
    }

    fn rename_path(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.files().rename(&request.payload).map_err(Into::into)
        })
    }

    fn delete_path(&self, request: MutationRequest<FileMutationRequest>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.files().delete(&request.payload).map_err(Into::into)
        })
    }
}

impl GitBackend for NativeBackend {
    fn git_status(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, GitStatusSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let git = runtime.git();
            // `status` spawns `git status` and waits for it; on an async worker
            // that blocks every other task sharing the thread.
            tokio::task::spawn_blocking(move || git.status(&workspace_id))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "git_status_task_failed",
                        "the git status task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn git_diff(&self, request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.git().diff(&request).map_err(Into::into)
        })
    }

    fn git_worktree_eligibility(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitProjectEligibility> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .project_git_eligibility(&workspace_id)
                .map_err(Into::into)
        })
    }

    fn git_worktree_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitWorktreeLifecycleSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_snapshot(&workspace_id)
                .map_err(Into::into)
        })
    }

    fn git_worktree_create(
        &self,
        request: MutationRequest<GitWorktreeCreateRequest>,
    ) -> BackendFuture<'_, GitWorktreeCreateResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            if let Some(expected_revision) = request.expected_revision.as_deref() {
                let eligibility = runtime
                    .git()
                    .project_git_eligibility(&request.payload.workspace_id)?;
                if eligibility.revision != expected_revision {
                    return Err(BackendError::conflict(
                        "worktree_eligibility_stale",
                        "project Git eligibility changed before worktree creation",
                    ));
                }
            }
            let context = WorktreeCreateContext::new(request.request_id, request.idempotency_key);
            runtime
                .git()
                .worktree_create_with_context(&request.payload, context)
                .map_err(Into::into)
        })
    }

    fn git_worktree_readiness(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, Option<GitWorktreeReadinessRecord>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_readiness(&workspace_id)
                .map_err(Into::into)
        })
    }

    fn git_worktree_set_readiness(
        &self,
        request: MutationRequest<GitWorktreeReadinessRequest>,
    ) -> BackendFuture<'_, GitWorktreeReadinessRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_set_readiness(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_merge_plan(
        &self,
        request: GitWorktreeMergeRequest,
    ) -> BackendFuture<'_, GitWorktreeMergePlan> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_merge_plan(&request)
                .map_err(Into::into)
        })
    }

    fn git_worktree_merge(
        &self,
        request: MutationRequest<GitWorktreeMergeRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_merge(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_resolve_conflict(
        &self,
        request: MutationRequest<GitWorktreeConflictResolveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_resolve_conflict(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_stage_conflicts(
        &self,
        request: MutationRequest<GitWorktreeConflictStageRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_stage_conflicts(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_bind_assistance_session(
        &self,
        request: MutationRequest<GitWorktreeAssistanceSessionRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_bind_assistance_session(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_continue_merge(
        &self,
        request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_continue_merge(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_abort_merge(
        &self,
        request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_abort_merge(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_archive_preflight(
        &self,
        request: GitWorktreeArchiveRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_archive_preflight(&request)
                .map_err(Into::into)
        })
    }

    fn git_worktree_archive(
        &self,
        request: MutationRequest<GitWorktreeArchiveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_archive(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_restore_preflight(
        &self,
        request: GitWorktreeRestoreRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_restore_preflight(&request)
                .map_err(Into::into)
        })
    }

    fn git_worktree_restore(
        &self,
        request: MutationRequest<GitWorktreeRestoreRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_restore(&request.payload)
                .map_err(Into::into)
        })
    }

    fn git_worktree_discard_preflight(
        &self,
        request: GitWorktreeDiscardRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_discard_preflight(&request)
                .map_err(Into::into)
        })
    }

    fn git_worktree_discard(
        &self,
        request: MutationRequest<GitWorktreeDiscardRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_discard(&request.payload)
                .map_err(Into::into)
        })
    }

    fn stage(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.git().stage(&request.payload).map_err(Into::into)
        })
    }

    fn unstage(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.git().unstage(&request.payload).map_err(Into::into)
        })
    }

    fn commit(
        &self,
        request: MutationRequest<GitCommitRequest>,
    ) -> BackendFuture<'_, GitCommitResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.git().commit(&request.payload).map_err(Into::into)
        })
    }

    fn git_history(&self, request: GitHistoryRequest) -> BackendFuture<'_, GitHistoryResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.git().history(&request).map_err(Into::into)
        })
    }

    fn git_commit_detail(
        &self,
        request: GitCommitDetailRequest,
    ) -> BackendFuture<'_, GitCommitDetail> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.git().commit_detail(&request).map_err(Into::into)
        })
    }

    fn git_branch_list(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitBranchListResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let git = runtime.git();
            // Listing branches runs `git rev-list` per branch; keep it off the
            // async workers.
            tokio::task::spawn_blocking(move || git.branch_list(&workspace_id))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "git_branch_list_task_failed",
                        "the git branch list task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn git_worktree_rename_branch(
        &self,
        workspace_id: WorkspaceId,
        new_branch: String,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .worktree_rename_branch(&workspace_id, &new_branch)
                .map_err(Into::into)
        })
    }

    fn git_revert(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime.git().revert(&request.payload).map_err(Into::into)
        })
    }

    fn git_remote_action(
        &self,
        request: MutationRequest<GitRemoteActionRequest>,
    ) -> BackendFuture<'_, GitRemoteActionResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .git()
                .remote_action(&request.payload)
                .map_err(Into::into)
        })
    }
}

struct NativeTerminalSubscription {
    terminals: TerminalHandle,
    terminal_id: TerminalId,
    next_sequence: i64,
    last_dropped_frames: u64,
}

impl TerminalFrameSubscription for NativeTerminalSubscription {
    fn next(&mut self) -> BackendFuture<'_, Option<TerminalFrameBatch>> {
        Box::pin(async move {
            let mut reset_required = false;
            loop {
                let requested_sequence = self.next_sequence;
                let snapshot = self
                    .terminals
                    .manager()
                    .raw_snapshot_from(&self.terminal_id, requested_sequence)
                    .map_err(BackendError::from)?;
                let first_sequence = snapshot.chunks.first().map(|chunk| chunk.sequence);
                reset_required |= terminal_frame_reset_required(
                    requested_sequence,
                    snapshot.next_sequence,
                    first_sequence,
                    snapshot.dropped_chunks,
                    self.last_dropped_frames,
                );
                let frames = snapshot
                    .chunks
                    .into_iter()
                    .map(|chunk| TerminalFrame {
                        sequence: chunk.sequence,
                        bytes: chunk.data,
                    })
                    .collect::<Vec<_>>();
                self.next_sequence = snapshot.next_sequence;
                self.last_dropped_frames = snapshot.dropped_chunks;
                if !frames.is_empty() {
                    return Ok(Some(TerminalFrameBatch {
                        terminal_id: self.terminal_id.clone(),
                        frames,
                        next_sequence: self.next_sequence,
                        dropped_frames: self.last_dropped_frames,
                        reset_required,
                    }));
                }
                if snapshot.session.status != TerminalStatus::Running {
                    return Ok(None);
                }
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        })
    }
}

fn terminal_frame_reset_required(
    requested_sequence: i64,
    server_next_sequence: i64,
    first_sequence: Option<i64>,
    dropped_frames: u64,
    previous_dropped_frames: u64,
) -> bool {
    requested_sequence > server_next_sequence
        || first_sequence.is_some_and(|sequence| sequence > requested_sequence)
        || dropped_frames > previous_dropped_frames
}

impl TerminalBackend for NativeBackend {
    fn list_terminals(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, Vec<TerminalSession>> {
        let runtime = self.runtime.clone();
        Box::pin(async move { runtime.list_terminals(&workspace_id).map_err(Into::into) })
    }

    fn create_terminal(
        &self,
        request: MutationRequest<TerminalCreateRequest>,
    ) -> BackendFuture<'_, TerminalSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            let workspace = runtime.workspace().get(&request.payload.workspace_id)?.1;
            runtime
                .create_terminal(workspace.root_path, request.payload)
                .map_err(Into::into)
        })
    }

    fn terminal_snapshot(&self, terminal_id: TerminalId) -> BackendFuture<'_, TerminalSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .terminals()
                .manager()
                .snapshot(&terminal_id)
                .map_err(Into::into)
        })
    }

    fn subscribe_terminal(
        &self,
        terminal_id: TerminalId,
        next_sequence: i64,
    ) -> BackendResult<Box<dyn TerminalFrameSubscription>> {
        if next_sequence < 1 {
            return Err(BackendError::failed(
                "terminal_frame_sequence_invalid",
                "terminal frame sequence must be positive",
            ));
        }
        self.runtime.ensure_accepting_actions()?;
        Ok(Box::new(NativeTerminalSubscription {
            terminals: self.runtime.terminals(),
            terminal_id,
            next_sequence,
            last_dropped_frames: 0,
        }))
    }

    fn write_terminal(
        &self,
        request: MutationRequest<TerminalWriteRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .terminals()
                .manager()
                .write(&request.payload)
                .map_err(Into::into)
        })
    }

    fn resize_terminal(
        &self,
        request: MutationRequest<TerminalResizeRequest>,
    ) -> BackendFuture<'_, TerminalSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .terminals()
                .manager()
                .resize(&request.payload)
                .map_err(Into::into)
        })
    }

    fn close_terminal(
        &self,
        request: MutationRequest<TerminalId>,
    ) -> BackendFuture<'_, TerminalSession> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.kill_terminal(&request.payload).map_err(Into::into)
        })
    }
}

/// Frame subscription for the embedded browser.
///
/// This seam is not wired to `DesktopRuntime::browser()` yet, so the type is not
/// handed out until that wiring lands; it is kept here so the seam already has
/// the concrete subscription it will return. Once constructed it ends
/// immediately instead of blocking, because a tab that is not screencasting has
/// no frames to wait for.
pub struct NativeBrowserSubscription {
    runtime: Arc<DesktopRuntime>,
    tab_id: BrowserTabId,
    next_sequence: u64,
    /// Held only while frames are being taken. The frame pump is cancelled by
    /// dropping it, which is the same idiom the terminal subscription uses.
    subscription: Option<vibex_browser::BrowserFrameSubscription>,
    dropped_frames: u64,
}

impl NativeBrowserSubscription {
    /// The tab this subscription streams frames for.
    pub fn tab_id(&self) -> &BrowserTabId {
        &self.tab_id
    }
}

impl BrowserFrameSubscription for NativeBrowserSubscription {
    fn next(&mut self) -> BackendFuture<'_, Option<BrowserFrameBatch>> {
        Box::pin(async move {
            if self.subscription.is_none() {
                let service = self.runtime.browser().service().clone();
                // The backend's frame subscription has no quality channel yet;
                // the desktop's local panel does not come through here.
                self.subscription = Some(
                    service
                        .subscribe_frames(&self.tab_id, BrowserCaptureQuality::Standard)
                        .await?,
                );
            }
            let Some(subscription) = self.subscription.as_mut() else {
                return Ok(None);
            };
            // Latest-value semantics: the caller always receives the freshest
            // frame, and frames that arrived while it was busy are counted as
            // dropped rather than queued. A client that fell behind therefore
            // has to resynchronise, which `reset_required` reports.
            let Some(frame) = subscription.next().await else {
                self.subscription = None;
                return Ok(None);
            };
            let reset_required =
                frame.sequence > self.next_sequence.saturating_add(1) && self.next_sequence != 0;
            self.next_sequence = frame.sequence;
            self.dropped_frames = subscription.dropped_frames();
            Ok(Some(BrowserFrameBatch {
                tab_id: self.tab_id.clone(),
                next_sequence: frame.sequence.saturating_add(1),
                dropped_frames: self.dropped_frames,
                reset_required,
                frame: Some(frame),
            }))
        })
    }
}

/// Browser seam for the desktop runtime.
///
/// The runtime owns the browser process, the CDP connection and the ledger;
/// this block only forwards. Every method goes through `DesktopRuntime::browser`
/// so the panel, the backend facade and the remote gateway all see one
/// authority.
impl BrowserBackend for NativeBackend {
    fn browser_availability(&self) -> BackendFuture<'_, BrowserAvailability> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.browser().service().availability().await)
        })
    }

    fn list_browser_sessions(&self) -> BackendFuture<'_, Vec<BrowserSession>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.browser().service().sessions().await)
        })
    }

    fn browser_session_snapshot(
        &self,
        session_id: BrowserSessionId,
    ) -> BackendFuture<'_, BrowserSessionSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .browser()
                .service()
                .session_snapshot(&session_id)
                .await
                .map_err(Into::into)
        })
    }

    fn browser_ledger(
        &self,
        session_id: BrowserSessionId,
    ) -> BackendFuture<'_, Vec<BrowserActionRecord>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.browser().service().ledger(&session_id).await)
        })
    }

    fn ensure_browser_session(
        &self,
        request: MutationRequest<BrowserSessionOpenRequest>,
    ) -> BackendFuture<'_, BrowserSessionId> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let payload = request.payload;
            // The key is the isolation boundary: an Agent session and a
            // workspace panel never share tabs by accident.
            let key = match (&payload.agent_session_id, &payload.workspace_id) {
                (Some(agent_session_id), _) => {
                    vibex_browser::BrowserSessionKey::Agent(agent_session_id.clone())
                }
                (None, Some(workspace_id)) => {
                    vibex_browser::BrowserSessionKey::Workspace(workspace_id.clone())
                }
                (None, None) => vibex_browser::BrowserSessionKey::Anonymous,
            };
            runtime
                .browser()
                .service()
                .ensure_session(key, payload.workspace_id)
                .await
                .map_err(Into::into)
        })
    }

    fn create_browser_tab(
        &self,
        request: MutationRequest<BrowserTabOpenRequest>,
    ) -> BackendFuture<'_, BrowserTab> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let payload = request.payload;
            let service = runtime.browser().service().clone();
            let tab_id = service
                .create_tab(
                    &payload.session_id,
                    payload.url.as_deref(),
                    vibex_core::BrowserTabOwner::User,
                )
                .await?;
            let snapshot = service.session_snapshot(&payload.session_id).await?;
            snapshot
                .session
                .tabs
                .into_iter()
                .find(|tab| tab.tab_id == tab_id)
                .ok_or_else(|| {
                    BackendError::failed(
                        "browser_tab_missing",
                        "the browser tab vanished immediately after it was created",
                    )
                })
        })
    }

    fn close_browser_tab(&self, request: MutationRequest<BrowserTabId>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .browser()
                .service()
                .close_tab(&request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn select_browser_tab(
        &self,
        request: MutationRequest<BrowserTabSelection>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let payload = request.payload;
            runtime
                .browser()
                .service()
                .select_tab(&payload.session_id, &payload.tab_id)
                .await
                .map_err(Into::into)
        })
    }

    fn subscribe_browser_frames(
        &self,
        tab_id: BrowserTabId,
        next_sequence: u64,
    ) -> BackendResult<Box<dyn BrowserFrameSubscription>> {
        Ok(Box::new(NativeBrowserSubscription {
            runtime: self.runtime.clone(),
            tab_id,
            next_sequence,
            subscription: None,
            dropped_frames: 0,
        }))
    }

    fn set_browser_viewport(
        &self,
        request: MutationRequest<BrowserViewportRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let payload = request.payload;
            runtime
                .browser()
                .service()
                .set_viewport(
                    &payload.tab_id,
                    payload.width,
                    payload.height,
                    payload.device_scale_factor,
                )
                .await
                .map_err(Into::into)
        })
    }

    fn send_browser_input(
        &self,
        request: MutationRequest<BrowserInputRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let payload = request.payload;
            let input = crate::browser::payload_to_browser_input(&payload.input);
            runtime
                .browser()
                .service()
                .dispatch_input(&payload.tab_id, input)
                .await
                .map_err(Into::into)
        })
    }

    fn resolve_browser_dialog(
        &self,
        request: MutationRequest<BrowserDialogResolution>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let payload = request.payload;
            runtime
                .browser()
                .service()
                .handle_dialog(
                    &payload.tab_id,
                    payload.accept,
                    payload.prompt_text.as_deref(),
                )
                .await
                .map_err(Into::into)
        })
    }

    fn stop_browser_screencast(&self, tab_id: BrowserTabId) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.browser().service().stop_screencast(&tab_id).await;
            Ok(())
        })
    }
}

impl ManagementBackend for NativeBackend {
    fn list_agents(&self, request: AgentListRequest) -> BackendFuture<'_, AgentListResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_agents(request)
                .map_err(Into::into)
        })
    }

    fn refresh_agent_snapshot(
        &self,
        request: AgentRefreshSnapshotRequest,
    ) -> BackendFuture<'_, AgentRefreshSnapshotResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .refresh_agent_snapshot(request)
                .map_err(Into::into)
        })
    }

    fn install_managed_agent(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .install_managed_agent(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn rollback_managed_agent(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .rollback_managed_agent(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn check_managed_agent_update(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .check_managed_agent_update(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn uninstall_managed_agent(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .uninstall_managed_agent(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn delete_agent_auth_catalog(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .delete_auth_catalog(&request.payload)
                .map_err(Into::into)
        })
    }

    fn update_agent_config(
        &self,
        request: MutationRequest<AgentUpdateConfigRequest>,
    ) -> BackendFuture<'_, AgentSnapshotEntry> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_agent_config(request.payload)
                .map_err(Into::into)
        })
    }

    fn create_custom_agent(
        &self,
        request: MutationRequest<vibex_core::CustomAgentCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentSnapshotEntry> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_custom_agent(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_custom_agent(
        &self,
        request: MutationRequest<vibex_core::CustomAgentDeleteRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_custom_agent(request.payload)
                .map_err(Into::into)
        })
    }

    fn list_profiles(&self) -> BackendFuture<'_, Vec<vibex_core::ProviderProfileSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_profiles()
                .map(|profiles| {
                    profiles
                        .into_iter()
                        .map(|profile| profile.summary())
                        .collect()
                })
                .map_err(Into::into)
        })
    }

    fn agent_model_provider_profile_detail(
        &self,
        request: MutationRequest<vibex_core::AgentModelProviderProfileDetailRequest>,
    ) -> BackendFuture<'_, vibex_core::ProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_profile(&request.payload.provider_profile_id)?
                .ok_or_else(|| {
                    vibex_core::VibexError::validation(
                        "provider_profile_not_found",
                        "the provider profile was not found",
                    )
                    .into()
                })
        })
    }

    fn select_profile(
        &self,
        request: MutationRequest<ManagementProfileSelectionRequest>,
    ) -> BackendFuture<'_, vibex_core::ProviderProfileSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let selection = runtime
                .management()
                .providers()
                .management()
                .set_agent_model_provider_default(vibex_core::AgentModelProviderSetDefaultRequest {
                    scope: request.payload.scope.unwrap_or(
                        vibex_core::ProviderProfileDefaultScope {
                            kind: vibex_core::ProviderDefaultScopeKind::Global,
                            project_id: None,
                            workspace_id: None,
                        },
                    ),
                    agent_id: request.payload.agent_id,
                    provider_profile_id: request.payload.provider_profile_id,
                })
                .map_err(BackendError::from)?;
            let profile_id = selection.provider_profile_id.ok_or_else(|| {
                BackendError::failed(
                    "management_profile_selection_empty",
                    "provider selection returned no active profile",
                )
            })?;
            let profile = runtime
                .management()
                .providers()
                .management()
                .get_profile(&profile_id)
                .map_err(BackendError::from)?
                .ok_or_else(|| {
                    BackendError::failed(
                        "management_profile_missing_after_select",
                        "selected provider profile was not found after the mutation",
                    )
                })?;
            Ok(profile.summary())
        })
    }

    fn list_model_provider_profiles(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::ModelProviderProfile>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                // The page lists Providers a reader created. The store also holds
                // the per-kind local defaults, the per-Agent ACP launch configs and
                // the gateway's own Profile; they must exist but are not Suppliers,
                // and drawing them made the page look like it contained entries
                // nobody added.
                .list_visible_model_provider_profiles()
                .map_err(Into::into)
        })
    }

    fn list_model_routes(&self) -> BackendFuture<'_, Vec<vibex_core::ModelRoute>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .service()
                .list_model_routes()
                .map_err(Into::into)
        })
    }

    fn list_all_agent_model_route_bindings(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentModelRouteBinding>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .service()
                .list_all_agent_model_route_bindings()
                .map_err(Into::into)
        })
    }

    fn create_model_route(
        &self,
        request: MutationRequest<vibex_core::ModelRoute>,
    ) -> BackendFuture<'_, vibex_core::ModelRoute> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let route = runtime
                .management()
                .providers()
                .service()
                .create_model_route(request.payload)?;
            // A new group is invisible to a session until the gateway Profile it
            // is selected through lists it, so the catalogue is republished now
            // rather than on the next launch or gateway toggle.
            refresh_gateway_catalog(&runtime)?;
            Ok(route)
        })
    }

    fn update_model_route(
        &self,
        request: MutationRequest<vibex_core::ModelRouteUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelRoute> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let vibex_core::ModelRouteUpdateRequest {
                route,
                expected_revision,
            } = request.payload;
            let route = runtime
                .management()
                .providers()
                .service()
                .update_model_route(route, expected_revision)?;
            // Editing a group's Providers changes which models it offers, and
            // the Profile copy has to follow or a session would offer the old
            // intersection.
            refresh_gateway_catalog(&runtime)?;
            Ok(route)
        })
    }

    fn delete_model_route(
        &self,
        request: MutationRequest<vibex_core::ModelRouteDeleteRequest>,
    ) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let deleted = runtime
                .management()
                .providers()
                .service()
                .delete_model_route(&request.payload.model_route_id)?;
            // The Profile must stop offering a route that no longer exists;
            // otherwise the picker lists a model the gateway would refuse.
            refresh_gateway_catalog(&runtime)?;
            Ok(deleted)
        })
    }

    fn bind_agent_model_route(
        &self,
        request: MutationRequest<vibex_core::AgentModelRouteBindRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelRouteBinding> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let vibex_core::AgentModelRouteBindRequest {
                agent_id,
                model_route_id,
                make_default,
            } = request.payload;
            let binding = runtime
                .management()
                .providers()
                .service()
                .bind_model_route(&agent_id, &model_route_id, make_default)?;
            // Binding is what makes a route this Agent's, and the Profile is
            // per Agent: this is the write that puts it in that Agent's picker.
            refresh_gateway_catalog(&runtime)?;
            Ok(binding)
        })
    }

    fn unbind_agent_model_route(
        &self,
        request: MutationRequest<vibex_core::AgentModelRouteUnbindRequest>,
    ) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let vibex_core::AgentModelRouteUnbindRequest {
                agent_id,
                model_route_id,
            } = request.payload;
            let removed = runtime
                .management()
                .providers()
                .service()
                .unbind_model_route(&agent_id, &model_route_id)?;
            // The Agent no longer references it, so its Profile must drop the
            // entry too. Leaving it would offer a model the Agent may not use.
            refresh_gateway_catalog(&runtime)?;
            Ok(removed)
        })
    }

    fn set_agent_default_model_route(
        &self,
        request: MutationRequest<vibex_core::AgentModelRouteDefaultRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let vibex_core::AgentModelRouteDefaultRequest {
                agent_id,
                model_route_id,
            } = request.payload;
            runtime
                .management()
                .providers()
                .service()
                .set_agent_default_model_route(&agent_id, &model_route_id)
                .map_err(Into::into)
        })
    }

    fn get_agent_model_source_default(
        &self,
        request: vibex_core::AgentModelSourceDefaultRequest,
    ) -> BackendFuture<'_, vibex_core::AgentModelSourceDefault> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_agent_model_source_default(request)
                .map_err(Into::into)
        })
    }

    fn set_agent_model_source_default(
        &self,
        request: MutationRequest<vibex_core::SetAgentModelSourceDefaultRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelSourceDefault> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .set_agent_model_source_default(request.payload)
                .map_err(Into::into)
        })
    }

    fn clear_agent_model_source_default(
        &self,
        request: MutationRequest<vibex_core::ClearAgentModelSourceDefaultRequest>,
    ) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .clear_agent_model_source_default(request.payload)
                .map_err(Into::into)
        })
    }

    fn model_capability_suggestion(
        &self,
        request: ModelCapabilitySuggestionRequest,
    ) -> BackendFuture<'_, Option<GatewayCapabilitySuggestion>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime
                .model_catalog()
                .suggest(
                    &request.provider_id,
                    &request.model_id,
                    vibex_core::unix_timestamp_ms(),
                )
                .map(|suggestion| GatewayCapabilitySuggestion {
                    approximate: suggestion.source.is_approximate(),
                    fields: suggestion
                        .fields()
                        .into_iter()
                        .map(|(field, value)| GatewayCapabilityField {
                            field: field.to_string(),
                            value,
                        })
                        .collect(),
                }))
        })
    }

    fn model_canonical_ids(
        &self,
        request: vibex_core::ModelCanonicalIdsRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::ModelCanonicalId>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let state = runtime
                .model_catalog()
                .load(vibex_core::unix_timestamp_ms());
            let catalog = state.catalog();
            Ok(request
                .models
                .into_iter()
                .map(|lookup| vibex_core::ModelCanonicalId {
                    canonical_model_id: catalog.and_then(|catalog| {
                        catalog.canonical_model_id(&lookup.provider_id, &lookup.model_id)
                    }),
                    provider_id: lookup.provider_id,
                    model_id: lookup.model_id,
                })
                .collect())
        })
    }

    fn model_catalog_status(&self) -> BackendFuture<'_, vibex_core::ModelCatalogStatus> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.model_catalog_status())
        })
    }

    fn refresh_model_catalog(&self) -> BackendFuture<'_, vibex_core::ModelCatalogStatus> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.refresh_model_catalog().await.map_err(Into::into)
        })
    }

    fn model_gateway_snapshot(&self) -> BackendFuture<'_, GatewaySnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            read_gateway_snapshot(&runtime).map_err(Into::into)
        })
    }

    fn update_model_gateway_settings(
        &self,
        request: MutationRequest<GatewaySettingsUpdateRequest>,
    ) -> BackendFuture<'_, GatewaySnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            request.validate()?;
            let payload = request.payload;
            let service = runtime.model_gateway();
            let provider = runtime.providers().service();
            let current = vibex_db::ModelGatewaySettingsRepository::get_or_create(
                &vibex_db::open_database(service.database_path())?,
            )?;
            // A Key command names a Provider and a Key; both have to be real
            // and still named by an enabled group before anything is written,
            // so a typo cannot create a state row for a Key that does not
            // exist. The group list is the gateway's only upstream set.
            let profiles = provider.list_model_provider_profiles()?;
            let route_members = provider.gateway_route_members().unwrap_or_default();
            for update in &payload.account_updates {
                update.validate()?;
                if !route_members.contains(&update.model_provider_profile_id) {
                    return Err(vibex_core::VibexError::validation(
                        "model_gateway_account_upstream_unknown",
                        "the account update names a Provider that no enabled route group uses",
                    )
                    .into());
                }
                let profile = profiles
                    .iter()
                    .find(|profile| profile.id == update.model_provider_profile_id)
                    .ok_or_else(|| {
                        vibex_core::VibexError::validation(
                            "model_gateway_account_provider_unknown",
                            "the account update names a Provider that does not exist",
                        )
                    })?;
                if !profile
                    .credentials
                    .iter()
                    .any(|credential| credential.id == update.credential_id)
                {
                    return Err(vibex_core::VibexError::validation(
                        "model_gateway_account_key_unknown",
                        "the account update names a Key that this Provider does not have",
                    )
                    .into());
                }
            }
            let mut policy = current.policy.clone();
            policy.token_limit = payload.token_limit;
            policy.account_limits = payload.account_limits;
            policy.translate = payload.translate;
            for update in &payload.account_updates {
                match update.preferred {
                    Some(true) => {
                        policy.preferred_keys.insert(
                            update.model_provider_profile_id.clone(),
                            update.credential_id.clone(),
                        );
                    }
                    Some(false)
                        if policy.preferred_keys.get(&update.model_provider_profile_id)
                            == Some(&update.credential_id) =>
                    {
                        policy
                            .preferred_keys
                            .remove(&update.model_provider_profile_id);
                    }
                    // The guard covers clearing this Key's preference; a
                    // different Key's preference is left alone.
                    Some(false) | None => {}
                }
            }
            policy.validate()?;
            let mut next = current.clone();
            next.display_name = payload.display_name;
            next.policy = policy;
            next.revision = payload.expected_revision.saturating_add(1);
            let mut connection = vibex_db::open_database(service.database_path())?;
            vibex_db::apply_migrations(&mut connection)?;
            vibex_db::ModelGatewaySettingsRepository::update(
                &connection,
                &next,
                payload.expected_revision,
            )?;
            // Enabled/disabled is per-Key durable state rather than policy, so
            // it is written beside the settings that carried the command.
            let now = vibex_core::unix_timestamp_ms();
            for update in &payload.account_updates {
                if update.enabled.is_some() {
                    vibex_db::ModelProviderAccountStateRepository::apply_update(
                        &connection,
                        update,
                        now,
                    )?;
                }
            }
            service.invalidate_snapshot();
            // A listener that cannot follow the policy is reported through the
            // runtime state, which the Config Center already renders; the write
            // itself still succeeded.
            let _ = service.apply_policy().await;
            match service.base_url() {
                Some(base_url) => {
                    provider.refresh_gateway_profiles(&base_url, &gateway_catalog(&provider)?)?;
                }
                None => {
                    provider.deactivate_gateway_profiles()?;
                }
            }
            read_gateway_snapshot(&runtime).map_err(Into::into)
        })
    }

    fn add_model_provider_credential(
        &self,
        request: MutationRequest<vibex_core::AddModelProviderCredentialRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            request.validate()?;
            let profile = runtime
                .providers()
                .service()
                .add_model_provider_credential(request.payload)?;
            runtime.model_gateway().invalidate_snapshot();
            Ok(profile)
        })
    }

    fn remove_model_provider_credential(
        &self,
        request: MutationRequest<vibex_core::RemoveModelProviderCredentialRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            request.validate()?;
            let profile = runtime
                .providers()
                .service()
                .remove_model_provider_credential(request.payload)?;
            runtime.model_gateway().invalidate_snapshot();
            Ok(profile)
        })
    }

    fn set_model_gateway_enabled(&self, enabled: bool) -> BackendFuture<'_, GatewaySnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.model_gateway();
            let provider = runtime.providers().service();
            let mut connection = vibex_db::open_database(service.database_path())?;
            vibex_db::apply_migrations(&mut connection)?;
            let current = vibex_db::ModelGatewaySettingsRepository::get_or_create(&connection)?;
            let mut next = current.clone();
            next.policy.enabled = enabled;
            next.revision = current.revision.saturating_add(1);
            next.policy.validate()?;
            vibex_db::ModelGatewaySettingsRepository::update(&connection, &next, current.revision)?;
            service.invalidate_snapshot();
            let _ = service.apply_policy().await;
            // An Agent bound to a stopped gateway must be told, and an Agent
            // whose gateway came back must be re-pointed: the address is the
            // same either way, so this only reports what changed.
            match service.base_url() {
                Some(base_url) => {
                    let catalog = provider
                        .gateway_model_catalog()
                        .map(|catalog| {
                            catalog
                                .into_iter()
                                .map(|model| model.display_id())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    provider.refresh_gateway_profiles(&base_url, &catalog)?;
                }
                None => {
                    provider.deactivate_gateway_profiles()?;
                }
            }
            read_gateway_snapshot(&runtime).map_err(Into::into)
        })
    }

    fn set_model_gateway_agent(
        &self,
        _request: MutationRequest<GatewayAgentUpdateRequest>,
    ) -> BackendFuture<'_, GatewaySnapshot> {
        // An Agent reaches the gateway by binding a route group, so there is no
        // per-Agent gateway switch to set. This entry point survives only so an
        // older client gets a refusal it can read instead of an unknown-variant
        // decode failure.
        Box::pin(async {
            Err(vibex_core::VibexError::validation(
                "model_gateway_agent_scope_removed",
                "bind a route group to the Agent instead of switching the gateway per Agent",
            )
            .into())
        })
    }

    fn answer_model_gateway_failover(
        &self,
        prompt_id: vibex_core::RequestId,
        answer: vibex_core::GatewayFailoverAnswer,
    ) -> BackendFuture<'_, bool> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            // A `false` here is not a failure: the prompt's window closed while
            // the click was in flight, and the request already took the default.
            Ok(runtime.model_gateway().answer_failover(&prompt_id, answer))
        })
    }

    fn model_gateway_requests(&self) -> BackendFuture<'_, Vec<GatewayRequestSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.model_gateway();
            let logs = service
                .recent_requests(50)
                .map_err(vibex_core::VibexError::from)?;
            let profiles = runtime
                .providers()
                .service()
                .list_model_provider_profiles()?;
            Ok(logs
                .into_iter()
                .map(|log| GatewayRequestSummary::from_log(log, &profiles))
                .collect())
        })
    }

    fn model_gateway_inflight(&self) -> BackendFuture<'_, Vec<vibex_core::GatewayInflight>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            // A live read, not a database query: in-flight state exists only
            // while the request does.
            Ok(runtime.model_gateway().inflight_requests())
        })
    }

    fn delete_model_provider_profile(
        &self,
        request: MutationRequest<vibex_core::ModelProviderProfileId>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_model_provider_profile(&request.payload)
                .map_err(Into::into)
        })
    }

    fn clear_model_gateway_requests(&self) -> BackendFuture<'_, usize> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .model_gateway()
                .clear_requests()
                .map_err(vibex_core::VibexError::from)
                .map_err(Into::into)
        })
    }

    fn create_model_provider_profile(
        &self,
        request: MutationRequest<vibex_core::ModelProviderProfileCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_model_provider_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_model_provider_profile(
        &self,
        request: MutationRequest<vibex_core::ModelProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_model_provider_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn list_agent_runtime_profiles(
        &self,
        agent_id: vibex_core::AgentId,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProfile>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_agent_runtime_profiles(&agent_id)
                .map_err(Into::into)
        })
    }

    fn create_agent_runtime_profile(
        &self,
        request: MutationRequest<vibex_core::AgentRuntimeProfileCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_agent_runtime_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_agent_runtime_profile(
        &self,
        request: MutationRequest<vibex_core::AgentRuntimeProfileUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_agent_runtime_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn list_agent_model_provider_bindings(
        &self,
        request: vibex_core::AgentModelProviderBindingListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentModelProviderBinding>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_agent_model_provider_bindings(request)
                .map_err(Into::into)
        })
    }

    fn create_agent_model_provider_binding(
        &self,
        request: MutationRequest<vibex_core::AgentModelProviderBindingCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_agent_model_provider_binding(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_agent_model_provider_binding(
        &self,
        request: MutationRequest<vibex_core::AgentModelProviderBindingUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_agent_model_provider_binding(request.payload)
                .map_err(Into::into)
        })
    }

    fn agent_provider_projection_capability(
        &self,
        request: vibex_core::AgentProviderProjectionCapabilityRequest,
    ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionCapability> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .agent_provider_projection_capability(request)
                .map_err(Into::into)
        })
    }

    fn preview_agent_provider_projection(
        &self,
        request: vibex_core::AgentProviderProjectionPreviewRequest,
    ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionPreview> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .preview_agent_provider_projection(request)
                .map_err(Into::into)
        })
    }

    fn start_agent_runtime_probe(
        &self,
        request: MutationRequest<vibex_core::AgentRuntimeProbeStartRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProbeRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .start_agent_runtime_probe(request.payload)
                .map_err(Into::into)
        })
    }

    fn get_agent_runtime_probe(
        &self,
        probe_id: vibex_core::AgentRuntimeProbeId,
    ) -> BackendFuture<'_, Option<vibex_core::AgentRuntimeProbeRecord>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_agent_runtime_probe(&probe_id)
                .map_err(Into::into)
        })
    }

    fn list_agent_runtime_probes(
        &self,
        request: vibex_core::AgentRuntimeProbeListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProbeRecord>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_agent_runtime_probes(request)
                .map_err(Into::into)
        })
    }

    fn cancel_agent_runtime_probe(
        &self,
        request: MutationRequest<vibex_core::AgentRuntimeProbeCancelRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProbeRecord> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .cancel_agent_runtime_probe(request.payload)
                .map_err(Into::into)
        })
    }

    fn mutate_provider_credential_secret(
        &self,
        request: MutationRequest<vibex_core::ProviderCredentialSecretMutationRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .mutate_provider_credential_secret(request.payload)
                .map_err(Into::into)
        })
    }

    fn get_model_provider_credential_secret(
        &self,
        request: vibex_core::ModelProviderCredentialSecretRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderCredentialSecretResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_model_provider_credential_secret(request)
                .map_err(Into::into)
        })
    }

    fn health_summaries(&self) -> BackendFuture<'_, Vec<ProviderHealthSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_health_summaries()
                .map_err(Into::into)
        })
    }

    fn run_health_probes(
        &self,
        request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunHealthProbesResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .run_health_probes(request.payload)
                .map_err(Into::into)
        })
    }

    fn mutate_agent_model_provider_profile_secret(
        &self,
        request: MutationRequest<AgentModelProviderProfileSecretValueUpdateRequest>,
    ) -> BackendFuture<'_, AgentModelProviderProfileSecretValueResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_agent_model_provider_profile_secret_value(request.payload)
                .map_err(Into::into)
        })
    }

    fn get_agent_model_provider_profile_secret_value(
        &self,
        request: AgentModelProviderProfileSecretValueRequest,
    ) -> BackendFuture<'_, AgentModelProviderProfileSecretValueResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_agent_model_provider_profile_secret_value(request)
                .map_err(Into::into)
        })
    }

    fn create_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileCreateRequest>,
    ) -> BackendFuture<'_, ProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_agent_model_provider_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, ProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_agent_model_provider_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileDeleteRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_agent_model_provider_profile(request.payload)
                .map_err(Into::into)
        })
    }

    fn bind_agent_model_provider(
        &self,
        request: MutationRequest<vibex_core::AgentModelProviderBindRequest>,
    ) -> BackendFuture<'_, vibex_core::ProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .bind_provider_to_agent(request.payload)
                .map_err(Into::into)
        })
    }

    fn agent_model_provider_display_order(
        &self,
        request: AgentModelProviderDisplayOrderListRequest,
    ) -> BackendFuture<'_, AgentModelProviderDisplayOrderListResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_agent_model_provider_display_order(request)
                .map_err(Into::into)
        })
    }

    fn set_agent_model_provider_display_order(
        &self,
        request: MutationRequest<AgentModelProviderDisplayOrderSetRequest>,
    ) -> BackendFuture<'_, AgentModelProviderDisplayOrderSetResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .set_agent_model_provider_display_order(request.payload)
                .map_err(Into::into)
        })
    }

    fn test_agent_model_provider_profile(
        &self,
        request: AgentModelProviderProfileTestRequest,
    ) -> BackendFuture<'_, AgentModelProviderProfileTestResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .test_agent_model_provider_profile(request)
                .map_err(Into::into)
        })
    }

    fn fetch_agent_model_provider_profile_models(
        &self,
        request: AgentModelProviderProfileFetchModelsRequest,
    ) -> BackendFuture<'_, AgentModelProviderProfileFetchModelsResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .fetch_agent_model_provider_profile_models(request)
                .map_err(Into::into)
        })
    }

    fn test_model_provider_credential(
        &self,
        request: vibex_core::ModelProviderKeyTestRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderTestResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .test_model_provider_credential(request)
                .map_err(Into::into)
        })
    }

    fn test_model_provider_proxy(
        &self,
        request: vibex_core::ModelProviderProxyTestRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderTestResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .test_model_provider_proxy(request)
                .map_err(Into::into)
        })
    }

    fn probe_model_provider_endpoints(
        &self,
        request: vibex_core::ModelProviderEndpointProbeRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderEndpointProbeResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .probe_model_provider_endpoints(request)
                .map_err(Into::into)
        })
    }

    fn fetch_model_provider_profile_models(
        &self,
        request: vibex_core::ModelProviderFetchModelsRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderFetchModelsResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .fetch_model_provider_profile_models(request)
                .map_err(Into::into)
        })
    }

    fn test_model_provider_profile(
        &self,
        request: vibex_core::ModelProviderTestRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderTestResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .test_model_provider_profile(request)
                .map_err(Into::into)
        })
    }

    fn fetch_agent_model_provider_draft_models(
        &self,
        request: AgentModelProviderDraftFetchModelsRequest,
    ) -> BackendFuture<'_, AgentModelProviderDraftFetchModelsResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .fetch_agent_model_provider_draft_models(request)
                .map_err(Into::into)
        })
    }

    fn capability_summaries(&self) -> BackendFuture<'_, Vec<ProviderCapabilitySummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_capability_summaries()
                .map_err(Into::into)
        })
    }

    fn run_capability_probes(
        &self,
        request: MutationRequest<ProviderRunCapabilityProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunCapabilityProbesResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .run_capability_probes(request.payload)
                .map_err(Into::into)
        })
    }

    fn mcp_servers(&self) -> BackendFuture<'_, Vec<McpServer>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_mcp_servers()
                .map_err(Into::into)
        })
    }

    fn create_mcp_server(
        &self,
        request: MutationRequest<McpServerCreateRequest>,
    ) -> BackendFuture<'_, McpServer> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_mcp_server(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_mcp_server(
        &self,
        request: MutationRequest<McpServerUpdateRequest>,
    ) -> BackendFuture<'_, McpServer> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_mcp_server(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_mcp_server(
        &self,
        request: MutationRequest<McpServerDeleteRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_mcp_server(request.payload)
                .map_err(Into::into)
        })
    }

    fn set_mcp_server_agent_matrix(
        &self,
        request: MutationRequest<McpServerSetAgentMatrixRequest>,
    ) -> BackendFuture<'_, McpServer> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .set_mcp_server_agent_matrix(request.payload)
                .map_err(Into::into)
        })
    }

    fn mcp_server_agent_matrix(
        &self,
        request: McpServerAgentMatrixListRequest,
    ) -> BackendFuture<'_, Vec<McpServerAgentMatrix>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_mcp_server_agent_matrix(request)
                .map_err(Into::into)
        })
    }

    fn discover_mcp_sources(
        &self,
        request: McpServerDiscoverRequest,
    ) -> BackendFuture<'_, McpServerDiscoveryResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .discover_mcp_sources(request)
                .map_err(Into::into)
        })
    }

    fn import_mcp_servers(
        &self,
        request: MutationRequest<McpServerImportRequest>,
    ) -> BackendFuture<'_, McpServerImportResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .import_mcp_servers(request.payload)
                .map_err(Into::into)
        })
    }

    fn validate_mcp_server(
        &self,
        request: McpServerValidateRequest,
    ) -> BackendFuture<'_, McpServerValidationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .validate_mcp_server(request)
                .map_err(Into::into)
        })
    }

    fn search_mcp_market(
        &self,
        request: McpMarketSearchRequest,
    ) -> BackendFuture<'_, McpMarketSearchResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.management().providers().management();
            // A catalog fetch is blocking I/O; running it on the async worker
            // would stall every other task sharing the thread.
            tokio::task::spawn_blocking(move || service.search_mcp_market(request))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "market_search_task_failed",
                        "the market search task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn install_mcp_market_entry(
        &self,
        request: MutationRequest<McpMarketInstallRequest>,
    ) -> BackendFuture<'_, McpMarketInstallResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.management().providers().management();
            tokio::task::spawn_blocking(move || service.install_mcp_market_entry(request.payload))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "market_install_task_failed",
                        "the market install task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn skills(&self) -> BackendFuture<'_, Vec<Skill>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_skills()
                .map_err(Into::into)
        })
    }

    fn create_skill(
        &self,
        request: MutationRequest<SkillCreateRequest>,
    ) -> BackendFuture<'_, Skill> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_skill(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_skill(
        &self,
        request: MutationRequest<SkillUpdateRequest>,
    ) -> BackendFuture<'_, Skill> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_skill(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_skill(&self, request: MutationRequest<SkillDeleteRequest>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_skill(request.payload)
                .map_err(Into::into)
        })
    }

    fn set_skill_agent_matrix(
        &self,
        request: MutationRequest<SkillSetAgentMatrixRequest>,
    ) -> BackendFuture<'_, Skill> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .set_skill_agent_matrix(request.payload)
                .map_err(Into::into)
        })
    }

    fn skill_agent_matrix(
        &self,
        request: SkillAgentMatrixListRequest,
    ) -> BackendFuture<'_, Vec<SkillAgentMatrix>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_skill_agent_matrix(request)
                .map_err(Into::into)
        })
    }

    fn discover_skill_sources(
        &self,
        request: SkillDiscoverRequest,
    ) -> BackendFuture<'_, SkillDiscoveryResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .discover_skill_sources(request)
                .map_err(Into::into)
        })
    }

    fn import_skills(
        &self,
        request: MutationRequest<SkillImportRequest>,
    ) -> BackendFuture<'_, SkillImportResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .import_skills(request.payload)
                .map_err(Into::into)
        })
    }

    fn validate_skill(
        &self,
        request: SkillValidateRequest,
    ) -> BackendFuture<'_, SkillValidationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .validate_skill(request)
                .map_err(Into::into)
        })
    }

    fn search_skill_market(
        &self,
        request: SkillMarketSearchRequest,
    ) -> BackendFuture<'_, SkillMarketSearchResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.management().providers().management();
            tokio::task::spawn_blocking(move || service.search_skill_market(request))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "market_search_task_failed",
                        "the market search task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn skill_market_document(
        &self,
        request: SkillMarketDocumentRequest,
    ) -> BackendFuture<'_, SkillMarketDocument> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.management().providers().management();
            tokio::task::spawn_blocking(move || service.skill_market_document(request))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "market_document_task_failed",
                        "the market document task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn install_skill_market_entry(
        &self,
        request: MutationRequest<SkillMarketInstallRequest>,
    ) -> BackendFuture<'_, SkillMarketInstallResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let service = runtime.management().providers().management();
            tokio::task::spawn_blocking(move || service.install_skill_market_entry(request.payload))
                .await
                .map_err(|_| {
                    BackendError::failed(
                        "market_install_task_failed",
                        "the market install task did not complete",
                    )
                })?
                .map_err(Into::into)
        })
    }

    fn prompts(&self) -> BackendFuture<'_, Vec<Prompt>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_prompts()
                .map_err(Into::into)
        })
    }

    fn create_prompt(
        &self,
        request: MutationRequest<PromptCreateRequest>,
    ) -> BackendFuture<'_, Prompt> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_prompt(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_prompt(
        &self,
        request: MutationRequest<PromptUpdateRequest>,
    ) -> BackendFuture<'_, Prompt> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_prompt(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_prompt(
        &self,
        request: MutationRequest<PromptDeleteRequest>,
    ) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_prompt(request.payload)
                .map_err(Into::into)
        })
    }

    fn validate_prompt(
        &self,
        request: PromptValidateRequest,
    ) -> BackendFuture<'_, PromptValidationResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .validate_prompt(request)
                .map_err(Into::into)
        })
    }

    fn record_prompt_usage(
        &self,
        request: MutationRequest<PromptUsageRecordRequest>,
    ) -> BackendFuture<'_, PromptUsage> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .record_prompt_usage(request.payload)
                .map_err(Into::into)
        })
    }

    fn hooks(&self) -> BackendFuture<'_, Vec<Hook>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_hooks()
                .map_err(Into::into)
        })
    }

    fn create_hook(&self, request: MutationRequest<HookCreateRequest>) -> BackendFuture<'_, Hook> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_hook(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_hook(&self, request: MutationRequest<HookUpdateRequest>) -> BackendFuture<'_, Hook> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_hook(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_hook(&self, request: MutationRequest<HookDeleteRequest>) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .delete_hook(request.payload)
                .map_err(Into::into)
        })
    }

    fn preview_hook_install(
        &self,
        request: HookInstallPreviewRequest,
    ) -> BackendFuture<'_, HookInstallPreview> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .preview_hook_install(request)
                .map_err(Into::into)
        })
    }

    fn scheduled_tasks(
        &self,
        request: ScheduledTaskListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::ScheduledTask>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .list(request)
                .map_err(Into::into)
        })
    }

    fn create_scheduled_task(
        &self,
        request: MutationRequest<ScheduledTaskCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::ScheduledTask> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .create(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_scheduled_task(
        &self,
        request: MutationRequest<ScheduledTaskUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ScheduledTask> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .update(request.payload)
                .map_err(Into::into)
        })
    }

    fn set_scheduled_task_status(
        &self,
        task_id: ScheduledTaskId,
        paused: bool,
    ) -> BackendFuture<'_, vibex_core::ScheduledTask> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            if paused {
                runtime.management().scheduled().pause(&task_id)
            } else {
                runtime.management().scheduled().resume(&task_id)
            }
            .map_err(Into::into)
        })
    }

    fn delete_scheduled_task(&self, task_id: ScheduledTaskId) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .delete(&task_id)
                .map(|_| ())
                .map_err(Into::into)
        })
    }

    fn scheduled_task_runs(
        &self,
        request: ScheduledTaskRunListRequest,
    ) -> BackendFuture<'_, Vec<ScheduledTaskRun>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .list_runs(request)
                .map_err(Into::into)
        })
    }

    fn scheduled_task_attention(
        &self,
        request: ScheduledTaskAttentionListRequest,
    ) -> BackendFuture<'_, Vec<ScheduledTaskAttentionSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .list_attention(request)
                .map_err(Into::into)
        })
    }

    fn scheduled_task_audit(
        &self,
        request: ScheduledTaskAuditListRequest,
    ) -> BackendFuture<'_, Vec<ScheduledTaskAuditRecord>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .list_audit(request)
                .map_err(Into::into)
        })
    }

    fn claim_due_scheduled_task(
        &self,
        task_id: ScheduledTaskId,
        now_ms: i64,
    ) -> BackendFuture<'_, Option<ScheduledTaskRun>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .scheduled()
                .claim_due(&task_id, now_ms)
                .map_err(Into::into)
        })
    }

    fn automation_graphs(
        &self,
        request: AutomationGraphListRequest,
    ) -> BackendFuture<'_, Vec<AutomationGraph>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .list(request)
                .map_err(Into::into)
        })
    }

    fn create_automation_graph(
        &self,
        request: MutationRequest<AutomationGraphCreateRequest>,
    ) -> BackendFuture<'_, AutomationGraph> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .create(request.payload)
                .map_err(Into::into)
        })
    }

    fn update_automation_graph(
        &self,
        request: MutationRequest<AutomationGraphUpdateRequest>,
    ) -> BackendFuture<'_, AutomationGraph> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .update(request.payload)
                .map_err(Into::into)
        })
    }

    fn replace_automation_definition(
        &self,
        request: MutationRequest<AutomationGraphDefinitionUpdateRequest>,
    ) -> BackendFuture<'_, AutomationGraph> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .replace_definition(request.payload)
                .map_err(Into::into)
        })
    }

    fn set_automation_graph_status(
        &self,
        graph_id: AutomationGraphId,
        status: AutomationGraphStatus,
    ) -> BackendFuture<'_, AutomationGraph> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let handle = runtime.management().automation();
            if status == vibex_core::AutomationGraphStatus::Paused {
                handle.pause(&graph_id)
            } else {
                handle.resume(&graph_id)
            }
            .map_err(Into::into)
        })
    }

    fn archive_automation_graph(
        &self,
        graph_id: AutomationGraphId,
    ) -> BackendFuture<'_, AutomationGraph> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .archive(&graph_id)
                .map_err(Into::into)
        })
    }

    fn automation_runs(
        &self,
        request: AutomationRunListRequest,
    ) -> BackendFuture<'_, Vec<AutomationRun>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .list_runs(request)
                .map_err(Into::into)
        })
    }

    fn automation_run_steps(
        &self,
        request: AutomationRunStepListRequest,
    ) -> BackendFuture<'_, Vec<AutomationRunStep>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .list_steps(request)
                .map_err(Into::into)
        })
    }

    fn start_automation_run(
        &self,
        request: MutationRequest<AutomationRunStartRequest>,
    ) -> BackendFuture<'_, AutomationRun> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .start_run(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn resume_automation_run(
        &self,
        request: MutationRequest<AutomationRunResumeRequest>,
    ) -> BackendFuture<'_, AutomationRun> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .resume_run(request.payload)
                .await
                .map_err(Into::into)
        })
    }

    fn cancel_automation_run(
        &self,
        request: MutationRequest<AutomationRunCancelRequest>,
    ) -> BackendFuture<'_, AutomationRun> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .automation()
                .cancel_run(request.payload)
                .map_err(Into::into)
        })
    }

    fn refresh_detected_agent_versions(&self) -> BackendFuture<'_, usize> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .refresh_detected_agent_versions()
                .map_err(Into::into)
        })
    }

    fn agent_catalog(&self) -> BackendFuture<'_, AgentCatalogListResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_agent_catalog()
                .map_err(Into::into)
        })
    }

    fn acp_catalog_presets(&self) -> BackendFuture<'_, AcpProviderCatalogListResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_acp_catalog_presets()
                .map_err(Into::into)
        })
    }

    fn acp_profile_config(
        &self,
        provider_profile_id: ProviderProfileId,
    ) -> BackendFuture<'_, AcpProviderConfig> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .get_acp_profile_config_for_launch(provider_profile_id)
                .map_err(Into::into)
        })
    }

    fn update_acp_profile_config(
        &self,
        request: MutationRequest<AcpProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, ProviderProfile> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .update_acp_profile_config(request.payload)
                .map_err(Into::into)
        })
    }

    fn preview_native_import(
        &self,
        request: ProviderNativeImportPreviewRequest,
    ) -> BackendFuture<'_, ProviderNativeImportPreview> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .preview_native_import(request)
                .map_err(Into::into)
        })
    }

    fn create_profile_from_import(
        &self,
        request: MutationRequest<ProviderNativeImportCreateRequest>,
    ) -> BackendFuture<'_, ProviderNativeImportCreateResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .create_profile_from_import(request.payload)
                .map_err(Into::into)
        })
    }

    fn preview_native_export(
        &self,
        request: ProviderNativeExportPreviewRequest,
    ) -> BackendFuture<'_, ProviderNativeExportPreview> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .preview_native_export(request)
                .map_err(Into::into)
        })
    }

    fn apply_native_export(
        &self,
        request: MutationRequest<ProviderNativeExportApplyRequest>,
    ) -> BackendFuture<'_, ProviderNativeExportApplyResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .apply_native_export(request.payload)
                .map_err(Into::into)
        })
    }

    fn rollback_native_export(
        &self,
        request: MutationRequest<ProviderNativeExportRollbackRequest>,
    ) -> BackendFuture<'_, ProviderNativeExportRollbackResult> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .rollback_native_export(request.payload)
                .map_err(Into::into)
        })
    }

    fn native_exports(
        &self,
        request: ProviderNativeExportListRequest,
    ) -> BackendFuture<'_, Vec<ProviderNativeExportRecordSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_native_exports(request)
                .map_err(Into::into)
        })
    }

    fn usage_summaries(
        &self,
        request: ProviderUsageListRequest,
    ) -> BackendFuture<'_, Vec<ProviderUsageSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .providers()
                .management()
                .list_usage_summaries(request)
                .map_err(Into::into)
        })
    }

    fn management_snapshot(
        &self,
        request: ManagementSnapshotPayload,
    ) -> BackendFuture<'_, RemoteProviderManagementSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            vibex_desktop_runtime::assemble_management_snapshot(
                &runtime.management(),
                request.default_scope,
                request.refresh_agent_versions,
            )
            .await
            .map_err(Into::into)
        })
    }

    fn export_diagnostics(
        &self,
        request: MutationRequest<DiagnosticExportPayload>,
    ) -> BackendFuture<'_, DiagnosticExportOutcome> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            Ok(runtime.management().export_diagnostics(request.payload)?)
        })
    }

    fn backup_create(
        &self,
        request: MutationRequest<BackupCreatePayload>,
    ) -> BackendFuture<'_, BackupCreateOutcome> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            Ok(runtime.management().backup_create(request.payload)?)
        })
    }

    fn backup_inspect(
        &self,
        request: BackupInspectPayload,
    ) -> BackendFuture<'_, BackupInspectOutcome> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            Ok(runtime.management().backup_inspect(request)?)
        })
    }

    fn backup_restore(
        &self,
        request: MutationRequest<BackupRestorePayload>,
    ) -> BackendFuture<'_, BackupRestoreOutcome> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            Ok(runtime.management().backup_restore(request.payload)?)
        })
    }

    fn rc_import(
        &self,
        request: MutationRequest<RcImportPayload>,
    ) -> BackendFuture<'_, RcImportOutcome> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            Ok(runtime.management().rc_import(request.payload)?)
        })
    }

    fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            let status = runtime.management().relay().get_status().await;
            Ok(RelayStatusSummary {
                state: map_relay_state(status.state),
                room_id: status.room_id,
                pc_peer_id: status.pc_peer_id,
                pc_public_key: status.pc_public_key,
                reconnect_attempt: status.reconnect_attempt,
                next_retry_at_ms: status.next_retry_at_ms,
                last_error: status.last_error,
            })
        })
    }
}

fn map_relay_state(state: RelayClientConnectionState) -> RelayConnectionState {
    match state {
        RelayClientConnectionState::Disabled => RelayConnectionState::Disabled,
        RelayClientConnectionState::Disconnected => RelayConnectionState::Disconnected,
        RelayClientConnectionState::Connecting => RelayConnectionState::Connecting,
        RelayClientConnectionState::Connected => RelayConnectionState::Connected,
        RelayClientConnectionState::Retrying => RelayConnectionState::Retrying,
        RelayClientConnectionState::Degraded => RelayConnectionState::Degraded,
        RelayClientConnectionState::Error => RelayConnectionState::Error,
    }
}

impl DeviceBackend for NativeBackend {
    fn create_pairing_offer(
        &self,
        request: MutationRequest<RemoteCreatePairingCodeRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingCodeResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .create_pairing_code(request.payload)
                .map_err(Into::into)
        })
    }

    fn create_pairing_offer_v2(
        &self,
        request: MutationRequest<RemoteCreatePairingOfferRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingOfferResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .create_pairing_offer(request.payload)
                .map_err(Into::into)
        })
    }

    fn cancel_pairing_offer(
        &self,
        request: MutationRequest<vibex_core::RemoteCancelPairingOfferRequest>,
    ) -> BackendFuture<'_, vibex_core::RemotePairingOfferSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .cancel_pairing_offer(request.payload)
                .map_err(Into::into)
        })
    }

    fn start_lan_pairing(
        &self,
        request: MutationRequest<vibex_core::RemoteStartLanPairingRequest>,
    ) -> BackendFuture<'_, vibex_core::RemoteLanPairingWindowSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let ttl_ms = request
                .payload
                .ttl_ms
                .unwrap_or(vibex_core::DEFAULT_PAIRING_TTL_MS);
            // The controller, not the gateway, owns the window: it probes the
            // Direct route for mobile-compatible TLS and puts the mDNS
            // advertisement up, and it takes both back down on cancel.
            runtime
                .remote_connectivity()
                .start_lan_pairing_window(request.payload.permission_level, ttl_ms)
                .await
                .map_err(Into::into)
        })
    }

    fn start_zero_config_lan_pairing(
        &self,
        request: MutationRequest<vibex_core::RemoteStartLanPairingRequest>,
    ) -> BackendFuture<'_, vibex_core::RemoteLanPairingWindowSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            let ttl_ms = request
                .payload
                .ttl_ms
                .unwrap_or(vibex_core::DEFAULT_PAIRING_TTL_MS);
            runtime
                .remote_connectivity()
                .start_zero_config_lan_pairing(request.payload.permission_level, ttl_ms)
                .await
                .map_err(Into::into)
        })
    }

    fn lan_pairing_window(
        &self,
    ) -> BackendFuture<'_, Option<vibex_core::RemoteLanPairingWindowSnapshot>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            // The controller owns the rule about which bootstrap is up, so this
            // and the gateway source cannot disagree about the window.
            runtime
                .remote_connectivity()
                .active_lan_pairing_window()
                .map_err(Into::into)
        })
    }

    fn decide_lan_pairing(
        &self,
        request: MutationRequest<vibex_core::RemoteLanPairingDecision>,
    ) -> BackendFuture<'_, vibex_core::RemoteLanPairingWindowSnapshot> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .remote_connectivity()
                .decide_active_lan_pairing(&request.payload.request_id, request.payload.approve)
                .map_err(Into::into)
        })
    }

    fn cancel_lan_pairing(&self) -> BackendFuture<'_, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .remote_connectivity()
                .cancel_active_lan_pairing()
                .await
                .map_err(Into::into)
        })
    }

    fn list_devices(&self) -> BackendFuture<'_, Vec<RemoteDeviceDetail>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .list_devices()
                .map_err(Into::into)
        })
    }

    fn pairing_publications(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::RemotePairingPublicationSummary>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            // Read straight from the controller rather than through
            // `DevicePairing`: the whole point of this read is to answer when
            // there is no route to mint an offer over.
            let publications = runtime.remote_connectivity().pairing_publications().await;
            BackendResult::Ok(publications)
        })
    }

    fn pairing_offer_status(
        &self,
        request: vibex_core::RemotePairingOfferStatusRequest,
    ) -> BackendFuture<'_, vibex_core::RemotePairingOfferSummary> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .gateway()
                .pairing_offer_status(&request.offer_id)
                .map_err(Into::into)
        })
    }

    fn runtime_name(&self) -> BackendFuture<'_, String> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .runtime_display_name()
                .map_err(Into::into)
        })
    }

    fn revoke_device(
        &self,
        request: MutationRequest<RemoteRevokeDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .revoke_device(request.payload)
                .map_err(Into::into)
        })
    }

    fn restore_device(
        &self,
        request: MutationRequest<vibex_core::RemoteRestoreDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .restore_device(request.payload)
                .map_err(Into::into)
        })
    }

    fn delete_device(
        &self,
        request: MutationRequest<vibex_core::RemoteDeleteDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .delete_device(request.payload)
                .map_err(Into::into)
        })
    }

    fn rename_device(
        &self,
        request: MutationRequest<RemoteRenameDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .rename_device(request.payload)
                .map_err(Into::into)
        })
    }

    fn rename_runtime(&self, request: MutationRequest<String>) -> BackendFuture<'_, String> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            request.validate()?;
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .rename_runtime(request.payload.trim())
                .map_err(Into::into)
        })
    }

    fn audit_records(
        &self,
        request: RemoteAuditListRequest,
    ) -> BackendFuture<'_, Vec<RemoteAuditRecord>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .management()
                .remote()
                .list_audit(request)
                .map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_native_capabilities_match_implemented_domain_traits() {
        let snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        assert_eq!(snapshot.revision, 1);
        assert!(!snapshot.agent.operations.is_empty());
        assert!(!snapshot.workspace.operations.is_empty());
        assert!(!snapshot.file.operations.is_empty());
        assert!(!snapshot.git.operations.is_empty());
        assert!(!snapshot.terminal.operations.is_empty());
        assert!(!snapshot.management.operations.is_empty());
        assert!(!snapshot.device.operations.is_empty());
    }

    #[test]
    fn terminal_frame_subscription_requires_reset_for_eviction_drop_or_runtime_rewind() {
        assert!(terminal_frame_reset_required(4, 8, Some(6), 0, 0));
        assert!(terminal_frame_reset_required(4, 8, Some(4), 2, 1));
        assert!(terminal_frame_reset_required(9, 4, Some(1), 0, 0));
        assert!(!terminal_frame_reset_required(4, 8, Some(4), 1, 1));
    }
}

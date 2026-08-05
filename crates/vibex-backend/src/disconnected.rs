use std::sync::Arc;

use vibex_core::{
    AgentListRequest, AgentListResponse, AgentSession, AgentSessionRuntimeSelectionState,
    CancelAgentSessionRuntimeSwitchRequest, ContinueAgentTurnRequest, CreateAgentSessionRequest,
    FetchTimelineRequest, FileMutationRequest, FileReadRequest, FileReadResponse,
    FileSearchRequest, FileSearchResult, FileTreeEntry, FileTreeRequest, FileWriteRequest,
    GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse, GitProjectEligibility,
    GitStageRequest, GitStatusSummary, GitWorktreeArchiveRequest,
    GitWorktreeAssistanceSessionRequest, GitWorktreeConflictResolveRequest,
    GitWorktreeConflictStageRequest, GitWorktreeCreateRequest, GitWorktreeCreateResult,
    GitWorktreeDestructivePreflight, GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot,
    GitWorktreeMergePlan, GitWorktreeMergeRequest, GitWorktreeOperationRecord,
    GitWorktreeOperationRequest, GitWorktreeReadinessRecord, GitWorktreeReadinessRequest,
    GitWorktreeRestoreRequest, OpenWorkspaceRequest, ProjectId, ProviderHealthSummary,
    ProviderProfileSummary, ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult,
    RemoteAuditListRequest, RemoteAuditRecord, RemoteCancelPairingOfferRequest,
    RemoteCreatePairingCodeRequest, RemoteCreatePairingCodeResponse,
    RemoteCreatePairingOfferRequest, RemoteCreatePairingOfferResponse, RemoteDeviceDetail,
    RemotePairingOfferSummary, RemoteRevokeDeviceRequest, RenameAgentSessionRequest,
    ResolveElicitationRequest, ResolvePermissionRequest, SendAgentMessageRequest,
    SessionRuntimeOptionCatalog, SetDesiredAgentSessionRuntimeRequest, TerminalCreateRequest,
    TerminalId, TerminalResizeRequest, TerminalSession, TerminalSnapshot, TerminalWriteRequest,
    TimelineItem, TimelinePage, VibexSessionId, WorkspaceId,
};

use crate::{
    AgentBackend, BackendCapabilitySnapshot, BackendError, BackendEventSubscription, BackendFacade,
    BackendFuture, BackendResult, DeviceBackend, FileBackend, GitBackend, ManagementBackend,
    ManagementProfileSelectionRequest, MutationRequest, RelayStatusSummary, TerminalBackend,
    TerminalFrameSubscription, WorkspaceBackend, WorkspaceSummary,
};

macro_rules! disconnected_future {
    () => {
        Box::pin(async { Err(DisconnectedBackend::error()) })
    };
}

/// Backend used only while a remote Web/mobile client has not established an
/// authoritative desktop connection. It keeps the product shell real while
/// every business operation remains explicitly offline.
#[derive(Debug, Default)]
pub struct DisconnectedBackend;

impl DisconnectedBackend {
    pub fn facade() -> BackendFacade {
        let backend = Arc::new(Self);
        BackendFacade::new(
            BackendCapabilitySnapshot::disconnected_v1(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend,
        )
    }

    fn error() -> BackendError {
        BackendError::offline(
            "remote_runtime_not_configured",
            "pair this device with the desktop runtime to use remote workflows",
        )
        .with_recovery_hint("Open device pairing from the desktop ManagementCenter")
    }
}

impl AgentBackend for DisconnectedBackend {
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
        Err(Self::error())
    }

    fn list_sessions(&self, _include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>> {
        disconnected_future!()
    }

    fn open_session(&self, _session_id: VibexSessionId) -> BackendFuture<'_, AgentSession> {
        disconnected_future!()
    }

    fn create_session(
        &self,
        _request: MutationRequest<CreateAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        disconnected_future!()
    }

    fn fetch_timeline(&self, _request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage> {
        disconnected_future!()
    }

    fn send_message(
        &self,
        _request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        disconnected_future!()
    }

    fn continue_turn(
        &self,
        _request: MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        disconnected_future!()
    }

    fn interrupt(&self, _request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, bool> {
        disconnected_future!()
    }

    fn resolve_permission(
        &self,
        _request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        disconnected_future!()
    }

    fn resolve_elicitation(
        &self,
        _request: MutationRequest<ResolveElicitationRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        disconnected_future!()
    }

    fn rename_session(
        &self,
        _request: MutationRequest<RenameAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        disconnected_future!()
    }

    fn archive_session(&self, _request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()> {
        disconnected_future!()
    }

    fn delete_session(&self, _request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()> {
        disconnected_future!()
    }

    fn list_runtime_options(&self) -> BackendFuture<'_, SessionRuntimeOptionCatalog> {
        disconnected_future!()
    }

    fn runtime_selection(
        &self,
        _session_id: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        disconnected_future!()
    }

    fn set_desired_runtime(
        &self,
        _request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        disconnected_future!()
    }

    fn cancel_runtime_switch(
        &self,
        _request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        disconnected_future!()
    }
}

impl WorkspaceBackend for DisconnectedBackend {
    fn list_workspaces(&self) -> BackendFuture<'_, Vec<WorkspaceSummary>> {
        disconnected_future!()
    }

    fn open_workspace(
        &self,
        _request: MutationRequest<OpenWorkspaceRequest>,
    ) -> BackendFuture<'_, WorkspaceSummary> {
        disconnected_future!()
    }

    fn get_workspace(&self, _workspace_id: WorkspaceId) -> BackendFuture<'_, WorkspaceSummary> {
        disconnected_future!()
    }

    fn delete_project(&self, _request: MutationRequest<ProjectId>) -> BackendFuture<'_, ()> {
        disconnected_future!()
    }
}

impl FileBackend for DisconnectedBackend {
    fn file_tree(&self, _request: FileTreeRequest) -> BackendFuture<'_, Vec<FileTreeEntry>> {
        disconnected_future!()
    }

    fn search_files(
        &self,
        _request: FileSearchRequest,
    ) -> BackendFuture<'_, Vec<FileSearchResult>> {
        disconnected_future!()
    }

    fn read_file(&self, _request: FileReadRequest) -> BackendFuture<'_, FileReadResponse> {
        disconnected_future!()
    }

    fn write_file(
        &self,
        _request: MutationRequest<FileWriteRequest>,
    ) -> BackendFuture<'_, FileReadResponse> {
        disconnected_future!()
    }

    fn create_directory(
        &self,
        _request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        disconnected_future!()
    }

    fn copy_path(
        &self,
        _request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        disconnected_future!()
    }

    fn rename_path(
        &self,
        _request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        disconnected_future!()
    }

    fn delete_path(&self, _request: MutationRequest<FileMutationRequest>) -> BackendFuture<'_, ()> {
        disconnected_future!()
    }
}

impl GitBackend for DisconnectedBackend {
    fn git_status(&self, _workspace_id: WorkspaceId) -> BackendFuture<'_, GitStatusSummary> {
        disconnected_future!()
    }

    fn git_diff(&self, _request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse> {
        disconnected_future!()
    }

    fn git_worktree_eligibility(
        &self,
        _workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitProjectEligibility> {
        disconnected_future!()
    }

    fn git_worktree_snapshot(
        &self,
        _workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitWorktreeLifecycleSnapshot> {
        disconnected_future!()
    }

    fn git_worktree_create(
        &self,
        _request: MutationRequest<GitWorktreeCreateRequest>,
    ) -> BackendFuture<'_, GitWorktreeCreateResult> {
        disconnected_future!()
    }

    fn git_worktree_readiness(
        &self,
        _workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, Option<GitWorktreeReadinessRecord>> {
        disconnected_future!()
    }

    fn git_worktree_set_readiness(
        &self,
        _request: MutationRequest<GitWorktreeReadinessRequest>,
    ) -> BackendFuture<'_, GitWorktreeReadinessRecord> {
        disconnected_future!()
    }

    fn git_worktree_merge_plan(
        &self,
        _request: GitWorktreeMergeRequest,
    ) -> BackendFuture<'_, GitWorktreeMergePlan> {
        disconnected_future!()
    }

    fn git_worktree_merge(
        &self,
        _request: MutationRequest<GitWorktreeMergeRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_resolve_conflict(
        &self,
        _request: MutationRequest<GitWorktreeConflictResolveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_stage_conflicts(
        &self,
        _request: MutationRequest<GitWorktreeConflictStageRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_bind_assistance_session(
        &self,
        _request: MutationRequest<GitWorktreeAssistanceSessionRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_continue_merge(
        &self,
        _request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_abort_merge(
        &self,
        _request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_archive_preflight(
        &self,
        _request: GitWorktreeArchiveRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        disconnected_future!()
    }

    fn git_worktree_archive(
        &self,
        _request: MutationRequest<GitWorktreeArchiveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_restore_preflight(
        &self,
        _request: GitWorktreeRestoreRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        disconnected_future!()
    }

    fn git_worktree_restore(
        &self,
        _request: MutationRequest<GitWorktreeRestoreRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn git_worktree_discard_preflight(
        &self,
        _request: GitWorktreeDiscardRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        disconnected_future!()
    }

    fn git_worktree_discard(
        &self,
        _request: MutationRequest<GitWorktreeDiscardRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        disconnected_future!()
    }

    fn stage(
        &self,
        _request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        disconnected_future!()
    }

    fn unstage(
        &self,
        _request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        disconnected_future!()
    }

    fn commit(
        &self,
        _request: MutationRequest<GitCommitRequest>,
    ) -> BackendFuture<'_, GitCommitResult> {
        disconnected_future!()
    }
}

impl TerminalBackend for DisconnectedBackend {
    fn list_terminals(
        &self,
        _workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, Vec<TerminalSession>> {
        disconnected_future!()
    }

    fn create_terminal(
        &self,
        _request: MutationRequest<TerminalCreateRequest>,
    ) -> BackendFuture<'_, TerminalSession> {
        disconnected_future!()
    }

    fn terminal_snapshot(&self, _terminal_id: TerminalId) -> BackendFuture<'_, TerminalSnapshot> {
        disconnected_future!()
    }

    fn subscribe_terminal(
        &self,
        _terminal_id: TerminalId,
        _next_sequence: i64,
    ) -> BackendResult<Box<dyn TerminalFrameSubscription>> {
        Err(Self::error())
    }

    fn write_terminal(
        &self,
        _request: MutationRequest<TerminalWriteRequest>,
    ) -> BackendFuture<'_, ()> {
        disconnected_future!()
    }

    fn resize_terminal(
        &self,
        _request: MutationRequest<TerminalResizeRequest>,
    ) -> BackendFuture<'_, TerminalSession> {
        disconnected_future!()
    }

    fn close_terminal(
        &self,
        _request: MutationRequest<TerminalId>,
    ) -> BackendFuture<'_, TerminalSession> {
        disconnected_future!()
    }
}

impl ManagementBackend for DisconnectedBackend {
    fn list_agents(&self, _request: AgentListRequest) -> BackendFuture<'_, AgentListResponse> {
        disconnected_future!()
    }

    fn list_profiles(&self) -> BackendFuture<'_, Vec<ProviderProfileSummary>> {
        disconnected_future!()
    }

    fn select_profile(
        &self,
        _request: MutationRequest<ManagementProfileSelectionRequest>,
    ) -> BackendFuture<'_, ProviderProfileSummary> {
        disconnected_future!()
    }

    fn list_model_provider_profiles(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::ModelProviderProfile>> {
        disconnected_future!()
    }

    fn create_model_provider_profile(
        &self,
        _request: MutationRequest<vibex_core::ModelProviderProfileCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        disconnected_future!()
    }

    fn update_model_provider_profile(
        &self,
        _request: MutationRequest<vibex_core::ModelProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        disconnected_future!()
    }

    fn list_agent_runtime_profiles(
        &self,
        _agent_id: vibex_core::AgentId,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProfile>> {
        disconnected_future!()
    }

    fn create_agent_runtime_profile(
        &self,
        _request: MutationRequest<vibex_core::AgentRuntimeProfileCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
        disconnected_future!()
    }

    fn update_agent_runtime_profile(
        &self,
        _request: MutationRequest<vibex_core::AgentRuntimeProfileUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
        disconnected_future!()
    }

    fn list_agent_model_provider_bindings(
        &self,
        _request: vibex_core::AgentModelProviderBindingListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentModelProviderBinding>> {
        disconnected_future!()
    }

    fn create_agent_model_provider_binding(
        &self,
        _request: MutationRequest<vibex_core::AgentModelProviderBindingCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
        disconnected_future!()
    }

    fn update_agent_model_provider_binding(
        &self,
        _request: MutationRequest<vibex_core::AgentModelProviderBindingUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
        disconnected_future!()
    }

    fn agent_provider_projection_capability(
        &self,
        _request: vibex_core::AgentProviderProjectionCapabilityRequest,
    ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionCapability> {
        disconnected_future!()
    }

    fn preview_agent_provider_projection(
        &self,
        _request: vibex_core::AgentProviderProjectionPreviewRequest,
    ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionPreview> {
        disconnected_future!()
    }

    fn mutate_provider_credential_secret(
        &self,
        _request: MutationRequest<vibex_core::ProviderCredentialSecretMutationRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        disconnected_future!()
    }

    fn health_summaries(&self) -> BackendFuture<'_, Vec<ProviderHealthSummary>> {
        disconnected_future!()
    }

    fn run_health_probes(
        &self,
        _request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunHealthProbesResult> {
        disconnected_future!()
    }

    fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary> {
        disconnected_future!()
    }
}

impl DeviceBackend for DisconnectedBackend {
    fn create_pairing_offer(
        &self,
        _request: MutationRequest<RemoteCreatePairingCodeRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingCodeResponse> {
        disconnected_future!()
    }

    fn create_pairing_offer_v2(
        &self,
        _request: MutationRequest<RemoteCreatePairingOfferRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingOfferResponse> {
        disconnected_future!()
    }

    fn cancel_pairing_offer(
        &self,
        _request: MutationRequest<RemoteCancelPairingOfferRequest>,
    ) -> BackendFuture<'_, RemotePairingOfferSummary> {
        disconnected_future!()
    }

    fn list_devices(&self) -> BackendFuture<'_, Vec<RemoteDeviceDetail>> {
        disconnected_future!()
    }

    fn revoke_device(
        &self,
        _request: MutationRequest<RemoteRevokeDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail> {
        disconnected_future!()
    }

    fn audit_records(
        &self,
        _request: RemoteAuditListRequest,
    ) -> BackendFuture<'_, Vec<RemoteAuditRecord>> {
        disconnected_future!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityAvailability;

    #[test]
    fn disconnected_facade_is_offline_in_every_domain() {
        let facade = DisconnectedBackend::facade();
        let snapshot = facade.capabilities();
        assert!(
            [
                snapshot.agent.availability,
                snapshot.workspace.availability,
                snapshot.file.availability,
                snapshot.git.availability,
                snapshot.terminal.availability,
                snapshot.management.availability,
                snapshot.device.availability,
            ]
            .into_iter()
            .all(|availability| availability == CapabilityAvailability::Offline)
        );
        let error = match facade.agent().subscribe() {
            Ok(_) => panic!("disconnected backend unexpectedly subscribed"),
            Err(error) => error,
        };
        assert_eq!(error.code, "remote_runtime_not_configured");
    }
}

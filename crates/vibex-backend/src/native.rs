use std::sync::Arc;
use std::time::Duration;

use vibex_core::{
    AgentListRequest, AgentListResponse, AgentSession, AgentSessionRuntimeSelectionState,
    AgentUsageStatistics, AgentUsageStatisticsRequest, CancelAgentSessionRuntimeSwitchRequest,
    ContinueAgentTurnRequest, CreateAgentSessionRequest, FetchTimelineRequest, FileMutationRequest,
    FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult, FileTreeEntry,
    FileTreeRequest, FileWriteRequest, GitCommitRequest, GitCommitResult, GitDiffRequest,
    GitDiffResponse, GitStageRequest, GitStatusSummary, OpenWorkspaceRequest, ProjectId,
    ProviderHealthSummary, ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult,
    RemoteAuditListRequest, RemoteAuditRecord, RemoteCreatePairingCodeRequest,
    RemoteCreatePairingCodeResponse, RemoteCreatePairingOfferRequest,
    RemoteCreatePairingOfferResponse, RemoteDeviceDetail, RemoteRevokeDeviceRequest,
    RenameAgentSessionRequest, ResolvePermissionRequest, SendAgentMessageRequest,
    SessionRuntimeOptionCatalog, SetDesiredAgentSessionRuntimeRequest, TerminalCreateRequest,
    TerminalId, TerminalResizeRequest, TerminalSession, TerminalSnapshot, TerminalStatus,
    TerminalWriteRequest, TimelineItem, TimelinePage, VibexSessionId, WorkspaceId,
};
use vibex_desktop_runtime::{
    AuthoritativeRefetch, DesktopEvent, DesktopEventReceiver, DesktopEventStream, DesktopRuntime,
    DesktopRuntimeFacade, RelayClientConnectionState, TerminalHandle,
};

use crate::{
    AgentBackend, BackendCapabilitySnapshot, BackendError, BackendEvent, BackendEventStream,
    BackendEventSubscription, BackendFacade, BackendFuture, BackendOperation, BackendProjection,
    BackendRefetch, BackendResult, DeviceBackend, FileBackend, GitBackend, ManagementBackend,
    ManagementProfileSelectionRequest, MutationRequest, RelayConnectionState, RelayStatusSummary,
    TerminalBackend, TerminalFrame, TerminalFrameBatch, TerminalFrameSubscription,
    WorkspaceBackend, WorkspaceSummary,
};

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
            snapshot
                .device
                .operations
                .remove(&BackendOperation::DevicePairing);
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
        DesktopEvent::Runtime(event) => BackendEvent::Runtime(event),
        DesktopEvent::RuntimeSelection(event) => BackendEvent::RuntimeSelection(event),
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
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
        Ok(Box::new(NativeEventSubscription {
            receiver: self.runtime.subscribe(),
        }))
    }

    fn list_sessions(&self, include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime
                .agent()
                .list_sessions(include_archived)
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
                        .map(|(project, workspace)| WorkspaceSummary { project, workspace })
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
                .map(|(project, workspace)| WorkspaceSummary { project, workspace })
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
                .map(|(project, workspace)| WorkspaceSummary { project, workspace })
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
}

impl FileBackend for NativeBackend {
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
            runtime.git().status(&workspace_id).map_err(Into::into)
        })
    }

    fn git_diff(&self, request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse> {
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.ensure_accepting_actions()?;
            runtime.git().diff(&request).map_err(Into::into)
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
                    scope: vibex_core::ProviderProfileDefaultScope {
                        kind: vibex_core::ProviderDefaultScopeKind::Global,
                        project_id: None,
                        workspace_id: None,
                    },
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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use vibex_backend::{
    AgentBackend, BACKEND_CAPABILITY_SCHEMA_VERSION, BackendCapabilitySnapshot, BackendError,
    BackendEvent, BackendEventStream, BackendEventSubscription, BackendFacade, BackendFuture,
    BackendOperation, BackendRefetch, BackendResult, DeviceBackend, DomainCapabilities,
    FileBackend, GitBackend, ManagementBackend, ManagementProfileSelectionRequest, MutationRequest,
    RelayStatusSummary, TerminalBackend, TerminalFrameBatch, TerminalFrameSubscription,
    WorkspaceBackend, WorkspaceSummary,
};
use vibex_core::{
    AgentAuthCatalog, AgentAuthContext, AgentAuthContextAuthenticateRequest,
    AgentAuthContextAuthenticateResult, AgentAuthContextCancelAuthenticationRequest,
    AgentAuthContextId, AgentAuthContextLogoutPreview, AgentAuthContextLogoutRequest,
    AgentAuthContextMutationResult, AgentAuthContextRefreshModelsRequest,
    AgentAuthContextVerifyRequest, AgentAuthenticationOperation, AgentAuthenticationOperationId,
    AgentId, AgentListRequest, AgentListResponse, AgentNotificationIntent, AgentSession,
    AgentSessionRuntimeSelectionState, CancelAgentSessionRuntimeSwitchRequest,
    ContinueAgentTurnRequest, CreateAgentSessionRequest, FetchTimelineRequest, FileMutationRequest,
    FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult, FileTreeEntry,
    FileTreeRequest, FileWriteRequest, GetMessageSubmissionRequest, GitCommitRequest,
    GitCommitResult, GitDiffRequest, GitDiffResponse, GitProjectEligibility, GitStageRequest,
    GitStatusSummary, GitWorktreeArchiveRequest, GitWorktreeAssistanceSessionRequest,
    GitWorktreeConflictResolveRequest, GitWorktreeConflictStageRequest, GitWorktreeCreateRequest,
    GitWorktreeCreateResult, GitWorktreeDestructivePreflight, GitWorktreeDiscardRequest,
    GitWorktreeLifecycleSnapshot, GitWorktreeMergePlan, GitWorktreeMergeRequest,
    GitWorktreeOperationRecord, GitWorktreeOperationRequest, GitWorktreeReadinessRecord,
    GitWorktreeReadinessRequest, GitWorktreeRestoreRequest, MessageSubmissionState,
    OpenWorkspaceRequest, ProjectId, ProjectWorkspaceSummary, ProviderHealthSummary,
    ProviderProfileSummary, ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult,
    RemoteActionClass, RemoteAgentAuthContextListRequest, RemoteAgentAuthContextListResponse,
    RemoteAgentAuthContextMutationResponse, RemoteAgentAuthLogoutPreviewRequest,
    RemoteAgentAuthLogoutPreviewResponse, RemoteAgentAuthMethodListRequest,
    RemoteAgentAuthMethodListResponse, RemoteAgentAuthenticateContextRequest,
    RemoteAgentAuthenticateContextResponse, RemoteAgentAuthenticationOperationRequest,
    RemoteAgentAuthenticationOperationResponse, RemoteAgentCancelContextAuthenticationRequest,
    RemoteAgentCancelRuntimeSwitchRequest, RemoteAgentCancelRuntimeSwitchResponse,
    RemoteAgentCreateSessionRequest, RemoteAgentCreateSessionResponse,
    RemoteAgentDeepLinkResolveRequest, RemoteAgentDeepLinkResolveResponse,
    RemoteAgentInterruptRequest, RemoteAgentInterruptResponse, RemoteAgentLogoutAuthContextRequest,
    RemoteAgentMessageSubmissionRequest, RemoteAgentMessageSubmissionResponse,
    RemoteAgentRefreshAuthModelsRequest, RemoteAgentRenameSessionRequest,
    RemoteAgentRenameSessionResponse, RemoteAgentRequest, RemoteAgentResolveElicitationRequest,
    RemoteAgentResolveElicitationResponse, RemoteAgentResolvePermissionRequest,
    RemoteAgentResolvePermissionResponse, RemoteAgentRuntimeOptionsRequest,
    RemoteAgentRuntimeOptionsResponse, RemoteAgentRuntimeSelectionRequest,
    RemoteAgentRuntimeSelectionResponse, RemoteAgentSendMessageRequest,
    RemoteAgentSendMessageResponse, RemoteAgentSessionActionRequest,
    RemoteAgentSessionActionResponse, RemoteAgentSessionDetailRequest,
    RemoteAgentSessionDetailResponse, RemoteAgentSessionListRequest,
    RemoteAgentSessionListResponse, RemoteAgentSetDesiredRuntimeRequest,
    RemoteAgentSetDesiredRuntimeResponse, RemoteAgentTimelineFetchRequest,
    RemoteAgentTimelineFetchResponse, RemoteAgentVerifyAuthContextRequest, RemoteAuditListRequest,
    RemoteAuditRecord, RemoteAuthProof, RemoteCreatePairingCodeRequest,
    RemoteCreatePairingCodeResponse, RemoteCreatePairingOfferRequest,
    RemoteCreatePairingOfferResponse, RemoteDeepLinkResolution,
    RemoteDeviceCancelPairingOfferRequest, RemoteDeviceCreatePairingOfferRequest,
    RemoteDeviceDetail, RemoteDeviceListRequest, RemoteDeviceListResponse, RemoteDeviceRequest,
    RemoteDeviceRevokeRequest, RemoteFileDeleteResponse, RemoteFileMutationRequest,
    RemoteFileReadRequest, RemoteFileReadResponse, RemoteFileRenameResponse,
    RemoteFileSearchRequest, RemoteFileSearchResponse, RemoteFileTreeRequest,
    RemoteFileTreeResponse, RemoteFileWriteRequest, RemoteFileWriteResponse,
    RemoteGitCommitRequest, RemoteGitCommitResponse, RemoteGitDiffRequest, RemoteGitDiffResponse,
    RemoteGitStageRequest, RemoteGitStatusMutationResponse, RemoteGitStatusRequest,
    RemoteGitStatusResponse, RemoteGitWorktreeEligibilityRequest,
    RemoteGitWorktreeEligibilityResponse, RemoteGitWorktreeSnapshotRequest,
    RemoteGitWorktreeSnapshotResponse, RemoteOperationKind, RemotePairingOfferSummary,
    RemoteProviderHealthSummaryListRequest, RemoteProviderHealthSummaryListResponse,
    RemoteProviderRequest, RemoteProviderRunHealthProbesRequest,
    RemoteProviderRunHealthProbesResponse, RemoteRevokeDeviceRequest, RemoteTerminalCreateRequest,
    RemoteTerminalCreateResponse, RemoteTerminalKillRequest, RemoteTerminalKillResponse,
    RemoteTerminalListRequest, RemoteTerminalListResponse, RemoteTerminalResizeRequest,
    RemoteTerminalResizeResponse, RemoteTerminalSnapshotRequest, RemoteTerminalSnapshotResponse,
    RemoteTerminalWriteRequest, RemoteTerminalWriteResponse, RemoteWorkbenchDeleteWorkspaceRequest,
    RemoteWorkbenchDeleteWorkspaceResponse, RemoteWorkbenchListWorkspacesRequest,
    RemoteWorkbenchListWorkspacesResponse, RemoteWorkbenchOpenWorkspaceRequest,
    RemoteWorkbenchOpenWorkspaceResponse, RemoteWorkbenchRequest, RenameAgentSessionRequest,
    ResolveElicitationRequest, ResolvePermissionRequest, SendAgentMessageRequest,
    SessionRuntimeOptionCatalog, SetDesiredAgentSessionRuntimeRequest, TerminalCreateRequest,
    TerminalId, TerminalResizeRequest, TerminalSession, TerminalSnapshot, TerminalWriteRequest,
    TimelineItem, TimelineLiveEvent, TimelinePage, VibexSessionId, WorkspaceId,
};

use crate::binary::TerminalBinaryBuffer;
use crate::sync::SyncDecision;
use crate::transport::{
    AutoRemoteTransport, DirectWebSocketTransport, RelayE2eeTransport, RemoteConnectionState,
    RemoteInboundEvent, RemoteLifecycleSignal, RemoteTransport, RemoteTransportEvent,
};

/// Typed remote adapter over one transport.  It owns no domain authority; all
/// mutations are delegated to the PC RemoteGateway through the shared v2 RPC
/// envelope.
pub struct WebRemoteBackend {
    transport: Arc<dyn RemoteTransport>,
    capabilities: Arc<Mutex<BackendCapabilitySnapshot>>,
    unknown_mutation_queries: Arc<Mutex<BTreeMap<String, UnknownMutationQuery>>>,
    auth: RemoteAuthProof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UnknownMutationQuery {
    AgentMessage(GetMessageSubmissionRequest),
}

impl Clone for WebRemoteBackend {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            capabilities: self.capabilities.clone(),
            unknown_mutation_queries: self.unknown_mutation_queries.clone(),
            auth: self.auth.clone(),
        }
    }
}

impl WebRemoteBackend {
    pub fn new(transport: Arc<dyn RemoteTransport>, auth: RemoteAuthProof) -> Self {
        Self {
            transport,
            capabilities: Arc::new(Mutex::new(remote_capabilities(None))),
            unknown_mutation_queries: Arc::new(Mutex::new(BTreeMap::new())),
            auth,
        }
    }

    pub fn from_direct(transport: DirectWebSocketTransport) -> Self {
        let auth = transport.config().auth.clone();
        Self::new(Arc::new(transport), auth)
    }

    pub fn from_relay(transport: RelayE2eeTransport) -> Self {
        let auth = transport.config().remote.auth.clone();
        Self::new(Arc::new(transport), auth)
    }

    pub fn from_auto(transport: AutoRemoteTransport) -> Self {
        let auth = transport.config().remote.auth.clone();
        Self::new(Arc::new(transport), auth)
    }

    pub fn transport(&self) -> &Arc<dyn RemoteTransport> {
        &self.transport
    }

    pub fn connection_state(&self) -> crate::transport::RemoteConnectionSnapshot {
        self.transport.state()
    }

    /// Reads the Desktop's sidebar tree — folders, nesting, ordering, and the
    /// collapsed/pinned flags — so a compact client renders the layout the user
    /// arranged there instead of inventing its own.
    pub async fn sidebar_organization(
        &self,
    ) -> BackendResult<vibex_core::RemoteSidebarOrganizationSnapshot> {
        let payload = RemoteAgentRequest::GetSidebarOrganization(
            vibex_core::RemoteSidebarOrganizationRequest { auth: self.auth() },
        );
        let value = self
            .rpc(
                RemoteOperationKind::AgentSession,
                payload,
                None,
                None,
                vibex_core::RemoteTimeoutClass::Standard,
            )
            .await?;
        Ok(decode::<vibex_core::RemoteSidebarOrganizationResponse>(value)?.snapshot)
    }

    /// Applies a sidebar change on the Desktop and returns the resulting tree.
    /// `expected_revision` is the snapshot the client rendered; the Desktop
    /// refuses the change when its own layout has moved on since.
    pub async fn mutate_sidebar_organization(
        &self,
        mutation: vibex_core::RemoteSidebarOrganizationMutation,
        expected_revision: Option<u64>,
    ) -> BackendResult<vibex_core::RemoteSidebarOrganizationSnapshot> {
        let payload = RemoteAgentRequest::MutateSidebarOrganization(
            vibex_core::RemoteSidebarOrganizationMutateRequest {
                auth: self.auth(),
                mutation,
                expected_revision,
            },
        );
        let value = self
            .rpc(
                RemoteOperationKind::AgentSession,
                payload,
                Some(vibex_core::RequestId::new()),
                None,
                vibex_core::RemoteTimeoutClass::Standard,
            )
            .await?;
        Ok(decode::<vibex_core::RemoteSidebarOrganizationResponse>(value)?.snapshot)
    }

    pub fn capability_snapshot(&self) -> BackendCapabilitySnapshot {
        if let Some(info) = self.transport.server_info() {
            self.update_capabilities_from_server(&info);
        }
        let snapshot = self
            .capabilities
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| remote_capabilities(None));
        if self.transport.state().state == RemoteConnectionState::Online {
            snapshot
        } else {
            let mut degraded = snapshot;
            for domain in [
                &mut degraded.agent,
                &mut degraded.workspace,
                &mut degraded.file,
                &mut degraded.git,
                &mut degraded.terminal,
                &mut degraded.management,
                &mut degraded.device,
            ] {
                if domain.availability == vibex_backend::CapabilityAvailability::Available {
                    domain.availability = vibex_backend::CapabilityAvailability::Offline;
                }
            }
            degraded
        }
    }

    pub fn permits_remote_action(&self, action: RemoteActionClass) -> bool {
        self.transport.server_info().is_some_and(|info| {
            info.device_permissions.is_empty() || info.device_permissions.contains(&action)
        })
    }

    pub fn update_capabilities_from_server(&self, info: &vibex_core::RemoteServerInfoV2) {
        if let Ok(mut capabilities) = self.capabilities.lock() {
            *capabilities = remote_capabilities(Some(info));
        }
    }

    pub async fn connect(&self) -> BackendResult<vibex_core::RemoteServerInfoV2> {
        let info = self.transport.connect().await?;
        self.update_capabilities_from_server(&info);
        Ok(info)
    }

    pub async fn disconnect(&self) -> BackendResult<()> {
        self.transport.disconnect().await
    }

    pub fn apply_lifecycle_signal(&self, signal: RemoteLifecycleSignal) {
        self.transport.apply_lifecycle_signal(signal);
    }

    pub async fn list_agent_config_summaries(
        &self,
        include_disabled: bool,
    ) -> BackendResult<Vec<vibex_core::RemoteAgentConfigSummary>> {
        let payload = RemoteProviderRequest::ListAgentSummaries(
            vibex_core::RemoteAgentConfigSummaryListRequest {
                auth: self.auth(),
                include_disabled,
            },
        );
        let value = self
            .rpc(
                RemoteOperationKind::ProviderSettings,
                payload,
                None,
                None,
                vibex_core::RemoteTimeoutClass::Standard,
            )
            .await?;
        Ok(decode::<vibex_core::RemoteAgentConfigSummaryListResponse>(value)?.agents)
    }

    pub async fn resolve_unknown_mutation(
        &self,
        request_id: &vibex_core::RequestId,
    ) -> BackendResult<MessageSubmissionState> {
        // Reconnect never calls this automatically.  The caller first asks the
        // authoritative server whether the durable submission exists; only a
        // confirmed absence may lead to a deliberate retry with the same key.
        let query = self
            .unknown_mutation_queries
            .lock()
            .map_err(|_| {
                BackendError::failed(
                    "remote_mutation_query_registry_poisoned",
                    "remote mutation query registry is unavailable",
                )
            })?
            .get(request_id.as_str())
            .cloned()
            .ok_or_else(|| {
                BackendError::unsupported(
                    "remote_mutation_result_query_unavailable",
                    "this unknown mutation has no authoritative result-query operation",
                )
            })?;
        let UnknownMutationQuery::AgentMessage(request) = query;
        let payload =
            RemoteAgentRequest::GetMessageSubmission(RemoteAgentMessageSubmissionRequest {
                auth: self.auth(),
                request,
            });
        let value = self
            .rpc(
                RemoteOperationKind::AgentSession,
                payload,
                None,
                None,
                vibex_core::RemoteTimeoutClass::Standard,
            )
            .await?;
        let submission = decode::<RemoteAgentMessageSubmissionResponse>(value)?.submission;
        if let Ok(mut queries) = self.unknown_mutation_queries.lock() {
            queries.remove(request_id.as_str());
        }
        self.transport.clear_unknown_mutation(request_id);
        Ok(submission)
    }

    async fn rpc<T: Serialize>(
        &self,
        operation: RemoteOperationKind,
        payload: T,
        request_id: Option<vibex_core::RequestId>,
        mutation: Option<(&str, Option<&str>, Option<u64>)>,
        timeout_class: vibex_core::RemoteTimeoutClass,
    ) -> BackendResult<serde_json::Value> {
        let payload = serde_json::to_value(payload).map_err(|_| {
            BackendError::failed(
                "remote_payload_encode_failed",
                "remote request payload could not be encoded",
            )
        })?;
        let mut request = vibex_core::RemoteRpcRequestV2::new(operation, Some(payload));
        if let Some(request_id) = request_id {
            request.request_id = request_id;
        }
        request.timeout_class = timeout_class;
        if let Some((key, revision, generation)) = mutation {
            request.mutation = Some(vibex_core::RemoteMutationContract {
                idempotency_key: key.to_string(),
                expected_revision: revision.map(str::to_string),
                expected_generation: generation,
            });
        }
        let response = self.transport.request(request).await?;
        response.payload.ok_or_else(|| {
            BackendError::failed(
                "remote_payload_missing",
                "remote response did not contain a typed payload",
            )
        })
    }

    fn auth(&self) -> RemoteAuthProof {
        self.auth.clone()
    }

    fn mutation_key<T>(request: &MutationRequest<T>) -> String {
        request
            .idempotency_key
            .clone()
            .unwrap_or_else(|| format!("remote-client:{}", request.request_id.as_str()))
    }

    fn remember_unknown_query(
        &self,
        request_id: &vibex_core::RequestId,
        query: UnknownMutationQuery,
    ) {
        if let Ok(mut queries) = self.unknown_mutation_queries.lock() {
            queries.insert(request_id.as_str().to_string(), query);
        }
    }

    fn clear_unknown_query(&self, request_id: &vibex_core::RequestId) {
        if let Ok(mut queries) = self.unknown_mutation_queries.lock() {
            queries.remove(request_id.as_str());
        }
        self.transport.clear_unknown_mutation(request_id);
    }

    fn unsupported<T>(&self, code: &'static str, message: &'static str) -> BackendFuture<'_, T> {
        Box::pin(async move { Err(BackendError::unsupported(code, message)) })
    }
}

impl WebRemoteBackend {
    pub fn facade(self: &Arc<Self>) -> BackendFacade {
        let _ = self.capability_snapshot();
        BackendFacade::new_shared(
            self.capabilities.clone(),
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

struct RemoteEventSubscription {
    transport: Arc<dyn RemoteTransport>,
}

impl BackendEventSubscription for RemoteEventSubscription {
    fn next(&mut self) -> BackendFuture<'_, Option<BackendEvent>> {
        let transport = self.transport.clone();
        Box::pin(async move {
            loop {
                match transport.next_domain_event().await? {
                    Some(RemoteTransportEvent::Event(event)) => match event.decision {
                        SyncDecision::Apply => {
                            return Ok(Some(map_remote_event(event)));
                        }
                        SyncDecision::IgnoreDuplicate | SyncDecision::IgnoreStaleGeneration => {
                            // The transport has already advanced/deduplicated
                            // the authoritative cursor.  Never expose a replay
                            // as a second UI event.
                        }
                        SyncDecision::CatchUp { domain, .. }
                        | SyncDecision::Resync { domain, .. } => {
                            if let Some(projection) = projection_for_domain(&domain) {
                                return Ok(Some(BackendEvent::ProjectionInvalidated(projection)));
                            }
                            return Ok(Some(lagged_for_domain(&domain, 0, true)));
                        }
                    },
                    Some(RemoteTransportEvent::Control(
                        vibex_core::RemoteControlMessageV2::ResyncRequired(resync),
                    )) => return Ok(Some(lagged_for_domain(&resync.domain, 0, false))),
                    Some(RemoteTransportEvent::Closed) | None => {
                        return Ok(Some(BackendEvent::Disconnected));
                    }
                    Some(RemoteTransportEvent::Binary(_))
                    | Some(RemoteTransportEvent::Control(_)) => {
                        // Binary terminal/file frames are consumed by their
                        // dedicated subscriptions; unrelated controls must
                        // not make the domain event stream look exhausted.
                    }
                }
            }
        })
    }
}

fn map_remote_event(event: RemoteInboundEvent) -> BackendEvent {
    let channel = event.event.channel.as_str();
    if let Some(payload) = event.event.payload {
        if channel == "agent_session"
            && let Ok(timeline) = serde_json::from_value::<TimelineLiveEvent>(payload.clone())
        {
            return BackendEvent::Timeline(timeline);
        }
        if channel == "agent_notification"
            && let Ok(notification) =
                serde_json::from_value::<AgentNotificationIntent>(payload.clone())
        {
            return BackendEvent::Notification(notification);
        }
        if channel == "runtime"
            && let Ok(runtime) = serde_json::from_value::<vibex_core::RuntimeSessionEvent>(payload)
        {
            return BackendEvent::Runtime(runtime);
        }
    }
    if let Some(projection) = projection_for_domain(channel) {
        return BackendEvent::ProjectionInvalidated(projection);
    }
    BackendEvent::Lagged {
        stream: stream_for_domain(channel),
        skipped: 0,
        refetch: refetch_for_domain(channel),
        observed_live: true,
    }
}

fn lagged_for_domain(domain: &str, skipped: u64, observed_live: bool) -> BackendEvent {
    BackendEvent::Lagged {
        stream: stream_for_domain(domain),
        skipped,
        refetch: refetch_for_domain(domain),
        observed_live,
    }
}

fn projection_for_domain(domain: &str) -> Option<vibex_backend::BackendProjection> {
    use vibex_backend::BackendProjection;
    match domain {
        "file" => Some(BackendProjection::Files),
        "git" => Some(BackendProjection::Git),
        "sidebar" => Some(BackendProjection::Sidebar),
        "provider" | "device" => Some(BackendProjection::Management),
        _ => None,
    }
}

fn stream_for_domain(domain: &str) -> BackendEventStream {
    match domain {
        "agent_session" => BackendEventStream::Timeline,
        "runtime" => BackendEventStream::Runtime,
        _ => BackendEventStream::Fanout,
    }
}

fn refetch_for_domain(domain: &str) -> BackendRefetch {
    BackendRefetch {
        session_id: None,
        timeline: domain == "agent_session",
        runtime: domain == "runtime",
        runtime_selection: domain == "runtime",
        projection: projection_for_domain(domain),
    }
}

impl AgentBackend for WebRemoteBackend {
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
        Ok(Box::new(RemoteEventSubscription {
            transport: self.transport.clone(),
        }))
    }

    fn list_sessions(&self, include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteAgentRequest::ListSessions(RemoteAgentSessionListRequest {
                auth: this.auth(),
                include_archived: Some(include_archived),
                timeline_limit: Some(50),
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            let response: RemoteAgentSessionListResponse = decode(value)?;
            Ok(response
                .sessions
                .into_iter()
                .map(|summary| summary.session)
                .collect())
        })
    }

    fn open_session(&self, session_id: VibexSessionId) -> BackendFuture<'_, AgentSession> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteAgentRequest::GetSession(RemoteAgentSessionDetailRequest {
                auth: this.auth(),
                session_id,
                timeline_limit: Some(50),
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentSessionDetailResponse>(value)?.session)
        })
    }

    fn create_session(
        &self,
        request: MutationRequest<CreateAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteAgentRequest::CreateSession(RemoteAgentCreateSessionRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentCreateSessionResponse>(value)?.session)
        })
    }

    fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteAgentRequest::FetchTimeline(RemoteAgentTimelineFetchRequest {
                auth: this.auth(),
                request,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentTimelineFetchResponse>(value)?.page)
        })
    }

    fn resolve_opaque_locator(
        &self,
        notification_id: String,
        opaque_locator: String,
    ) -> BackendFuture<'_, RemoteDeepLinkResolution> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteAgentRequest::ResolveOpaqueLocator(RemoteAgentDeepLinkResolveRequest {
                    auth: this.auth(),
                    notification_id,
                    opaque_locator,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteAgentDeepLinkResolveResponse>(value)?.resolution)
        })
    }

    fn send_message(
        &self,
        request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let request_id = request.request_id.clone();
            let query = GetMessageSubmissionRequest {
                session_id: request.payload.session_id.clone(),
                message_idempotency_key: request.payload.message_idempotency_key.clone(),
            };
            let payload = RemoteAgentRequest::SendMessage(RemoteAgentSendMessageRequest {
                auth: this.auth(),
                request: request.payload,
            });
            this.remember_unknown_query(&request_id, UnknownMutationQuery::AgentMessage(query));
            let value = match this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    if !is_unknown_mutation_error(&error) {
                        this.clear_unknown_query(&request_id);
                    }
                    return Err(error);
                }
            };
            this.clear_unknown_query(&request_id);
            Ok(decode::<RemoteAgentSendMessageResponse>(value)?.appended_items)
        })
    }

    fn continue_turn(
        &self,
        request: MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::ContinueTurn(vibex_core::RemoteAgentContinueTurnRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentContinueTurnResponse>(value)?.appended_items)
        })
    }

    fn interrupt(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, bool> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteAgentRequest::Interrupt(RemoteAgentInterruptRequest {
                auth: this.auth(),
                session_id: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteAgentInterruptResponse>(value)?.interrupted)
        })
    }

    fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::ResolvePermission(RemoteAgentResolvePermissionRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteAgentResolvePermissionResponse>(value)?.item)
        })
    }

    fn resolve_elicitation(
        &self,
        request: MutationRequest<ResolveElicitationRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::ResolveElicitation(RemoteAgentResolveElicitationRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteAgentResolveElicitationResponse>(value)?.item)
        })
    }

    fn rename_session(
        &self,
        request: MutationRequest<RenameAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteAgentRequest::RenameSession(RemoteAgentRenameSessionRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteAgentRenameSessionResponse>(value)?.session)
        })
    }

    fn archive_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteAgentRequest::ArchiveSession(RemoteAgentSessionActionRequest {
                auth: this.auth(),
                session_id: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            let response = decode::<RemoteAgentSessionActionResponse>(value)?;
            if response.completed {
                Ok(())
            } else {
                Err(BackendError::failed(
                    "remote_agent_session_archive_failed",
                    "the desktop did not confirm session archive",
                ))
            }
        })
    }

    fn delete_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteAgentRequest::DeleteSession(RemoteAgentSessionActionRequest {
                auth: this.auth(),
                session_id: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            let response = decode::<RemoteAgentSessionActionResponse>(value)?;
            if response.completed {
                Ok(())
            } else {
                Err(BackendError::failed(
                    "remote_agent_session_delete_failed",
                    "the desktop did not confirm session deletion",
                ))
            }
        })
    }

    fn list_runtime_options(&self) -> BackendFuture<'_, SessionRuntimeOptionCatalog> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteAgentRequest::ListRuntimeOptions(RemoteAgentRuntimeOptionsRequest {
                    auth: this.auth(),
                    supports_agent_account_auth: true,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentRuntimeOptionsResponse>(value)?.catalog)
        })
    }

    fn list_agent_auth_contexts(&self) -> BackendFuture<'_, Vec<AgentAuthContext>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteAgentRequest::ListAuthContexts(RemoteAgentAuthContextListRequest {
                auth: this.auth(),
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthContextListResponse>(value)?.contexts)
        })
    }

    fn list_agent_auth_methods(&self, agent_id: AgentId) -> BackendFuture<'_, AgentAuthCatalog> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteAgentRequest::ListAuthMethods(RemoteAgentAuthMethodListRequest {
                auth: this.auth(),
                agent_id,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthMethodListResponse>(value)?.catalog)
        })
    }

    fn authenticate_agent_context(
        &self,
        request: MutationRequest<AgentAuthContextAuthenticateRequest>,
    ) -> BackendFuture<'_, AgentAuthContextAuthenticateResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::AuthenticateContext(RemoteAgentAuthenticateContextRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthenticateContextResponse>(value)?.result)
        })
    }

    fn get_agent_authentication_operation(
        &self,
        operation_id: AgentAuthenticationOperationId,
    ) -> BackendFuture<'_, AgentAuthenticationOperation> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteAgentRequest::GetAuthenticationOperation(
                RemoteAgentAuthenticationOperationRequest {
                    auth: this.auth(),
                    operation_id,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthenticationOperationResponse>(value)?.operation)
        })
    }

    fn cancel_agent_context_authentication(
        &self,
        request: MutationRequest<AgentAuthContextCancelAuthenticationRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteAgentRequest::CancelContextAuthentication(
                RemoteAgentCancelContextAuthenticationRequest {
                    auth: this.auth(),
                    request: request.payload,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthContextMutationResponse>(value)?.result)
        })
    }

    fn verify_agent_auth_context(
        &self,
        request: MutationRequest<AgentAuthContextVerifyRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::VerifyAuthContext(RemoteAgentVerifyAuthContextRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthContextMutationResponse>(value)?.result)
        })
    }

    fn refresh_agent_auth_models(
        &self,
        request: MutationRequest<AgentAuthContextRefreshModelsRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::RefreshAuthModels(RemoteAgentRefreshAuthModelsRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthContextMutationResponse>(value)?.result)
        })
    }

    fn preview_agent_auth_logout(
        &self,
        auth_context_id: AgentAuthContextId,
    ) -> BackendFuture<'_, AgentAuthContextLogoutPreview> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteAgentRequest::PreviewAuthLogout(RemoteAgentAuthLogoutPreviewRequest {
                    auth: this.auth(),
                    auth_context_id,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthLogoutPreviewResponse>(value)?.preview)
        })
    }

    fn logout_agent_auth_context(
        &self,
        request: MutationRequest<AgentAuthContextLogoutRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::LogoutAuthContext(RemoteAgentLogoutAuthContextRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentAuthContextMutationResponse>(value)?.result)
        })
    }

    fn runtime_selection(
        &self,
        session_id: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteAgentRequest::GetRuntimeSelection(RemoteAgentRuntimeSelectionRequest {
                    auth: this.auth(),
                    session_id,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteAgentRuntimeSelectionResponse>(value)?.state)
        })
    }

    fn set_desired_runtime(
        &self,
        request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::SetDesiredRuntime(RemoteAgentSetDesiredRuntimeRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentSetDesiredRuntimeResponse>(value)?.state)
        })
    }

    fn cancel_runtime_switch(
        &self,
        request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteAgentRequest::CancelRuntimeSwitch(RemoteAgentCancelRuntimeSwitchRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::AgentSession,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteAgentCancelRuntimeSwitchResponse>(value)?.state)
        })
    }
}

impl WorkspaceBackend for WebRemoteBackend {
    fn list_workspaces(&self) -> BackendFuture<'_, Vec<WorkspaceSummary>> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteWorkbenchRequest::ListWorkspaces(RemoteWorkbenchListWorkspacesRequest {
                    auth: this.auth(),
                });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteWorkbenchListWorkspacesResponse>(value)?
                .workspaces
                .into_iter()
                .map(summary_to_backend)
                .collect())
        })
    }

    fn open_workspace(
        &self,
        request: MutationRequest<OpenWorkspaceRequest>,
    ) -> BackendFuture<'_, WorkspaceSummary> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteWorkbenchRequest::OpenWorkspace(RemoteWorkbenchOpenWorkspaceRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(summary_to_backend(
                decode::<RemoteWorkbenchOpenWorkspaceResponse>(value)?.summary,
            ))
        })
    }

    fn get_workspace(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, WorkspaceSummary> {
        let this = self.clone();
        Box::pin(async move {
            let workspaces = this.list_workspaces().await?;
            workspaces
                .into_iter()
                .find(|summary| summary.workspace.id == workspace_id)
                .ok_or_else(|| {
                    BackendError::failed("workspace_not_found", "remote workspace was not found")
                })
        })
    }

    fn delete_workspace(&self, request: MutationRequest<WorkspaceId>) -> BackendFuture<'_, ()> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteWorkbenchRequest::DeleteWorkspace(RemoteWorkbenchDeleteWorkspaceRequest {
                    auth: this.auth(),
                    workspace_id: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            let response = decode::<RemoteWorkbenchDeleteWorkspaceResponse>(value)?;
            if response.deleted {
                Ok(())
            } else {
                Err(BackendError::failed(
                    "remote_workspace_delete_failed",
                    "remote desktop did not delete the workspace",
                ))
            }
        })
    }

    fn delete_project(&self, _request: MutationRequest<ProjectId>) -> BackendFuture<'_, ()> {
        self.unsupported(
            "remote_workspace_delete_unavailable",
            "remote project deletion is not exposed by this Gateway",
        )
    }
}

impl FileBackend for WebRemoteBackend {
    fn file_tree(&self, request: FileTreeRequest) -> BackendFuture<'_, Vec<FileTreeEntry>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::FileListTree(RemoteFileTreeRequest {
                auth: this.auth(),
                request,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteFileTreeResponse>(value)?.entries)
        })
    }

    fn search_files(&self, request: FileSearchRequest) -> BackendFuture<'_, Vec<FileSearchResult>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::FileSearch(RemoteFileSearchRequest {
                auth: this.auth(),
                request,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteFileSearchResponse>(value)?.results)
        })
    }

    fn read_file(&self, request: FileReadRequest) -> BackendFuture<'_, FileReadResponse> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::FileRead(RemoteFileReadRequest {
                auth: this.auth(),
                request,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteFileReadResponse>(value)?.file)
        })
    }

    fn write_file(
        &self,
        request: MutationRequest<FileWriteRequest>,
    ) -> BackendFuture<'_, FileReadResponse> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::FileWrite(RemoteFileWriteRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteFileWriteResponse>(value)?.file)
        })
    }

    fn create_directory(
        &self,
        _request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        self.unsupported(
            "remote_file_directory_create_unavailable",
            "remote directory creation is not exposed by this Gateway",
        )
    }

    fn copy_path(
        &self,
        _request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        self.unsupported(
            "remote_file_copy_unavailable",
            "remote path copying is not exposed by this Gateway",
        )
    }

    fn rename_path(
        &self,
        request: MutationRequest<FileMutationRequest>,
    ) -> BackendFuture<'_, FileTreeEntry> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::FileRename(RemoteFileMutationRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteFileRenameResponse>(value)?.entry)
        })
    }

    fn delete_path(&self, request: MutationRequest<FileMutationRequest>) -> BackendFuture<'_, ()> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::FileDelete(RemoteFileMutationRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::WorkspaceFile,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            let response: RemoteFileDeleteResponse = decode(value)?;
            if response.deleted {
                Ok(())
            } else {
                Err(BackendError::failed(
                    "remote_file_delete_failed",
                    "remote file was not deleted",
                ))
            }
        })
    }
}

impl GitBackend for WebRemoteBackend {
    fn git_status(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, GitStatusSummary> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::GitStatus(RemoteGitStatusRequest {
                auth: this.auth(),
                workspace_id,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Git,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteGitStatusResponse>(value)?.status)
        })
    }

    fn git_diff(&self, request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::GitDiff(RemoteGitDiffRequest {
                auth: this.auth(),
                request,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Git,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteGitDiffResponse>(value)?.diff)
        })
    }

    fn git_worktree_eligibility(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitProjectEligibility> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::GitWorktreeEligibility(
                RemoteGitWorktreeEligibilityRequest {
                    auth: this.auth(),
                    workspace_id,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::Git,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteGitWorktreeEligibilityResponse>(value)?.eligibility)
        })
    }

    fn git_worktree_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitWorktreeLifecycleSnapshot> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteWorkbenchRequest::GitWorktreeSnapshot(RemoteGitWorktreeSnapshotRequest {
                    auth: this.auth(),
                    workspace_id,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::Git,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteGitWorktreeSnapshotResponse>(value)?.snapshot)
        })
    }

    fn git_worktree_create(
        &self,
        _request: MutationRequest<GitWorktreeCreateRequest>,
    ) -> BackendFuture<'_, GitWorktreeCreateResult> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "managed worktree creation is available only on the desktop runtime",
        )
    }

    fn git_worktree_readiness(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, Option<GitWorktreeReadinessRecord>> {
        let this = self.clone();
        Box::pin(async move {
            let snapshot = this.git_worktree_snapshot(workspace_id.clone()).await?;
            Ok(snapshot
                .readiness
                .into_iter()
                .find(|readiness| readiness.workspace_id == workspace_id))
        })
    }

    fn git_worktree_set_readiness(
        &self,
        _request: MutationRequest<GitWorktreeReadinessRequest>,
    ) -> BackendFuture<'_, GitWorktreeReadinessRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree readiness mutation is available only on the desktop runtime",
        )
    }

    fn git_worktree_merge_plan(
        &self,
        _request: GitWorktreeMergeRequest,
    ) -> BackendFuture<'_, GitWorktreeMergePlan> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree merge planning is available only on the desktop runtime",
        )
    }

    fn git_worktree_merge(
        &self,
        _request: MutationRequest<GitWorktreeMergeRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree merge is available only on the desktop runtime",
        )
    }

    fn git_worktree_resolve_conflict(
        &self,
        _request: MutationRequest<GitWorktreeConflictResolveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree conflict mutation is available only on the desktop runtime",
        )
    }

    fn git_worktree_stage_conflicts(
        &self,
        _request: MutationRequest<GitWorktreeConflictStageRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree conflict staging is available only on the desktop runtime",
        )
    }

    fn git_worktree_bind_assistance_session(
        &self,
        _request: MutationRequest<GitWorktreeAssistanceSessionRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree assistance Session binding is available only on the desktop runtime",
        )
    }

    fn git_worktree_continue_merge(
        &self,
        _request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree merge continue is available only on the desktop runtime",
        )
    }

    fn git_worktree_abort_merge(
        &self,
        _request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree merge abort is available only on the desktop runtime",
        )
    }

    fn git_worktree_archive_preflight(
        &self,
        _request: GitWorktreeArchiveRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree archive planning is available only on the desktop runtime",
        )
    }

    fn git_worktree_archive(
        &self,
        _request: MutationRequest<GitWorktreeArchiveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree archive is available only on the desktop runtime",
        )
    }

    fn git_worktree_restore_preflight(
        &self,
        _request: GitWorktreeRestoreRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree restore planning is available only on the desktop runtime",
        )
    }

    fn git_worktree_restore(
        &self,
        _request: MutationRequest<GitWorktreeRestoreRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree restore is available only on the desktop runtime",
        )
    }

    fn git_worktree_discard_preflight(
        &self,
        _request: GitWorktreeDiscardRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree discard planning is available only on the desktop runtime",
        )
    }

    fn git_worktree_discard(
        &self,
        _request: MutationRequest<GitWorktreeDiscardRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
        self.unsupported(
            "remote_worktree_mutation_unsupported",
            "worktree discard is available only on the desktop runtime",
        )
    }

    fn stage(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        self.git_stage_like(request, RemoteWorkbenchRequest::GitStage)
    }

    fn unstage(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary> {
        self.git_stage_like(request, RemoteWorkbenchRequest::GitUnstage)
    }

    fn commit(
        &self,
        request: MutationRequest<GitCommitRequest>,
    ) -> BackendFuture<'_, GitCommitResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::GitCommit(RemoteGitCommitRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Git,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteGitCommitResponse>(value)?.result)
        })
    }
}

impl WebRemoteBackend {
    fn git_stage_like(
        &self,
        request: MutationRequest<GitStageRequest>,
        constructor: fn(vibex_core::RemoteGitStageRequest) -> RemoteWorkbenchRequest,
    ) -> BackendFuture<'_, GitStatusSummary> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let workspace_id = request.payload.workspace_id.clone();
            let key = Self::mutation_key(&request);
            let payload = constructor(RemoteGitStageRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Git,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            let response: RemoteGitStatusMutationResponse = decode(value)?;
            if response.status.workspace_id != workspace_id {
                return Err(BackendError::failed(
                    "remote_git_workspace_mismatch",
                    "remote Git response workspace does not match the request",
                ));
            }
            Ok(response.status)
        })
    }
}

struct RemoteTerminalSubscription {
    backend: Arc<WebRemoteBackend>,
    terminal_id: TerminalId,
    next_sequence: i64,
    attachment_id: String,
    attached: bool,
    buffer: TerminalBinaryBuffer,
}

impl TerminalFrameSubscription for RemoteTerminalSubscription {
    fn next(&mut self) -> BackendFuture<'_, Option<TerminalFrameBatch>> {
        let backend = self.backend.clone();
        Box::pin(async move {
            if !self.attached {
                let snapshot = backend.terminal_snapshot(self.terminal_id.clone()).await?;
                let requested_sequence = self.next_sequence.max(1) as u64;
                let accepted = backend
                    .transport
                    .attach(vibex_core::RemoteAttachRequestV2 {
                        attachment_id: self.attachment_id.clone(),
                        kind: vibex_core::RemoteAttachmentKind::Terminal,
                        resource_id: self.terminal_id.as_str().to_string(),
                        scope_id: Some(snapshot.session.workspace_id.as_str().to_string()),
                        generation: backend.transport.state().session_epoch.unwrap_or(0),
                        after_sequence: requested_sequence,
                    })
                    .await?;
                if accepted.generation != backend.transport.state().session_epoch.unwrap_or(0)
                    || accepted.snapshot_required
                {
                    self.buffer.require_reset(requested_sequence as i64);
                }
                self.attached = true;
            }
            loop {
                match backend
                    .transport
                    .next_binary_event_for(Some(self.terminal_id.as_str().to_string()))
                    .await?
                {
                    Some(RemoteTransportEvent::Binary(frame))
                        if frame.header.stream_id == self.terminal_id.as_str() =>
                    {
                        if self.buffer.push_frame(&frame).is_err() {
                            // A gap/rebuild is represented by a reset batch;
                            // never append bytes across an unknown cursor.  A
                            // fresh buffer starts at the received frame so the
                            // caller gets the first rebuild byte instead of
                            // silently dropping it.
                            let sequence = i64::try_from(frame.header.sequence)
                                .unwrap_or(i64::MAX)
                                .max(1);
                            self.buffer =
                                TerminalBinaryBuffer::new(self.terminal_id.clone(), 128, sequence);
                            self.buffer.require_reset(sequence);
                            let _ = self.buffer.push_frame(&frame);
                        }
                        if let Some(batch) = self.buffer.take_batch() {
                            self.next_sequence = batch.next_sequence;
                            return Ok(Some(batch));
                        }
                    }
                    Some(RemoteTransportEvent::Closed) | None => return Ok(None),
                    Some(_) => {}
                }
            }
        })
    }
}

impl TerminalBackend for WebRemoteBackend {
    fn list_terminals(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, Vec<TerminalSession>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::TerminalList(RemoteTerminalListRequest {
                auth: this.auth(),
                workspace_id,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Terminal,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteTerminalListResponse>(value)?.terminals)
        })
    }

    fn create_terminal(
        &self,
        request: MutationRequest<TerminalCreateRequest>,
    ) -> BackendFuture<'_, TerminalSession> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::TerminalCreate(RemoteTerminalCreateRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Terminal,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteTerminalCreateResponse>(value)?.terminal)
        })
    }

    fn terminal_snapshot(&self, terminal_id: TerminalId) -> BackendFuture<'_, TerminalSnapshot> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteWorkbenchRequest::TerminalSnapshot(RemoteTerminalSnapshotRequest {
                auth: this.auth(),
                terminal_id,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Terminal,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteTerminalSnapshotResponse>(value)?.snapshot)
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
        let backend = Arc::new(self.clone());
        Ok(Box::new(RemoteTerminalSubscription {
            backend,
            terminal_id: terminal_id.clone(),
            next_sequence,
            attachment_id: format!("terminal-attachment-{}", terminal_id.as_str()),
            attached: false,
            buffer: TerminalBinaryBuffer::new(terminal_id, 128, next_sequence),
        }))
    }

    fn write_terminal(
        &self,
        request: MutationRequest<TerminalWriteRequest>,
    ) -> BackendFuture<'_, ()> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::TerminalWrite(RemoteTerminalWriteRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Terminal,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            if decode::<RemoteTerminalWriteResponse>(value)?.written {
                Ok(())
            } else {
                Err(BackendError::failed(
                    "remote_terminal_write_failed",
                    "remote terminal did not accept input",
                ))
            }
        })
    }

    fn resize_terminal(
        &self,
        request: MutationRequest<TerminalResizeRequest>,
    ) -> BackendFuture<'_, TerminalSession> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::TerminalResize(RemoteTerminalResizeRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Terminal,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Interactive,
                )
                .await?;
            Ok(decode::<RemoteTerminalResizeResponse>(value)?.terminal)
        })
    }

    fn close_terminal(
        &self,
        request: MutationRequest<TerminalId>,
    ) -> BackendFuture<'_, TerminalSession> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteWorkbenchRequest::TerminalKill(RemoteTerminalKillRequest {
                auth: this.auth(),
                terminal_id: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::Terminal,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteTerminalKillResponse>(value)?.terminal)
        })
    }
}

impl ManagementBackend for WebRemoteBackend {
    fn list_agents(&self, _request: AgentListRequest) -> BackendFuture<'_, AgentListResponse> {
        self.unsupported(
            "remote_management_agents_unavailable",
            "remote Agent management summaries are not exposed by this Gateway",
        )
    }

    fn create_custom_agent(
        &self,
        _request: MutationRequest<vibex_core::CustomAgentCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentSnapshotEntry> {
        self.unsupported(
            "remote_custom_agent_management_unavailable",
            "custom Agent management is only available on the authoritative desktop",
        )
    }

    fn delete_custom_agent(
        &self,
        _request: MutationRequest<vibex_core::CustomAgentDeleteRequest>,
    ) -> BackendFuture<'_, ()> {
        self.unsupported(
            "remote_custom_agent_management_unavailable",
            "custom Agent management is only available on the authoritative desktop",
        )
    }

    fn list_profiles(&self) -> BackendFuture<'_, Vec<ProviderProfileSummary>> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteProviderRequest::ListProfiles(vibex_core::RemoteProviderProfileListRequest {
                    auth: this.auth(),
                });
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteProviderProfileListResponse>(value)?.profiles)
        })
    }

    fn select_profile(
        &self,
        _request: MutationRequest<ManagementProfileSelectionRequest>,
    ) -> BackendFuture<'_, ProviderProfileSummary> {
        self.unsupported(
            "remote_provider_profile_select_unavailable",
            "remote profile selection is not exposed by this Gateway",
        )
    }

    fn list_model_provider_profiles(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::ModelProviderProfile>> {
        self.unsupported(
            "remote_model_provider_profiles_private",
            "remote clients receive projection capabilities instead of provider storage records",
        )
    }

    fn create_model_provider_profile(
        &self,
        _request: MutationRequest<vibex_core::ModelProviderProfileCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        self.unsupported(
            "remote_model_provider_mutation_unavailable",
            "model provider mutations must be performed by the authoritative desktop",
        )
    }

    fn update_model_provider_profile(
        &self,
        _request: MutationRequest<vibex_core::ModelProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        self.unsupported(
            "remote_model_provider_mutation_unavailable",
            "model provider mutations must be performed by the authoritative desktop",
        )
    }

    fn list_agent_runtime_profiles(
        &self,
        _agent_id: vibex_core::AgentId,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProfile>> {
        self.unsupported(
            "remote_agent_runtime_profiles_private",
            "remote clients do not receive native Agent command or runtime-home records",
        )
    }

    fn create_agent_runtime_profile(
        &self,
        _request: MutationRequest<vibex_core::AgentRuntimeProfileCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
        self.unsupported(
            "remote_agent_runtime_mutation_unavailable",
            "Agent runtime mutations must be performed by the authoritative desktop",
        )
    }

    fn update_agent_runtime_profile(
        &self,
        _request: MutationRequest<vibex_core::AgentRuntimeProfileUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
        self.unsupported(
            "remote_agent_runtime_mutation_unavailable",
            "Agent runtime mutations must be performed by the authoritative desktop",
        )
    }

    fn list_agent_model_provider_bindings(
        &self,
        _request: vibex_core::AgentModelProviderBindingListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentModelProviderBinding>> {
        self.unsupported(
            "remote_agent_provider_bindings_private",
            "remote clients receive projection capabilities instead of binding storage records",
        )
    }

    fn create_agent_model_provider_binding(
        &self,
        _request: MutationRequest<vibex_core::AgentModelProviderBindingCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
        self.unsupported(
            "remote_agent_provider_binding_mutation_unavailable",
            "Agent provider binding mutations must be performed by the authoritative desktop",
        )
    }

    fn update_agent_model_provider_binding(
        &self,
        _request: MutationRequest<vibex_core::AgentModelProviderBindingUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
        self.unsupported(
            "remote_agent_provider_binding_mutation_unavailable",
            "Agent provider binding mutations must be performed by the authoritative desktop",
        )
    }

    fn agent_provider_projection_capability(
        &self,
        request: vibex_core::AgentProviderProjectionCapabilityRequest,
    ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionCapability> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteProviderRequest::ProjectionCapability(
                vibex_core::RemoteAgentProjectionCapabilityRequest {
                    auth: this.auth(),
                    request,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentProjectionCapabilityResponse>(value)?.capability)
        })
    }

    fn preview_agent_provider_projection(
        &self,
        request: vibex_core::AgentProviderProjectionPreviewRequest,
    ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionPreview> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteProviderRequest::ProjectionPreview(
                vibex_core::RemoteAgentProjectionPreviewRequest {
                    auth: this.auth(),
                    request,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentProjectionPreviewResponse>(value)?.preview)
        })
    }

    fn start_agent_runtime_probe(
        &self,
        request: MutationRequest<vibex_core::AgentRuntimeProbeStartRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProbeRecord> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteProviderRequest::StartRuntimeProbe(
                vibex_core::RemoteAgentRuntimeProbeStartRequest {
                    auth: this.auth(),
                    request: request.payload,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    Some(request.request_id),
                    Some((&key, None, None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentRuntimeProbeStartResponse>(value)?.probe)
        })
    }

    fn get_agent_runtime_probe(
        &self,
        probe_id: vibex_core::AgentRuntimeProbeId,
    ) -> BackendFuture<'_, Option<vibex_core::AgentRuntimeProbeRecord>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteProviderRequest::GetRuntimeProbe(
                vibex_core::RemoteAgentRuntimeProbeGetRequest {
                    auth: this.auth(),
                    probe_id,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentRuntimeProbeGetResponse>(value)?.probe)
        })
    }

    fn list_agent_runtime_probes(
        &self,
        request: vibex_core::AgentRuntimeProbeListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProbeRecord>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteProviderRequest::ListRuntimeProbes(
                vibex_core::RemoteAgentRuntimeProbeListRequest {
                    auth: this.auth(),
                    request,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentRuntimeProbeListResponse>(value)?.probes)
        })
    }

    fn cancel_agent_runtime_probe(
        &self,
        request: MutationRequest<vibex_core::AgentRuntimeProbeCancelRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentRuntimeProbeRecord> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteProviderRequest::CancelRuntimeProbe(
                vibex_core::RemoteAgentRuntimeProbeCancelRequest {
                    auth: this.auth(),
                    request: request.payload,
                },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    Some(request.request_id),
                    Some((&key, None, None)),
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<vibex_core::RemoteAgentRuntimeProbeCancelResponse>(value)?.probe)
        })
    }

    fn mutate_provider_credential_secret(
        &self,
        _request: MutationRequest<vibex_core::ProviderCredentialSecretMutationRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
        self.unsupported(
            "remote_provider_secret_mutation_unavailable",
            "Secret values never cross the Remote provider protocol",
        )
    }

    fn health_summaries(&self) -> BackendFuture<'_, Vec<ProviderHealthSummary>> {
        let this = self.clone();
        Box::pin(async move {
            let payload = RemoteProviderRequest::ListHealthSummaries(
                RemoteProviderHealthSummaryListRequest { auth: this.auth() },
            );
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteProviderHealthSummaryListResponse>(value)?.summaries)
        })
    }

    fn run_health_probes(
        &self,
        request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunHealthProbesResult> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteProviderRequest::RunHealthProbes(RemoteProviderRunHealthProbesRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::ProviderSettings,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            Ok(decode::<RemoteProviderRunHealthProbesResponse>(value)?.result)
        })
    }

    fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary> {
        self.unsupported(
            "remote_relay_status_unavailable",
            "Relay status belongs to the local desktop management runtime",
        )
    }
}

impl DeviceBackend for WebRemoteBackend {
    fn create_pairing_offer(
        &self,
        _request: MutationRequest<RemoteCreatePairingCodeRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingCodeResponse> {
        self.unsupported(
            "remote_device_pairing_unavailable",
            "a remote client cannot create a new pairing offer",
        )
    }

    fn create_pairing_offer_v2(
        &self,
        request: MutationRequest<RemoteCreatePairingOfferRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingOfferResponse> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteDeviceRequest::CreatePairingOffer(RemoteDeviceCreatePairingOfferRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::DeviceManagement,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::LongRunning,
                )
                .await?;
            decode(value)
        })
    }

    fn cancel_pairing_offer(
        &self,
        request: MutationRequest<vibex_core::RemoteCancelPairingOfferRequest>,
    ) -> BackendFuture<'_, RemotePairingOfferSummary> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload =
                RemoteDeviceRequest::CancelPairingOffer(RemoteDeviceCancelPairingOfferRequest {
                    auth: this.auth(),
                    request: request.payload,
                });
            let value = this
                .rpc(
                    RemoteOperationKind::DeviceManagement,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            decode(value)
        })
    }

    fn list_devices(&self) -> BackendFuture<'_, Vec<RemoteDeviceDetail>> {
        let this = self.clone();
        Box::pin(async move {
            let payload =
                RemoteDeviceRequest::ListDevices(RemoteDeviceListRequest { auth: this.auth() });
            let value = this
                .rpc(
                    RemoteOperationKind::DeviceManagement,
                    payload,
                    None,
                    None,
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            Ok(decode::<RemoteDeviceListResponse>(value)?.devices)
        })
    }

    fn revoke_device(
        &self,
        request: MutationRequest<RemoteRevokeDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail> {
        let this = self.clone();
        Box::pin(async move {
            request.validate()?;
            let key = Self::mutation_key(&request);
            let payload = RemoteDeviceRequest::RevokeDevice(RemoteDeviceRevokeRequest {
                auth: this.auth(),
                request: request.payload,
            });
            let value = this
                .rpc(
                    RemoteOperationKind::DeviceManagement,
                    payload,
                    Some(request.request_id),
                    Some((&key, request.expected_revision.as_deref(), None)),
                    vibex_core::RemoteTimeoutClass::Standard,
                )
                .await?;
            decode(value)
        })
    }

    fn audit_records(
        &self,
        _request: RemoteAuditListRequest,
    ) -> BackendFuture<'_, Vec<RemoteAuditRecord>> {
        self.unsupported(
            "remote_audit_unavailable",
            "remote audit administration remains local to the desktop",
        )
    }
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> BackendResult<T> {
    serde_json::from_value(value).map_err(|error| {
        BackendError::failed(
            "remote_payload_decode_failed",
            format!("remote typed response could not be decoded: {error}"),
        )
    })
}

fn is_unknown_mutation_error(error: &BackendError) -> bool {
    matches!(
        error.code.as_str(),
        "remote_rpc_timeout" | "remote_rpc_result_unknown" | "remote_socket_write_failed"
    )
}

fn summary_to_backend(summary: ProjectWorkspaceSummary) -> WorkspaceSummary {
    WorkspaceSummary {
        project: summary.project,
        workspace: summary.workspace,
        git_branch: summary.git_branch,
    }
}

fn remote_capabilities(info: Option<&vibex_core::RemoteServerInfoV2>) -> BackendCapabilitySnapshot {
    use BackendOperation::*;
    let features = info
        .map(|info| {
            info.enabled_features
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let has_agent = features.is_empty() || features.contains("agent");
    let has_agent_account_auth = features.contains("agent_account_auth");
    let has_workbench = features.is_empty() || features.contains("workspace_file");
    let has_git = features.is_empty() || features.contains("git");
    let has_worktree_read = features.contains("git_worktree_read");
    let has_terminal = features.is_empty() || features.contains("terminal");
    let has_provider = features.is_empty() || features.contains("provider_settings");
    let has_device = features.contains("device_management");
    let has_device_pairing = features.contains("device_pairing");
    let permits = |action: RemoteActionClass| {
        info.and_then(|info| {
            if info.device_permissions.is_empty() {
                None
            } else {
                Some(info.device_permissions.contains(&action))
            }
        })
        .unwrap_or(true)
    };
    BackendCapabilitySnapshot {
        schema_version: BACKEND_CAPABILITY_SCHEMA_VERSION.to_string(),
        revision: 1,
        agent: if has_agent {
            available_filtered([
                (
                    AgentListSessions,
                    permits(RemoteActionClass::ReadAgentSession),
                ),
                (
                    AgentOpenSession,
                    permits(RemoteActionClass::ReadAgentSession),
                ),
                (
                    AgentCreateSession,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
                (
                    AgentFetchTimeline,
                    permits(RemoteActionClass::ReadAgentSession),
                ),
                (
                    AgentSendMessage,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
                (
                    AgentContinueTurn,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
                (
                    AgentInterrupt,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
                (
                    AgentManageSession,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
                (
                    AgentResolveApproval,
                    permits(RemoteActionClass::ResolvePermission),
                ),
                (
                    AgentRespondElicitation,
                    permits(RemoteActionClass::ResolveElicitation),
                ),
                (
                    AgentSwitchRuntime,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
                (
                    AgentAuthRead,
                    has_agent_account_auth && permits(RemoteActionClass::ReadAgentSession),
                ),
                (
                    AgentAuthManage,
                    has_agent_account_auth && permits(RemoteActionClass::MutateAgentAuthentication),
                ),
                (
                    AgentSidebarOrganizationRead,
                    permits(RemoteActionClass::ReadAgentSession),
                ),
                (
                    AgentSidebarOrganizationMutate,
                    permits(RemoteActionClass::MutateAgentSession),
                ),
            ])
        } else {
            unavailable()
        },
        workspace: if has_workbench {
            available_filtered([
                (
                    BackendOperation::WorkspaceList,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::WorkspaceOpen,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::WorkspaceDelete,
                    permits(RemoteActionClass::MutateFile),
                ),
            ])
        } else {
            unavailable()
        },
        file: if has_workbench {
            available_filtered([
                (
                    BackendOperation::FileTree,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::FileSearch,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::FileRead,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::FileWrite,
                    permits(RemoteActionClass::MutateFile),
                ),
            ])
        } else {
            unavailable()
        },
        git: if has_git {
            available_filtered([
                (
                    BackendOperation::GitStatus,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::GitDiff,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::GitStage,
                    permits(RemoteActionClass::MutateGit),
                ),
                (
                    BackendOperation::GitUnstage,
                    permits(RemoteActionClass::MutateGit),
                ),
                (
                    BackendOperation::GitCommit,
                    permits(RemoteActionClass::MutateGit),
                ),
                (
                    BackendOperation::GitWorktreeRead,
                    has_worktree_read && permits(RemoteActionClass::ReadProject),
                ),
            ])
        } else {
            unavailable()
        },
        terminal: if has_terminal {
            available_filtered([
                (
                    BackendOperation::TerminalList,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::TerminalCreate,
                    permits(RemoteActionClass::MutateTerminal),
                ),
                (
                    BackendOperation::TerminalAttach,
                    permits(RemoteActionClass::ReadProject),
                ),
                (
                    BackendOperation::TerminalInput,
                    permits(RemoteActionClass::MutateTerminal),
                ),
                (
                    BackendOperation::TerminalResize,
                    permits(RemoteActionClass::MutateTerminal),
                ),
                (
                    BackendOperation::TerminalClose,
                    permits(RemoteActionClass::MutateTerminal),
                ),
            ])
        } else {
            unavailable()
        },
        management: if has_provider {
            available_filtered([
                (
                    BackendOperation::ManagementProfiles,
                    permits(RemoteActionClass::ReadProviderSettings),
                ),
                (
                    BackendOperation::ManagementProviderProjectionRead,
                    permits(RemoteActionClass::ReadProviderSettings),
                ),
                (
                    BackendOperation::ManagementRuntimeProbeRead,
                    permits(RemoteActionClass::ReadProviderSettings),
                ),
                (
                    BackendOperation::ManagementRuntimeProbeMutate,
                    permits(RemoteActionClass::MutateProviderSettings),
                ),
                (
                    BackendOperation::ManagementHealth,
                    permits(RemoteActionClass::ReadProviderSettings),
                ),
            ])
        } else {
            unavailable()
        },
        device: if has_device {
            available_filtered([
                (
                    BackendOperation::DevicePairing,
                    has_device_pairing && permits(RemoteActionClass::MutateDeviceManagement),
                ),
                (
                    BackendOperation::DeviceList,
                    permits(RemoteActionClass::ReadDeviceManagement),
                ),
                (
                    BackendOperation::DeviceRevoke,
                    permits(RemoteActionClass::MutateDeviceManagement),
                ),
            ])
        } else {
            unavailable()
        },
    }
}

fn available_filtered(
    operations: impl IntoIterator<Item = (BackendOperation, bool)>,
) -> DomainCapabilities {
    DomainCapabilities::available(
        operations
            .into_iter()
            .filter_map(|(operation, allowed)| allowed.then_some(operation)),
    )
}

fn unavailable() -> DomainCapabilities {
    DomainCapabilities {
        availability: vibex_backend::CapabilityAvailability::Unsupported,
        operations: BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use vibex_core::{EventId, RemoteEventV2, RemotePing, unix_timestamp_ms};

    #[derive(Clone)]
    struct MockTransport {
        events: Arc<Mutex<VecDeque<RemoteTransportEvent>>>,
        responses: Arc<Mutex<VecDeque<vibex_core::RemoteRpcResponseV2>>>,
        requests: Arc<Mutex<Vec<vibex_core::RemoteRpcRequestV2>>>,
        server_info: Arc<Mutex<Option<vibex_core::RemoteServerInfoV2>>>,
        lifecycle_signals: Arc<Mutex<Vec<RemoteLifecycleSignal>>>,
    }

    impl MockTransport {
        fn new(events: impl IntoIterator<Item = RemoteTransportEvent>) -> Self {
            Self {
                events: Arc::new(Mutex::new(events.into_iter().collect())),
                responses: Arc::new(Mutex::new(VecDeque::new())),
                requests: Arc::new(Mutex::new(Vec::new())),
                server_info: Arc::new(Mutex::new(None)),
                lifecycle_signals: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_responses(
            responses: impl IntoIterator<Item = vibex_core::RemoteRpcResponseV2>,
        ) -> Self {
            Self {
                events: Arc::new(Mutex::new(VecDeque::new())),
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                requests: Arc::new(Mutex::new(Vec::new())),
                server_info: Arc::new(Mutex::new(None)),
                lifecycle_signals: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<vibex_core::RemoteRpcRequestV2> {
            self.requests.lock().expect("mock request lock").clone()
        }

        fn set_server_info(&self, server_info: vibex_core::RemoteServerInfoV2) {
            *self.server_info.lock().expect("mock server info lock") = Some(server_info);
        }

        fn lifecycle_signals(&self) -> Vec<RemoteLifecycleSignal> {
            self.lifecycle_signals
                .lock()
                .expect("mock lifecycle signal lock")
                .clone()
        }

        fn unavailable<T>() -> BackendFuture<'static, T> {
            Box::pin(async {
                Err(BackendError::unsupported(
                    "mock_unavailable",
                    "mock operation is unavailable",
                ))
            })
        }
    }

    impl RemoteTransport for MockTransport {
        fn state(&self) -> crate::transport::RemoteConnectionSnapshot {
            crate::transport::RemoteConnectionSnapshot {
                state: RemoteConnectionState::Online,
                ..Default::default()
            }
        }

        fn server_info(&self) -> Option<vibex_core::RemoteServerInfoV2> {
            self.server_info
                .lock()
                .expect("mock server info lock")
                .clone()
        }

        fn gateway_info(&self) -> Option<crate::transport::RemoteGatewayInfo> {
            None
        }

        fn connect(&self) -> BackendFuture<'_, vibex_core::RemoteServerInfoV2> {
            Self::unavailable()
        }

        fn disconnect(&self) -> BackendFuture<'_, ()> {
            Self::unavailable()
        }

        fn request(
            &self,
            request: vibex_core::RemoteRpcRequestV2,
        ) -> BackendFuture<'_, vibex_core::RemoteRpcResponseV2> {
            self.requests
                .lock()
                .expect("mock request lock")
                .push(request);
            let response = self
                .responses
                .lock()
                .expect("mock response lock")
                .pop_front();
            Box::pin(async move {
                response.ok_or_else(|| {
                    BackendError::unsupported("mock_unavailable", "mock operation is unavailable")
                })
            })
        }

        fn subscribe(
            &self,
            _request: vibex_core::RemoteSubscribeRequestV2,
        ) -> BackendFuture<'_, vibex_core::RemoteSubscriptionAcceptedV2> {
            Self::unavailable()
        }

        fn attach(
            &self,
            _request: vibex_core::RemoteAttachRequestV2,
        ) -> BackendFuture<'_, vibex_core::RemoteAttachmentAcceptedV2> {
            Self::unavailable()
        }

        fn detach(&self, _attachment_id: String) -> BackendFuture<'_, ()> {
            Self::unavailable()
        }

        fn send_binary(&self, _frame: vibex_core::RemoteBinaryFrame) -> BackendFuture<'_, ()> {
            Self::unavailable()
        }

        fn next_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
            let events = self.events.clone();
            Box::pin(async move {
                Ok(events
                    .lock()
                    .map_err(|_| {
                        BackendError::failed("mock_poisoned", "mock event queue is unavailable")
                    })?
                    .pop_front())
            })
        }

        fn heartbeat(&self) -> BackendFuture<'_, ()> {
            Self::unavailable()
        }

        fn apply_lifecycle_signal(&self, signal: RemoteLifecycleSignal) {
            self.lifecycle_signals
                .lock()
                .expect("mock lifecycle signal lock")
                .push(signal);
        }

        fn cursors(&self) -> Vec<vibex_core::RemoteStreamCursor> {
            Vec::new()
        }
    }

    fn inbound(decision: SyncDecision) -> RemoteTransportEvent {
        RemoteTransportEvent::Event(RemoteInboundEvent {
            event: RemoteEventV2 {
                event_id: EventId::new(),
                channel: "file".to_string(),
                generation: 1,
                sequence: 1,
                correlation_id: None,
                payload: None,
                emitted_at_ms: unix_timestamp_ms(),
            },
            decision,
        })
    }

    fn full_control_server_info(enabled_features: &[&str]) -> vibex_core::RemoteServerInfoV2 {
        vibex_core::RemoteServerInfoV2 {
            server_id: "server_test".to_string(),
            server_identity_public_key: "public".to_string(),
            desktop_version: "test".to_string(),
            protocol_range: vibex_core::RemoteProtocolVersionRange::v2(),
            selected_protocol: vibex_core::RemoteProtocolVersion { major: 2, minor: 0 },
            server_ephemeral_public_key: "ephemeral".to_string(),
            session_key_confirmation: "confirmation".to_string(),
            capabilities: Vec::new(),
            enabled_features: enabled_features
                .iter()
                .map(|feature| (*feature).to_string())
                .collect(),
            device_permissions: vibex_core::remote_permissions_for_level(
                vibex_core::RemoteDevicePermissionLevel::FullControl,
            ),
            session_epoch: 1,
            connection_id: vibex_core::RequestId::new(),
            server_time_ms: 0,
        }
    }

    #[test]
    fn event_subscription_skips_unrelated_controls_and_duplicate_events() {
        let transport: Arc<dyn RemoteTransport> = Arc::new(MockTransport::new([
            RemoteTransportEvent::Control(vibex_core::RemoteControlMessageV2::Pong(RemotePing {
                nonce: 1,
                sent_at_ms: 0,
            })),
            inbound(SyncDecision::IgnoreDuplicate),
            RemoteTransportEvent::Closed,
        ]));
        let mut subscription = RemoteEventSubscription { transport };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime.block_on(subscription.next()).unwrap(),
            Some(BackendEvent::Disconnected)
        );
    }

    #[test]
    fn event_subscription_keeps_running_after_non_domain_frames() {
        let transport: Arc<dyn RemoteTransport> = Arc::new(MockTransport::new([
            RemoteTransportEvent::Control(vibex_core::RemoteControlMessageV2::Pong(RemotePing {
                nonce: 2,
                sent_at_ms: 0,
            })),
            inbound(SyncDecision::Apply),
        ]));
        let mut subscription = RemoteEventSubscription { transport };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert!(matches!(
            runtime.block_on(subscription.next()).unwrap(),
            Some(BackendEvent::ProjectionInvalidated(
                vibex_backend::BackendProjection::Files
            ))
        ));
    }

    #[test]
    fn agent_notification_event_maps_to_the_typed_backend_event() {
        let notification = AgentNotificationIntent {
            notification_id: "turn-completed.session.event".to_string(),
            source_event_id: vibex_core::TimelineItemId::new(),
            session_id: vibex_core::VibexSessionId::new(),
            kind: vibex_core::AgentNotificationKind::TurnCompleted,
            created_at_ms: 1,
            expires_at_ms: 2,
            opaque_locator: "opaque-session".to_string(),
        };
        let mapped = map_remote_event(RemoteInboundEvent {
            event: RemoteEventV2 {
                event_id: EventId::new(),
                channel: "agent_notification".to_string(),
                generation: 1,
                sequence: 1,
                correlation_id: None,
                payload: Some(serde_json::to_value(&notification).unwrap()),
                emitted_at_ms: unix_timestamp_ms(),
            },
            decision: SyncDecision::Apply,
        });

        assert_eq!(mapped, BackendEvent::Notification(notification));
    }

    #[test]
    fn sidebar_event_maps_to_the_sidebar_projection() {
        let mapped = map_remote_event(RemoteInboundEvent {
            event: RemoteEventV2 {
                event_id: EventId::new(),
                channel: "sidebar".to_string(),
                generation: 1,
                sequence: 1,
                correlation_id: None,
                payload: None,
                emitted_at_ms: unix_timestamp_ms(),
            },
            decision: SyncDecision::Apply,
        });

        assert_eq!(
            mapped,
            BackendEvent::ProjectionInvalidated(vibex_backend::BackendProjection::Sidebar)
        );
    }

    #[test]
    fn projection_gap_is_live_but_control_resync_is_recovery_only() {
        let transport: Arc<dyn RemoteTransport> = Arc::new(MockTransport::new([
            inbound(SyncDecision::CatchUp {
                domain: "file".to_string(),
                generation: 1,
                after_cursor: 0,
            }),
            RemoteTransportEvent::Control(vibex_core::RemoteControlMessageV2::ResyncRequired(
                vibex_core::RemoteResyncRequired {
                    domain: "file".to_string(),
                    generation: 1,
                    reason: "reconnect".to_string(),
                    authoritative_operation: "file".to_string(),
                },
            )),
        ]));
        let mut subscription = RemoteEventSubscription { transport };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime.block_on(subscription.next()).unwrap(),
            Some(BackendEvent::ProjectionInvalidated(
                vibex_backend::BackendProjection::Files
            ))
        );
        assert!(matches!(
            runtime.block_on(subscription.next()).unwrap(),
            Some(BackendEvent::Lagged {
                observed_live: false,
                refetch: BackendRefetch {
                    projection: Some(vibex_backend::BackendProjection::Files),
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn remote_v1_capabilities_hide_dangerous_file_mutations() {
        let snapshot = remote_capabilities(None);
        assert!(snapshot.file.supports(BackendOperation::FileRead));
        assert!(snapshot.file.supports(BackendOperation::FileWrite));
        assert!(!snapshot.file.supports(BackendOperation::FileMove));
        assert!(!snapshot.file.supports(BackendOperation::FileDelete));
        assert!(!snapshot.git.supports(BackendOperation::GitWorktreeRead));
        assert!(!snapshot.git.supports(BackendOperation::GitWorktreeCreate));
        assert!(
            !snapshot
                .git
                .supports(BackendOperation::GitWorktreeLifecycleMutate)
        );
    }

    #[test]
    fn remote_agent_session_lifecycle_capabilities_follow_device_permissions() {
        let full = remote_capabilities(Some(&full_control_server_info(&["agent"])));
        assert!(full.agent.supports(BackendOperation::AgentCreateSession));
        assert!(full.agent.supports(BackendOperation::AgentManageSession));

        let mut read_only = full_control_server_info(&["agent"]);
        read_only.device_permissions = vibex_core::remote_permissions_for_level(
            vibex_core::RemoteDevicePermissionLevel::ReadOnly,
        );
        let read_only = remote_capabilities(Some(&read_only));
        assert!(
            read_only
                .agent
                .supports(BackendOperation::AgentListSessions)
        );
        assert!(
            !read_only
                .agent
                .supports(BackendOperation::AgentCreateSession)
        );
        assert!(
            !read_only
                .agent
                .supports(BackendOperation::AgentManageSession)
        );
    }

    #[test]
    fn remote_workspace_delete_capability_follows_device_permissions() {
        let full = remote_capabilities(Some(&full_control_server_info(&["workspace_file"])));
        assert!(full.workspace.supports(BackendOperation::WorkspaceList));
        assert!(full.workspace.supports(BackendOperation::WorkspaceDelete));

        let mut read_only = full_control_server_info(&["workspace_file"]);
        read_only.device_permissions = vibex_core::remote_permissions_for_level(
            vibex_core::RemoteDevicePermissionLevel::ReadOnly,
        );
        let read_only = remote_capabilities(Some(&read_only));
        assert!(
            read_only
                .workspace
                .supports(BackendOperation::WorkspaceList)
        );
        assert!(
            !read_only
                .workspace
                .supports(BackendOperation::WorkspaceDelete)
        );
    }

    #[test]
    fn negotiated_remote_exposes_worktree_read_but_never_lifecycle_mutation() {
        let info = full_control_server_info(&["git", "git_worktree_read"]);
        let snapshot = remote_capabilities(Some(&info));
        assert!(snapshot.git.supports(BackendOperation::GitWorktreeRead));
        assert!(!snapshot.git.supports(BackendOperation::GitWorktreeCreate));
        assert!(
            !snapshot
                .git
                .supports(BackendOperation::GitWorktreeLifecycleMutate)
        );
    }

    #[test]
    fn backend_forwards_mobile_lifecycle_to_the_transport_owner() {
        let transport = Arc::new(MockTransport::new([]));
        let backend = WebRemoteBackend::new(
            transport.clone(),
            RemoteAuthProof {
                device_id: vibex_core::DeviceId::new(),
                auth_token: "test-token".to_string(),
            },
        );

        backend.apply_lifecycle_signal(RemoteLifecycleSignal::AppBackgrounded);
        backend.apply_lifecycle_signal(RemoteLifecycleSignal::AppResumed);

        assert_eq!(
            transport.lifecycle_signals(),
            vec![
                RemoteLifecycleSignal::AppBackgrounded,
                RemoteLifecycleSignal::AppResumed,
            ]
        );
    }

    #[tokio::test]
    async fn remote_lifecycle_mutations_fail_with_one_stable_capability_code() {
        let backend = WebRemoteBackend::new(
            Arc::new(MockTransport::new([])),
            RemoteAuthProof {
                device_id: vibex_core::DeviceId::new(),
                auth_token: "test-token".to_string(),
            },
        );
        let error = backend
            .git_worktree_set_readiness(MutationRequest::new(GitWorktreeReadinessRequest {
                workspace_id: WorkspaceId::new(),
                state: vibex_core::GitWorktreeReadinessState::Reviewing,
                expected_source_head: None,
                expected_dirty_fingerprint: None,
                checks: Vec::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code, "remote_worktree_mutation_unsupported");
    }

    #[tokio::test]
    async fn remote_provider_projection_keeps_entities_and_secret_mutation_private() {
        let backend = WebRemoteBackend::new(
            Arc::new(MockTransport::new([])),
            RemoteAuthProof {
                device_id: vibex_core::DeviceId::new(),
                auth_token: "test-token".to_string(),
            },
        );

        assert_eq!(
            backend
                .list_model_provider_profiles()
                .await
                .unwrap_err()
                .code,
            "remote_model_provider_profiles_private"
        );
        assert_eq!(
            backend
                .list_agent_runtime_profiles(vibex_core::AgentId::parse("codex").unwrap())
                .await
                .unwrap_err()
                .code,
            "remote_agent_runtime_profiles_private"
        );
        assert_eq!(
            backend
                .list_agent_model_provider_bindings(
                    vibex_core::AgentModelProviderBindingListRequest {
                        agent_id: Some(vibex_core::AgentId::parse("codex").unwrap()),
                        model_provider_profile_id: None,
                    },
                )
                .await
                .unwrap_err()
                .code,
            "remote_agent_provider_bindings_private"
        );

        let secret = "remote-secret-must-stay-local";
        let error = backend
            .mutate_provider_credential_secret(MutationRequest::new(
                vibex_core::ProviderCredentialSecretMutationRequest {
                    model_provider_profile_id: vibex_core::ModelProviderProfileId::new(),
                    credential_id: vibex_core::RequestId::new(),
                    touched: true,
                    clear: false,
                    value: Some(secret.to_string()),
                },
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code, "remote_provider_secret_mutation_unavailable");
        assert!(!format!("{error:?}").contains(secret));
    }

    #[tokio::test]
    async fn remote_agent_config_summaries_use_the_redacted_provider_projection() {
        let expected = vibex_core::RemoteAgentConfigSummary {
            id: vibex_core::AgentId::parse("codex").unwrap(),
            label: "Codex".to_string(),
            enabled: true,
            installed: true,
            configured: true,
            config_status: vibex_core::AgentConfigStatus::Configured,
            runtime_status: vibex_core::AgentRuntimeStatus::Ready,
            model_count: 2,
            updated_at_ms: Some(11),
        };
        let transport = Arc::new(MockTransport::with_responses([
            vibex_core::RemoteRpcResponseV2 {
                request_id: vibex_core::RequestId::new(),
                correlation_id: None,
                payload: Some(
                    serde_json::to_value(vibex_core::RemoteAgentConfigSummaryListResponse {
                        agents: vec![expected.clone()],
                    })
                    .unwrap(),
                ),
                error: None,
                metadata: Default::default(),
                completed_at_ms: unix_timestamp_ms(),
            },
        ]));
        let backend = WebRemoteBackend::new(
            transport.clone(),
            RemoteAuthProof {
                device_id: vibex_core::DeviceId::new(),
                auth_token: "test-token".to_string(),
            },
        );

        assert_eq!(
            backend.list_agent_config_summaries(true).await.unwrap(),
            vec![expected]
        );
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].operation, "provider_settings");
        assert_eq!(
            requests[0].payload.as_ref().unwrap()["type"],
            "list_agent_summaries"
        );
        assert_eq!(
            requests[0].payload.as_ref().unwrap()["data"]["includeDisabled"],
            true
        );
    }

    #[test]
    fn remote_management_exposes_redacted_profiles_health_and_runtime_probes() {
        let snapshot = remote_capabilities(None);
        assert!(
            snapshot
                .management
                .supports(BackendOperation::ManagementProfiles)
        );
        assert!(
            snapshot
                .management
                .supports(BackendOperation::ManagementHealth)
        );
        assert!(
            snapshot
                .management
                .supports(BackendOperation::ManagementRuntimeProbeRead)
        );
        assert!(
            snapshot
                .management
                .supports(BackendOperation::ManagementRuntimeProbeMutate)
        );
        assert!(
            !snapshot
                .management
                .supports(BackendOperation::ManagementProfileSelect)
        );
        assert!(matches!(
            snapshot.device.availability,
            vibex_backend::CapabilityAvailability::Unsupported
        ));
    }

    #[test]
    fn remote_runtime_probe_capabilities_follow_provider_permissions() {
        let mut info = full_control_server_info(&["provider_settings"]);
        info.device_permissions = vec![RemoteActionClass::ReadProviderSettings];

        let snapshot = remote_capabilities(Some(&info));

        assert!(
            snapshot
                .management
                .supports(BackendOperation::ManagementRuntimeProbeRead)
        );
        assert!(
            !snapshot
                .management
                .supports(BackendOperation::ManagementRuntimeProbeMutate)
        );
    }

    #[test]
    fn negotiated_action_permissions_distinguish_provider_reads_from_mutations() {
        let transport = Arc::new(MockTransport::new([]));
        let mut info = full_control_server_info(&["provider_settings"]);
        info.device_permissions = vec![RemoteActionClass::ReadProviderSettings];
        transport.set_server_info(info.clone());
        let backend = WebRemoteBackend::new(
            transport.clone(),
            RemoteAuthProof {
                device_id: vibex_core::DeviceId::new(),
                auth_token: "test-token".to_string(),
            },
        );

        assert!(backend.permits_remote_action(RemoteActionClass::ReadProviderSettings));
        assert!(!backend.permits_remote_action(RemoteActionClass::MutateProviderSettings));

        info.device_permissions = vibex_core::remote_permissions_for_level(
            vibex_core::RemoteDevicePermissionLevel::FullControl,
        );
        transport.set_server_info(info);
        assert!(backend.permits_remote_action(RemoteActionClass::MutateProviderSettings));
    }

    #[test]
    fn shared_facade_tracks_capability_additions_and_removals_after_reconnect() {
        let transport = Arc::new(MockTransport::new([]));
        let backend = Arc::new(WebRemoteBackend::new(
            transport.clone(),
            RemoteAuthProof {
                device_id: vibex_core::DeviceId::new(),
                auth_token: "test-token".to_string(),
            },
        ));
        let facade = backend.facade();

        assert!(
            !facade
                .capabilities()
                .device
                .supports(BackendOperation::DevicePairing)
        );

        transport.set_server_info(full_control_server_info(&[
            "device_management",
            "device_pairing",
        ]));
        assert!(
            backend
                .capability_snapshot()
                .device
                .supports(BackendOperation::DevicePairing)
        );
        assert!(
            facade
                .capabilities()
                .device
                .supports(BackendOperation::DevicePairing)
        );

        transport.set_server_info(full_control_server_info(&["device_management"]));
        assert!(
            !backend
                .capability_snapshot()
                .device
                .supports(BackendOperation::DevicePairing)
        );
        assert!(
            !facade
                .capabilities()
                .device
                .supports(BackendOperation::DevicePairing)
        );
    }

    #[test]
    fn full_control_v2_device_management_is_advertised_only_when_negotiated() {
        let info = full_control_server_info(&["device_management", "device_pairing"]);
        let snapshot = remote_capabilities(Some(&info));
        assert!(snapshot.device.supports(BackendOperation::DevicePairing));
        assert!(snapshot.device.supports(BackendOperation::DeviceList));
        assert!(snapshot.device.supports(BackendOperation::DeviceRevoke));

        let mut without_pairing_route = info;
        without_pairing_route
            .enabled_features
            .retain(|feature| feature != "device_pairing");
        let snapshot = remote_capabilities(Some(&without_pairing_route));
        assert!(!snapshot.device.supports(BackendOperation::DevicePairing));
        assert!(snapshot.device.supports(BackendOperation::DeviceList));
    }

    #[test]
    fn agent_account_auth_is_advertised_only_when_negotiated_and_permitted() {
        let info = full_control_server_info(&["agent", "agent_account_auth"]);
        let snapshot = remote_capabilities(Some(&info));
        assert!(snapshot.agent.supports(BackendOperation::AgentAuthRead));
        assert!(snapshot.agent.supports(BackendOperation::AgentAuthManage));

        let without_feature = full_control_server_info(&["agent"]);
        let snapshot = remote_capabilities(Some(&without_feature));
        assert!(!snapshot.agent.supports(BackendOperation::AgentAuthRead));
        assert!(!snapshot.agent.supports(BackendOperation::AgentAuthManage));

        let mut approve_only = info.clone();
        approve_only.device_permissions = vibex_core::remote_permissions_for_level(
            vibex_core::RemoteDevicePermissionLevel::ApproveOnly,
        );
        let snapshot = remote_capabilities(Some(&approve_only));
        assert!(snapshot.agent.supports(BackendOperation::AgentAuthRead));
        assert!(!snapshot.agent.supports(BackendOperation::AgentAuthManage));

        let mut read_only = info;
        read_only.device_permissions = vibex_core::remote_permissions_for_level(
            vibex_core::RemoteDevicePermissionLevel::ReadOnly,
        );
        let snapshot = remote_capabilities(Some(&read_only));
        assert!(snapshot.agent.supports(BackendOperation::AgentAuthRead));
        assert!(!snapshot.agent.supports(BackendOperation::AgentAuthManage));
    }
}

use vibex_core::{
    AgentSession, AgentSessionRuntimeSelectionEvent, AgentSessionRuntimeSelectionState,
    AgentUsageStatistics, AgentUsageStatisticsRequest, CancelAgentSessionRuntimeSwitchRequest,
    ContinueAgentTurnRequest, CreateAgentSessionRequest, FetchTimelineRequest,
    RemoteDeepLinkResolution, RenameAgentSessionRequest, ResolvePermissionRequest,
    RuntimeSessionEvent, SendAgentMessageRequest, SessionRuntimeOptionCatalog,
    SetDesiredAgentSessionRuntimeRequest, TimelineItem, TimelineLiveEvent, TimelinePage,
    VibexSessionId,
};

use crate::{BackendBound, BackendFuture, BackendResult, MutationRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendEventStream {
    Timeline,
    Runtime,
    RuntimeSelection,
    Usage,
    Fanout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendProjection {
    Files,
    Git,
    Management,
    Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRefetch {
    pub session_id: Option<VibexSessionId>,
    pub timeline: bool,
    pub runtime: bool,
    pub runtime_selection: bool,
    pub projection: Option<BackendProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEvent {
    Timeline(TimelineLiveEvent),
    Runtime(RuntimeSessionEvent),
    RuntimeSelection(AgentSessionRuntimeSelectionEvent),
    ProjectionInvalidated(BackendProjection),
    Lagged {
        stream: BackendEventStream,
        skipped: u64,
        refetch: BackendRefetch,
        observed_live: bool,
    },
    Disconnected,
}

pub trait BackendEventSubscription: BackendBound {
    fn next(&mut self) -> BackendFuture<'_, Option<BackendEvent>>;
}

pub trait AgentBackend: BackendBound {
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>>;

    fn list_sessions(&self, include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>>;

    fn open_session(&self, session_id: VibexSessionId) -> BackendFuture<'_, AgentSession>;

    fn create_session(
        &self,
        request: MutationRequest<CreateAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession>;

    fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage>;

    fn usage_statistics(
        &self,
        _request: AgentUsageStatisticsRequest,
    ) -> BackendFuture<'_, AgentUsageStatistics> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_usage_statistics_unavailable",
                "Agent usage statistics are unavailable on this backend",
            ))
        })
    }

    /// Resolve a short-lived push/deep-link locator on the authoritative PC.
    /// Native backends that do not expose push routing keep the explicit
    /// unsupported default; remote backends override it with the typed RPC.
    fn resolve_opaque_locator(
        &self,
        _notification_id: String,
        _opaque_locator: String,
    ) -> BackendFuture<'_, RemoteDeepLinkResolution> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "remote_deep_link_unavailable",
                "opaque deep-link resolution is unavailable on this backend",
            ))
        })
    }

    fn send_message(
        &self,
        request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>>;

    fn continue_turn(
        &self,
        request: MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>>;

    fn interrupt(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, bool>;

    fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem>;

    fn rename_session(
        &self,
        request: MutationRequest<RenameAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession>;

    fn archive_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()>;

    fn delete_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()>;

    fn list_runtime_options(&self) -> BackendFuture<'_, SessionRuntimeOptionCatalog>;

    fn runtime_selection(
        &self,
        session_id: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState>;

    fn set_desired_runtime(
        &self,
        request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState>;

    fn cancel_runtime_switch(
        &self,
        request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState>;
}

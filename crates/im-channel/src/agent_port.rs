use async_trait::async_trait;
use tokio::sync::broadcast;
use vibex_core::{
    AgentNotificationIntent, AgentSession, AgentSessionRuntimeSelectionState,
    CreateAgentSessionRequest, FetchTimelineRequest, ResolveElicitationRequest,
    ResolvePermissionRequest, SendAgentMessageRequest, TimelineItem, TimelineLiveEvent,
    TimelinePage, VibexResult, VibexSessionId,
};

/// The narrow slice of the authoritative Agent runtime an IM channel needs.
///
/// Keeping this a port rather than depending on `vibex-agent` lets the channel
/// run against a mock in tests, and keeps the IM crate out of the ACP/provider
/// dependency graph.
#[async_trait]
pub trait ImAgentPort: Send + Sync + 'static {
    fn subscribe_timeline(&self) -> broadcast::Receiver<TimelineLiveEvent>;
    fn subscribe_session_updates(&self) -> broadcast::Receiver<AgentSession>;
    fn subscribe_notifications(&self) -> broadcast::Receiver<AgentNotificationIntent>;

    async fn get_session(&self, session_id: &VibexSessionId) -> VibexResult<AgentSession>;
    async fn list_sessions(&self, include_archived: bool) -> VibexResult<Vec<AgentSession>>;
    async fn fetch_timeline(&self, request: FetchTimelineRequest) -> VibexResult<TimelinePage>;
    async fn runtime_selection(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState>;
    async fn create_session(&self, request: CreateAgentSessionRequest)
    -> VibexResult<AgentSession>;
    async fn send_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>>;
    async fn interrupt(&self, session_id: &VibexSessionId) -> VibexResult<()>;
    async fn resolve_permission(
        &self,
        request: ResolvePermissionRequest,
    ) -> VibexResult<TimelineItem>;
    async fn resolve_elicitation(
        &self,
        request: ResolveElicitationRequest,
    ) -> VibexResult<TimelineItem>;
}

/// The Agent port for administrative commands that never run a turn.
///
/// `vibex-server im list|pair|revoke|test` edits channel configuration, pairing
/// codes and bindings; none of it dispatches a prompt or resolves an approval.
/// Rather than start a runtime to satisfy the signature, those commands build
/// the service with this port and call only the administrative methods. Every
/// Agent operation fails loudly, so a future caller that reaches one gets a
/// capability error instead of silence.
pub struct AdministrativeAgent;

#[async_trait]
impl ImAgentPort for AdministrativeAgent {
    fn subscribe_timeline(&self) -> broadcast::Receiver<TimelineLiveEvent> {
        broadcast::channel(1).0.subscribe()
    }

    fn subscribe_session_updates(&self) -> broadcast::Receiver<AgentSession> {
        broadcast::channel(1).0.subscribe()
    }

    fn subscribe_notifications(&self) -> broadcast::Receiver<AgentNotificationIntent> {
        broadcast::channel(1).0.subscribe()
    }

    async fn get_session(&self, _session_id: &VibexSessionId) -> VibexResult<AgentSession> {
        Err(no_agent())
    }

    async fn list_sessions(&self, _include_archived: bool) -> VibexResult<Vec<AgentSession>> {
        Ok(Vec::new())
    }

    async fn fetch_timeline(&self, _request: FetchTimelineRequest) -> VibexResult<TimelinePage> {
        Err(no_agent())
    }

    async fn runtime_selection(
        &self,
        _session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        Err(no_agent())
    }

    async fn create_session(
        &self,
        _request: CreateAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        Err(no_agent())
    }

    async fn send_message(
        &self,
        _request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        Err(no_agent())
    }

    async fn interrupt(&self, _session_id: &VibexSessionId) -> VibexResult<()> {
        Err(no_agent())
    }

    async fn resolve_permission(
        &self,
        _request: ResolvePermissionRequest,
    ) -> VibexResult<TimelineItem> {
        Err(no_agent())
    }

    async fn resolve_elicitation(
        &self,
        _request: ResolveElicitationRequest,
    ) -> VibexResult<TimelineItem> {
        Err(no_agent())
    }
}

fn no_agent() -> vibex_core::VibexError {
    vibex_core::VibexError::capability(
        "im_administrative_only",
        "this channel service was built for administration and hosts no Agent",
    )
}

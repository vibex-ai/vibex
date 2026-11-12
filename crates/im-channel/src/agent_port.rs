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

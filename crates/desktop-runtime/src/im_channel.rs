//! IM channel host wired to the authoritative Agent runtime.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use vibex_agent::{AgentManager, RuntimeSelectionService};
use vibex_core::{
    AgentNotificationIntent, AgentSession, AgentSessionRuntimeSelectionState,
    CreateAgentSessionRequest, FetchTimelineRequest, ResolveElicitationRequest,
    ResolvePermissionRequest, SendAgentMessageRequest, TimelineItem, TimelineLiveEvent,
    TimelinePage, VibexResult, VibexSessionId,
};
use vibex_im_channel::ImAgentPort;

pub struct AgentManagerImPort {
    manager: Arc<AgentManager>,
    runtime_selection: Arc<RuntimeSelectionService>,
}

impl AgentManagerImPort {
    pub fn new(
        manager: Arc<AgentManager>,
        runtime_selection: Arc<RuntimeSelectionService>,
    ) -> Self {
        Self {
            manager,
            runtime_selection,
        }
    }
}

#[async_trait]
impl ImAgentPort for AgentManagerImPort {
    fn subscribe_timeline(&self) -> broadcast::Receiver<TimelineLiveEvent> {
        self.manager.subscribe()
    }

    fn subscribe_session_updates(&self) -> broadcast::Receiver<AgentSession> {
        self.manager.subscribe_session_updates()
    }

    fn subscribe_notifications(&self) -> broadcast::Receiver<AgentNotificationIntent> {
        self.manager.subscribe_notifications()
    }

    async fn get_session(&self, session_id: &VibexSessionId) -> VibexResult<AgentSession> {
        self.manager.get_session(session_id).await
    }

    async fn list_sessions(&self, include_archived: bool) -> VibexResult<Vec<AgentSession>> {
        self.manager.list_sessions(include_archived).await
    }

    async fn fetch_timeline(&self, request: FetchTimelineRequest) -> VibexResult<TimelinePage> {
        self.manager.fetch_timeline(request).await
    }

    async fn runtime_selection(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        self.runtime_selection.get_selection_state(session_id)
    }

    async fn create_session(
        &self,
        request: CreateAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        self.manager.create_session(request).await
    }

    async fn send_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        self.manager.send_message(request).await
    }

    async fn interrupt(&self, session_id: &VibexSessionId) -> VibexResult<()> {
        self.manager.interrupt(session_id).await
    }

    async fn resolve_permission(
        &self,
        request: ResolvePermissionRequest,
    ) -> VibexResult<TimelineItem> {
        self.manager.resolve_permission(request).await
    }

    async fn resolve_elicitation(
        &self,
        request: ResolveElicitationRequest,
    ) -> VibexResult<TimelineItem> {
        self.manager.resolve_elicitation(request).await
    }
}

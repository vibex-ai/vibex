use std::sync::Arc;

use vibex_backend::{
    AgentBackend, BackendResult, MutationRequest, WorkspaceBackend, WorkspaceSummary,
};
use vibex_core::{
    AgentSession, FetchTimelineRequest, SendAgentMessageRequest, TimelineItem, TimelinePage,
    VibexSessionId,
};

use crate::AsyncState;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionControllerState {
    pub sessions: AsyncState<Vec<AgentSession>>,
    pub active_session: AsyncState<AgentSession>,
    pub timeline: AsyncState<TimelinePage>,
    pub latest_mutation: AsyncState<Vec<TimelineItem>>,
}

pub struct SessionController {
    backend: Arc<dyn AgentBackend>,
    pub state: SessionControllerState,
}

impl SessionController {
    pub fn new(backend: Arc<dyn AgentBackend>) -> Self {
        Self {
            backend,
            state: SessionControllerState::default(),
        }
    }

    pub async fn refresh_sessions(&mut self, include_archived: bool) -> BackendResult<()> {
        self.state.sessions.begin();
        match self.backend.list_sessions(include_archived).await {
            Ok(sessions) => {
                self.state.sessions.resolve(sessions);
                Ok(())
            }
            Err(error) => {
                self.state.sessions.reject(error.clone());
                Err(error)
            }
        }
    }

    pub async fn open_session(
        &mut self,
        session_id: VibexSessionId,
        timeline_limit: u32,
    ) -> BackendResult<()> {
        self.state.active_session.begin();
        self.state.timeline.begin();
        let session = match self.backend.open_session(session_id.clone()).await {
            Ok(session) => session,
            Err(error) => {
                self.state.active_session.reject(error.clone());
                self.state.timeline.reject(error.clone());
                return Err(error);
            }
        };
        self.state.active_session.resolve(session);
        match self
            .backend
            .fetch_timeline(FetchTimelineRequest {
                session_id,
                after_sequence: None,
                limit: timeline_limit.max(1),
            })
            .await
        {
            Ok(page) => {
                self.state.timeline.resolve(page);
                Ok(())
            }
            Err(error) => {
                self.state.timeline.reject(error.clone());
                Err(error)
            }
        }
    }

    pub async fn send_message(
        &mut self,
        request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendResult<()> {
        self.state.latest_mutation.begin();
        match self.backend.send_message(request).await {
            Ok(items) => {
                self.state.latest_mutation.resolve(items);
                Ok(())
            }
            Err(error) => {
                self.state.latest_mutation.reject(error.clone());
                Err(error)
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceControllerState {
    pub workspaces: AsyncState<Vec<WorkspaceSummary>>,
}

pub struct WorkspaceController {
    backend: Arc<dyn WorkspaceBackend>,
    pub state: WorkspaceControllerState,
}

impl WorkspaceController {
    pub fn new(backend: Arc<dyn WorkspaceBackend>) -> Self {
        Self {
            backend,
            state: WorkspaceControllerState::default(),
        }
    }

    pub async fn refresh(&mut self) -> BackendResult<()> {
        self.state.workspaces.begin();
        match self.backend.list_workspaces().await {
            Ok(workspaces) => {
                self.state.workspaces.resolve(workspaces);
                Ok(())
            }
            Err(error) => {
                self.state.workspaces.reject(error.clone());
                Err(error)
            }
        }
    }
}

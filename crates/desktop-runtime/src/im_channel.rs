//! IM channel host wired to the authoritative Agent runtime.

use std::sync::Arc;

use crate::catalog::RuntimeOptionCatalogService;
use async_trait::async_trait;
use tokio::sync::broadcast;
use vibex_agent::{AgentManager, RuntimeSelectionService};
use vibex_core::{
    AgentNotificationIntent, AgentSession, AgentSessionRuntimeSelectionState,
    CreateAgentSessionRequest, FetchTimelineRequest, ImRuntimeAgentChoice, ImRuntimeChoice,
    ImRuntimeOptions, ResolveElicitationRequest, ResolvePermissionRequest,
    RuntimeOptionAvailability, SendAgentMessageRequest, SetDesiredAgentSessionRuntimeRequest,
    TimelineItem, TimelineLiveEvent, TimelinePage, VibexResult, VibexSessionId,
};
use vibex_im_channel::ImAgentPort;

pub struct AgentManagerImPort {
    manager: Arc<AgentManager>,
    runtime_selection: Arc<RuntimeSelectionService>,
    catalog: Arc<RuntimeOptionCatalogService>,
}

impl AgentManagerImPort {
    pub fn new(
        manager: Arc<AgentManager>,
        runtime_selection: Arc<RuntimeSelectionService>,
        catalog: Arc<RuntimeOptionCatalogService>,
    ) -> Self {
        Self {
            manager,
            runtime_selection,
            catalog,
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

    async fn submit_message(&self, request: SendAgentMessageRequest) -> VibexResult<()> {
        self.manager.submit_message_in_background(request)
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

    async fn set_desired_runtime(
        &self,
        request: SetDesiredAgentSessionRuntimeRequest,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        self.runtime_selection.set_desired_runtime(request).await
    }

    async fn runtime_options(&self, session_id: &VibexSessionId) -> VibexResult<ImRuntimeOptions> {
        let state = self.runtime_selection.get_selection_state(session_id)?;
        let catalog = self.catalog.list().await?;
        let current = state.desired.clone();
        let selectable = catalog
            .options
            .iter()
            .filter(|option| option.availability == RuntimeOptionAvailability::Available);

        let mut models: Vec<ImRuntimeChoice> = Vec::new();
        for option in catalog.options.iter().filter(|option| {
            option.availability == RuntimeOptionAvailability::Available
                && option.selection.agent_id == current.agent_id
                && option.selection.auth_source == current.auth_source
        }) {
            let Some(model_id) = option.selection.model.model_id() else {
                continue;
            };
            if models.iter().any(|choice| choice.id == model_id) {
                continue;
            }
            models.push(ImRuntimeChoice {
                id: model_id.to_string(),
                label: option.model_label.clone(),
                current: current.model.model_id() == Some(model_id),
            });
        }

        // Effort and mode are advertised per model, so the base is the option
        // for the selection in force, not any option of the same Agent.
        let base = selectable
            .clone()
            .find(|option| option.selection == current)
            .or_else(|| {
                selectable.clone().find(|option| {
                    option.selection.agent_id == current.agent_id
                        && option.selection.auth_source == current.auth_source
                })
            });

        let default_effort = ImRuntimeChoice {
            id: String::new(),
            label: "Default".to_string(),
            current: current.reasoning_effort.is_none(),
        };
        let default_mode = ImRuntimeChoice {
            id: String::new(),
            label: "Default".to_string(),
            current: current.mode_id.is_none(),
        };
        let mut efforts = vec![default_effort];
        let mut modes = vec![default_mode];
        if let Some(base) = base {
            for value in &base.reasoning_efforts {
                efforts.push(ImRuntimeChoice {
                    id: value.value.clone(),
                    label: value.label.clone().unwrap_or_else(|| value.value.clone()),
                    current: current.reasoning_effort.as_deref() == Some(value.value.as_str()),
                });
            }
            for value in &base.modes {
                modes.push(ImRuntimeChoice {
                    id: value.value.clone(),
                    label: value.label.clone().unwrap_or_else(|| value.value.clone()),
                    current: current.mode_id.as_deref() == Some(value.value.as_str()),
                });
            }
        }

        let mut agents: Vec<ImRuntimeAgentChoice> = Vec::new();
        for option in selectable.clone() {
            if agents.iter().any(|choice| {
                choice.agent_id == option.selection.agent_id
                    && choice.auth_source == option.selection.auth_source
            }) {
                continue;
            }
            agents.push(ImRuntimeAgentChoice {
                agent_id: option.selection.agent_id.clone(),
                auth_source: option.selection.auth_source.clone(),
                model: option.selection.model.clone(),
                agent_label: option.agent_label.clone(),
                source_label: option.auth_source_label.clone(),
                current: option.selection.agent_id == current.agent_id
                    && option.selection.auth_source == current.auth_source,
            });
        }

        let matched = selectable
            .clone()
            .find(|option| option.selection == current);
        let agent_label = matched
            .map(|option| option.agent_label.clone())
            .or_else(|| {
                catalog
                    .agents
                    .iter()
                    .find(|agent| agent.agent_id == current.agent_id)
                    .map(|agent| agent.label.clone())
            })
            .unwrap_or_else(|| current.agent_id.as_str().to_string());
        let model_label = matched
            .map(|option| option.model_label.clone())
            .or_else(|| current.model.model_id().map(str::to_string))
            .unwrap_or_else(|| "Agent default".to_string());

        Ok(ImRuntimeOptions {
            session_revision: state.session_revision,
            selection_revision: state.selection_revision,
            agent_label,
            model_label,
            effort_label: current.reasoning_effort.clone(),
            mode_label: current.mode_id.clone(),
            current,
            agents,
            models,
            efforts,
            modes,
        })
    }
}

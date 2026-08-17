use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vibex_backend::{
    AgentBackend, BackendError, BackendEvent, BackendFuture, BackendOperation, BackendResult,
    DomainCapabilities, MutationRequest,
};
use vibex_core::{
    AgentSession, AgentSessionRuntimeSelectionState, ContinueAgentTurnRequest,
    CreateAgentSessionRequest, ElicitationAnswerValue, ElicitationFieldKind, ElicitationRequest,
    ElicitationRequestStatus, ElicitationResolution, ElicitationResolutionAction,
    FetchTimelineRequest, PermissionRequestStatus, RenameAgentSessionRequest,
    ResolveElicitationRequest, ResolvePermissionRequest, SendAgentMessageRequest,
    SessionRuntimeOptionCatalog, TimelineItem, TimelineLiveEvent, TimelinePage, TimelinePayload,
    VibexError, VibexResult, VibexSessionId,
};
use vibex_desktop_model::{
    AgentSidebarRow, SidebarState, TimelineConversationTurn, TimelineModel, project_sidebar_rows,
};

use crate::{
    AgentWorkflowView, ApprovalSurfaceModel, AsyncPhase, AsyncState, ElicitationSurfaceModel,
    ShellKind, WorkflowViewGeneration,
};

pub const AGENT_TIMELINE_PAGE_LIMIT: u32 = 500;
pub const AGENT_TIMELINE_MAX_ITEMS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElicitationDraftValue {
    Text(String),
    Boolean(bool),
    MultiSelect(BTreeSet<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElicitationFormDraft {
    pub request_id: vibex_core::RequestId,
    pub values: BTreeMap<String, ElicitationDraftValue>,
}

impl ElicitationFormDraft {
    pub fn from_request(request: &ElicitationRequest) -> Self {
        let values = request
            .fields
            .iter()
            .filter_map(|field| {
                let value = match &field.kind {
                    ElicitationFieldKind::Text { default, .. }
                    | ElicitationFieldKind::Number { default, .. } => {
                        default.clone().map(ElicitationDraftValue::Text)
                    }
                    ElicitationFieldKind::Integer { default, .. } => {
                        default.map(|value| ElicitationDraftValue::Text(value.to_string()))
                    }
                    ElicitationFieldKind::Boolean { default } => {
                        default.map(ElicitationDraftValue::Boolean)
                    }
                    ElicitationFieldKind::MultiSelect { default, .. } => Some(
                        ElicitationDraftValue::MultiSelect(default.iter().cloned().collect()),
                    ),
                    ElicitationFieldKind::Unsupported { .. } => None,
                }?;
                Some((field.id.clone(), value))
            })
            .collect();
        Self {
            request_id: request.id.clone(),
            values,
        }
    }

    pub fn set_text(&mut self, field_id: impl Into<String>, value: impl Into<String>) {
        self.values
            .insert(field_id.into(), ElicitationDraftValue::Text(value.into()));
    }

    pub fn set_boolean(&mut self, field_id: impl Into<String>, value: bool) {
        self.values
            .insert(field_id.into(), ElicitationDraftValue::Boolean(value));
    }

    pub fn select_option(&mut self, field_id: impl Into<String>, value: impl Into<String>) {
        self.set_text(field_id, value);
    }

    pub fn toggle_multi_option(&mut self, field_id: impl Into<String>, value: impl Into<String>) {
        let field_id = field_id.into();
        let value = value.into();
        let selected = self
            .values
            .entry(field_id)
            .or_insert_with(|| ElicitationDraftValue::MultiSelect(BTreeSet::new()));
        if let ElicitationDraftValue::MultiSelect(selected) = selected
            && !selected.insert(value.clone())
        {
            selected.remove(&value);
        }
    }

    pub fn text(&self, field_id: &str) -> Option<&str> {
        match self.values.get(field_id) {
            Some(ElicitationDraftValue::Text(value)) => Some(value),
            _ => None,
        }
    }

    pub fn boolean(&self, field_id: &str) -> Option<bool> {
        match self.values.get(field_id) {
            Some(ElicitationDraftValue::Boolean(value)) => Some(*value),
            _ => None,
        }
    }

    pub fn multi_selected(&self, field_id: &str, value: &str) -> bool {
        matches!(
            self.values.get(field_id),
            Some(ElicitationDraftValue::MultiSelect(selected)) if selected.contains(value)
        )
    }

    pub fn resolve_request(
        &self,
        request: &ElicitationRequest,
        action: ElicitationResolutionAction,
        resolved_at_ms: i64,
    ) -> VibexResult<ResolveElicitationRequest> {
        if self.request_id != request.id {
            return Err(VibexError::validation(
                "elicitation_draft_request_mismatch",
                "input draft belongs to another request",
            ));
        }
        let mut answers = BTreeMap::new();
        if action == ElicitationResolutionAction::Accept {
            for field in &request.fields {
                let answer = match (&field.kind, self.values.get(&field.id)) {
                    (
                        ElicitationFieldKind::Text { .. },
                        Some(ElicitationDraftValue::Text(value)),
                    ) if field.required || !value.is_empty() => {
                        Some(ElicitationAnswerValue::String(value.clone()))
                    }
                    (
                        ElicitationFieldKind::Number { .. },
                        Some(ElicitationDraftValue::Text(value)),
                    ) if field.required || !value.is_empty() => {
                        Some(ElicitationAnswerValue::Number(value.clone()))
                    }
                    (
                        ElicitationFieldKind::Integer { .. },
                        Some(ElicitationDraftValue::Text(value)),
                    ) if field.required || !value.is_empty() => Some(
                        ElicitationAnswerValue::Integer(value.parse::<i64>().map_err(|_| {
                            VibexError::validation(
                                "elicitation_answer_invalid",
                                "integer answer is invalid",
                            )
                            .with_diagnostic("fieldId", &field.id)
                        })?),
                    ),
                    (
                        ElicitationFieldKind::Boolean { .. },
                        Some(ElicitationDraftValue::Boolean(value)),
                    ) => Some(ElicitationAnswerValue::Boolean(*value)),
                    (ElicitationFieldKind::Boolean { .. }, None) if field.required => {
                        Some(ElicitationAnswerValue::Boolean(false))
                    }
                    (
                        ElicitationFieldKind::MultiSelect { .. },
                        Some(ElicitationDraftValue::MultiSelect(values)),
                    ) if field.required || !values.is_empty() => Some(
                        ElicitationAnswerValue::StringArray(values.iter().cloned().collect()),
                    ),
                    (ElicitationFieldKind::Unsupported { .. }, _) if field.required => {
                        return Err(VibexError::capability(
                            "elicitation_required_field_unsupported",
                            "a required input field is not supported by this client",
                        )
                        .with_diagnostic("fieldId", &field.id));
                    }
                    _ => None,
                };
                if let Some(answer) = answer {
                    answers.insert(field.id.clone(), answer);
                }
            }
        }
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action,
            answers,
            responder_device_id: None,
            resolved_at_ms,
        };
        request.validate_resolution(&resolution)?;
        Ok(ResolveElicitationRequest {
            session_id: request.session_id.clone(),
            request_id: request.id.clone(),
            resolution,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConnectionState {
    #[default]
    Online,
    Reconnecting,
    Offline,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentWorkflowState {
    pub generation: WorkflowViewGeneration,
    /// Advances only when the selected session changes. Authoritative
    /// same-session refetches must not invalidate in-flight mutations.
    pub mutation_generation: WorkflowViewGeneration,
    pub selected_session_id: Option<VibexSessionId>,
    pub sessions: AsyncState<Vec<AgentSession>>,
    pub active_session: AsyncState<AgentSession>,
    pub timeline_status: AsyncState<()>,
    pub timeline: TimelineModel,
    pub runtime_options: AsyncState<SessionRuntimeOptionCatalog>,
    pub runtime_selection: AsyncState<AgentSessionRuntimeSelectionState>,
    pub latest_mutation: AsyncState<Vec<TimelineItem>>,
    pub connection: AgentConnectionState,
    pub last_runtime_event: Option<vibex_core::RuntimeSessionEvent>,
    pending_mutations: BTreeSet<String>,
    pending_permission_resolutions: BTreeSet<String>,
    pending_elicitation_resolutions: BTreeSet<String>,
}

impl fmt::Debug for AgentWorkflowState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentWorkflowState")
            .field("generation", &self.generation)
            .field("selected_session_id", &self.selected_session_id)
            .field("sessions_phase", &self.sessions.phase)
            .field("has_sessions", &self.sessions.value.is_some())
            .field("active_session_phase", &self.active_session.phase)
            .field("has_active_session", &self.active_session.value.is_some())
            .field("timeline_status", &self.timeline_status.phase)
            .field("timeline_item_count", &self.timeline.items.len())
            .field("runtime_options_phase", &self.runtime_options.phase)
            .field("runtime_selection_phase", &self.runtime_selection.phase)
            .field("latest_mutation_phase", &self.latest_mutation.phase)
            .field("connection", &self.connection)
            .field("has_runtime_event", &self.last_runtime_event.is_some())
            .field("pending_mutation_count", &self.pending_mutations.len())
            .field(
                "pending_permission_resolution_count",
                &self.pending_permission_resolutions.len(),
            )
            .field(
                "pending_elicitation_resolution_count",
                &self.pending_elicitation_resolutions.len(),
            )
            .finish()
    }
}

impl Default for AgentWorkflowState {
    fn default() -> Self {
        Self {
            generation: WorkflowViewGeneration::default(),
            mutation_generation: WorkflowViewGeneration::default(),
            selected_session_id: None,
            sessions: AsyncState::default(),
            active_session: AsyncState::default(),
            timeline_status: AsyncState::default(),
            timeline: TimelineModel::default(),
            runtime_options: AsyncState::default(),
            runtime_selection: AsyncState::default(),
            latest_mutation: AsyncState::default(),
            connection: AgentConnectionState::Online,
            last_runtime_event: None,
            pending_mutations: BTreeSet::new(),
            pending_permission_resolutions: BTreeSet::new(),
            pending_elicitation_resolutions: BTreeSet::new(),
        }
    }
}

impl AgentWorkflowState {
    pub fn view(&self, sidebar: &SidebarState, query: &str, shell: ShellKind) -> AgentWorkflowView {
        AgentWorkflowView {
            generation: self.generation.0,
            sessions: self.sidebar_rows(sidebar, query),
            active_session: self.active_session.value.clone(),
            timeline_rows: self.timeline.rows(),
            conversation_turns: self.conversation_turns(),
            approvals: self.approval_surfaces(shell),
            elicitations: self.elicitation_surfaces(shell),
            connection: self.connection,
        }
    }

    pub fn sidebar_rows(&self, sidebar: &SidebarState, query: &str) -> Vec<AgentSidebarRow> {
        project_sidebar_rows(
            self.sessions.value.as_deref().unwrap_or_default(),
            sidebar,
            query,
        )
    }

    pub fn conversation_turns(&self) -> Vec<TimelineConversationTurn> {
        let session_state = self
            .active_session
            .value
            .as_ref()
            .map(|session| session.state);
        let pending_turn = !self.pending_mutations.is_empty();
        self.timeline
            .conversation_turns(session_state, pending_turn)
    }

    pub fn approval_surfaces(&self, shell: ShellKind) -> Vec<ApprovalSurfaceModel> {
        let mut resolved = BTreeSet::new();
        for item in &self.timeline.items {
            if let TimelinePayload::PermissionResolution(resolution) = &item.payload {
                resolved.insert(resolution.request_id.to_string());
            }
        }
        let mut seen = BTreeSet::new();
        self.timeline
            .items
            .iter()
            .filter_map(|item| match &item.payload {
                TimelinePayload::PermissionRequest(request)
                    if request.status == PermissionRequestStatus::Pending
                        && !resolved.contains(request.id.as_str())
                        && seen.insert(request.id.to_string()) =>
                {
                    Some(ApprovalSurfaceModel::from_request(request, shell))
                }
                _ => None,
            })
            .collect()
    }

    pub fn pending_permission_resolution(&self, request_id: &str) -> bool {
        self.pending_permission_resolutions.contains(request_id)
    }

    pub fn elicitation_surfaces(&self, shell: ShellKind) -> Vec<ElicitationSurfaceModel> {
        let resolved = self
            .timeline
            .items
            .iter()
            .filter_map(|item| match &item.payload {
                TimelinePayload::ElicitationResolution(resolution) => {
                    Some(resolution.request_id.to_string())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        self.timeline
            .items
            .iter()
            .filter_map(|item| match &item.payload {
                TimelinePayload::ElicitationRequest(request)
                    if request.status == ElicitationRequestStatus::Pending
                        && !resolved.contains(request.id.as_str())
                        && seen.insert(request.id.to_string()) =>
                {
                    Some(ElicitationSurfaceModel::from_request(request, shell))
                }
                _ => None,
            })
            .collect()
    }

    pub fn pending_elicitation_resolution(&self, request_id: &str) -> bool {
        self.pending_elicitation_resolutions.contains(request_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionLoadTicket {
    pub generation: WorkflowViewGeneration,
    pub session_id: VibexSessionId,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AgentSessionSnapshot {
    pub session: AgentSession,
    pub timeline: Vec<TimelineItem>,
    pub runtime_selection: Option<AgentSessionRuntimeSelectionState>,
}

impl fmt::Debug for AgentSessionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentSessionSnapshot")
            .field("session_id", &self.session.id)
            .field("timeline_item_count", &self.timeline.len())
            .field("has_runtime_selection", &self.runtime_selection.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMutationKind {
    SendMessage,
    ContinueTurn,
    Interrupt,
    ResolvePermission,
    ResolveElicitation,
    CreateSession,
    RenameSession,
    ArchiveSession,
    DeleteSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMutationTicket {
    pub generation: WorkflowViewGeneration,
    pub session_id: VibexSessionId,
    pub request_id: String,
    pub kind: AgentMutationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEventDecision {
    Applied,
    IgnoredStale,
    NeedsAuthoritativeRefetch,
    Disconnected,
}

#[derive(Clone)]
pub struct AgentWorkflowController {
    backend: Arc<dyn AgentBackend>,
    capabilities: DomainCapabilities,
    pub state: AgentWorkflowState,
}

impl AgentWorkflowController {
    pub fn new(backend: Arc<dyn AgentBackend>, capabilities: DomainCapabilities) -> Self {
        Self {
            backend,
            capabilities,
            state: AgentWorkflowState::default(),
        }
    }

    pub fn set_capabilities(&mut self, capabilities: DomainCapabilities) {
        self.capabilities = capabilities;
    }

    pub fn capabilities(&self) -> &DomainCapabilities {
        &self.capabilities
    }

    pub fn list_sessions(
        &self,
        include_archived: bool,
    ) -> BackendFuture<'static, Vec<AgentSession>> {
        if let Err(error) = self.require(BackendOperation::AgentListSessions) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.list_sessions(include_archived).await })
    }

    pub fn apply_sessions(
        &mut self,
        result: BackendResult<Vec<AgentSession>>,
    ) -> BackendResult<()> {
        match result {
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

    pub fn begin_sessions_refresh(&mut self) {
        self.state.sessions.begin();
    }

    pub fn list_runtime_options(&self) -> BackendFuture<'static, SessionRuntimeOptionCatalog> {
        if let Err(error) = self.require(BackendOperation::AgentSwitchRuntime) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.list_runtime_options().await })
    }

    pub fn apply_runtime_options(
        &mut self,
        result: BackendResult<SessionRuntimeOptionCatalog>,
    ) -> BackendResult<()> {
        match result {
            Ok(catalog) => {
                self.state.runtime_options.resolve(catalog);
                Ok(())
            }
            Err(error) => {
                self.state.runtime_options.reject(error.clone());
                Err(error)
            }
        }
    }

    pub fn begin_session_load(
        &mut self,
        session_id: VibexSessionId,
    ) -> BackendResult<AgentSessionLoadTicket> {
        self.require(BackendOperation::AgentOpenSession)?;
        self.require(BackendOperation::AgentFetchTimeline)?;
        let switching_session = self.state.selected_session_id.as_ref() != Some(&session_id);
        let generation = self.state.generation.advance();
        self.state.selected_session_id = Some(session_id.clone());
        if switching_session {
            self.state.active_session.clear();
            self.state.timeline = TimelineModel::default();
            self.state.timeline_status.clear();
            self.state.runtime_selection.clear();
        }
        self.state.active_session.begin();
        self.state.timeline_status.begin();
        if self.supports(BackendOperation::AgentSwitchRuntime) {
            self.state.runtime_selection.begin();
        }
        if switching_session {
            // A same-session reload is an authoritative refetch (recovery or
            // event catch-up); mutations already accepted by the backend stay
            // valid and their completion state must survive the reload.
            self.state.mutation_generation.advance();
            self.state.latest_mutation.clear();
            self.state.pending_mutations.clear();
            self.state.pending_permission_resolutions.clear();
            self.state.pending_elicitation_resolutions.clear();
        }
        self.state.last_runtime_event = None;
        Ok(AgentSessionLoadTicket {
            generation,
            session_id,
        })
    }

    pub fn load_session(
        &self,
        ticket: AgentSessionLoadTicket,
    ) -> BackendFuture<'static, AgentSessionSnapshot> {
        let backend = self.backend.clone();
        let include_runtime = self.supports(BackendOperation::AgentSwitchRuntime);
        Box::pin(async move {
            let session = backend.open_session(ticket.session_id.clone()).await?;
            if session.id != ticket.session_id {
                return Err(BackendError::failed(
                    "agent_session_response_mismatch",
                    "the backend returned a different Agent session",
                ));
            }
            let timeline = load_complete_timeline(backend.as_ref(), &ticket.session_id).await?;
            // Runtime metadata is a best-effort sibling query. The
            // authoritative timeline must still render when a provider or
            // remote device cannot expose runtime-selection details.
            let runtime_selection = if include_runtime {
                backend
                    .runtime_selection(ticket.session_id.clone())
                    .await
                    .ok()
            } else {
                None
            };
            Ok(AgentSessionSnapshot {
                session,
                timeline,
                runtime_selection,
            })
        })
    }

    pub fn apply_session_snapshot(
        &mut self,
        ticket: &AgentSessionLoadTicket,
        result: BackendResult<AgentSessionSnapshot>,
    ) -> bool {
        if self.state.generation != ticket.generation
            || self.state.selected_session_id.as_ref() != Some(&ticket.session_id)
        {
            return false;
        }
        match result {
            Ok(snapshot) => {
                if snapshot.session.id != ticket.session_id {
                    let error = BackendError::failed(
                        "agent_session_response_mismatch",
                        "the backend returned a different Agent session",
                    );
                    self.state.active_session.reject(error.clone());
                    self.state.timeline_status.reject(error);
                    return true;
                }
                self.state.active_session.resolve(snapshot.session);
                self.state
                    .timeline
                    .replace_authoritative(ticket.session_id.clone(), snapshot.timeline);
                self.state.timeline_status.resolve(());
                if let Some(runtime_selection) = snapshot.runtime_selection {
                    self.state.runtime_selection.resolve(runtime_selection);
                } else if self.state.runtime_selection.phase == AsyncPhase::Loading {
                    self.state.runtime_selection.clear();
                }
                self.state.connection = AgentConnectionState::Online;
            }
            Err(error) => {
                self.state.active_session.reject(error.clone());
                self.state.timeline_status.reject(error.clone());
                if self.state.runtime_selection.phase == AsyncPhase::Loading {
                    self.state.runtime_selection.reject(error);
                }
            }
        }
        true
    }

    pub fn create_session(
        &self,
        request: MutationRequest<CreateAgentSessionRequest>,
    ) -> BackendFuture<'static, AgentSession> {
        if let Err(error) = self.require(BackendOperation::AgentCreateSession) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.create_session(request).await })
    }

    pub fn rename_session(
        &self,
        request: MutationRequest<RenameAgentSessionRequest>,
    ) -> BackendFuture<'static, AgentSession> {
        if let Err(error) = self.require(BackendOperation::AgentManageSession) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.rename_session(request).await })
    }

    pub fn archive_session(
        &self,
        request: MutationRequest<VibexSessionId>,
    ) -> BackendFuture<'static, ()> {
        if let Err(error) = self.require(BackendOperation::AgentManageSession) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.archive_session(request).await })
    }

    pub fn delete_session(
        &self,
        request: MutationRequest<VibexSessionId>,
    ) -> BackendFuture<'static, ()> {
        if let Err(error) = self.require(BackendOperation::AgentManageSession) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.delete_session(request).await })
    }

    pub fn begin_send_message(
        &mut self,
        request: &MutationRequest<SendAgentMessageRequest>,
    ) -> BackendResult<AgentMutationTicket> {
        self.begin_mutation(
            request.request_id.as_str(),
            &request.payload.session_id,
            AgentMutationKind::SendMessage,
            BackendOperation::AgentSendMessage,
        )
    }

    pub fn send_message(
        &self,
        request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendFuture<'static, Vec<TimelineItem>> {
        if let Err(error) = self.require(BackendOperation::AgentSendMessage) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.send_message(request).await })
    }

    pub fn begin_continue_turn(
        &mut self,
        request: &MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendResult<AgentMutationTicket> {
        self.begin_mutation(
            request.request_id.as_str(),
            &request.payload.session_id,
            AgentMutationKind::ContinueTurn,
            BackendOperation::AgentContinueTurn,
        )
    }

    pub fn continue_turn(
        &self,
        request: MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendFuture<'static, Vec<TimelineItem>> {
        if let Err(error) = self.require(BackendOperation::AgentContinueTurn) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.continue_turn(request).await })
    }

    pub fn begin_interrupt(
        &mut self,
        request: &MutationRequest<VibexSessionId>,
    ) -> BackendResult<AgentMutationTicket> {
        self.begin_mutation(
            request.request_id.as_str(),
            &request.payload,
            AgentMutationKind::Interrupt,
            BackendOperation::AgentInterrupt,
        )
    }

    pub fn interrupt(
        &self,
        request: MutationRequest<VibexSessionId>,
    ) -> BackendFuture<'static, bool> {
        if let Err(error) = self.require(BackendOperation::AgentInterrupt) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.interrupt(request).await })
    }

    pub fn begin_resolve_permission(
        &mut self,
        request: &MutationRequest<ResolvePermissionRequest>,
    ) -> BackendResult<AgentMutationTicket> {
        self.require(BackendOperation::AgentResolveApproval)?;
        let permission_id = request.payload.request_id.as_str();
        let pending = self.state.timeline.items.iter().find_map(|item| {
            let TimelinePayload::PermissionRequest(permission) = &item.payload else {
                return None;
            };
            (permission.id == request.payload.request_id
                && permission.session_id == request.payload.session_id
                && permission.status == PermissionRequestStatus::Pending)
                .then_some(permission)
        });
        let Some(permission) = pending else {
            return Err(BackendError::conflict(
                "agent_permission_not_pending",
                "the permission request is no longer pending",
            ));
        };
        if !permission
            .allowed_responses
            .contains(&request.payload.resolution.response)
        {
            return Err(BackendError::permission(
                "agent_permission_response_not_allowed",
                "the selected permission response is not allowed",
            ));
        }
        if !self
            .state
            .pending_permission_resolutions
            .insert(permission_id.to_string())
        {
            return Err(BackendError::conflict(
                "agent_permission_resolution_pending",
                "this permission response is already being submitted",
            ));
        }
        match self.begin_mutation(
            request.request_id.as_str(),
            &request.payload.session_id,
            AgentMutationKind::ResolvePermission,
            BackendOperation::AgentResolveApproval,
        ) {
            Ok(ticket) => Ok(ticket),
            Err(error) => {
                self.state
                    .pending_permission_resolutions
                    .remove(permission_id);
                Err(error)
            }
        }
    }

    pub fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'static, TimelineItem> {
        if let Err(error) = self.require(BackendOperation::AgentResolveApproval) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.resolve_permission(request).await })
    }

    pub fn begin_resolve_elicitation(
        &mut self,
        request: &MutationRequest<ResolveElicitationRequest>,
    ) -> BackendResult<AgentMutationTicket> {
        self.require(BackendOperation::AgentRespondElicitation)?;
        request.payload.validate().map_err(BackendError::from)?;
        let elicitation_id = request.payload.request_id.as_str();
        let pending = self.state.timeline.items.iter().find_map(|item| {
            let TimelinePayload::ElicitationRequest(elicitation) = &item.payload else {
                return None;
            };
            (elicitation.id == request.payload.request_id
                && elicitation.session_id == request.payload.session_id
                && elicitation.status == ElicitationRequestStatus::Pending)
                .then_some(elicitation)
        });
        let Some(elicitation) = pending else {
            return Err(BackendError::conflict(
                "agent_elicitation_not_pending",
                "the input request is no longer pending",
            ));
        };
        elicitation
            .validate_resolution(&request.payload.resolution)
            .map_err(BackendError::from)?;
        if !self
            .state
            .pending_elicitation_resolutions
            .insert(elicitation_id.to_string())
        {
            return Err(BackendError::conflict(
                "agent_elicitation_resolution_pending",
                "this input response is already being submitted",
            ));
        }
        match self.begin_mutation(
            request.request_id.as_str(),
            &request.payload.session_id,
            AgentMutationKind::ResolveElicitation,
            BackendOperation::AgentRespondElicitation,
        ) {
            Ok(ticket) => Ok(ticket),
            Err(error) => {
                self.state
                    .pending_elicitation_resolutions
                    .remove(elicitation_id);
                Err(error)
            }
        }
    }

    pub fn resolve_elicitation(
        &self,
        request: MutationRequest<ResolveElicitationRequest>,
    ) -> BackendFuture<'static, TimelineItem> {
        if let Err(error) = self.require(BackendOperation::AgentRespondElicitation) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.resolve_elicitation(request).await })
    }

    pub fn apply_timeline_mutation(
        &mut self,
        ticket: &AgentMutationTicket,
        result: BackendResult<Vec<TimelineItem>>,
    ) -> bool {
        if !matches!(
            ticket.kind,
            AgentMutationKind::SendMessage | AgentMutationKind::ContinueTurn
        ) {
            return false;
        }
        if !self.finish_mutation(ticket) {
            return false;
        }
        match result {
            Ok(items)
                if items.iter().any(|item| {
                    item.session_id != ticket.session_id
                        || item.sequence <= 0
                        || item.kind != item.payload.kind()
                }) =>
            {
                self.state.latest_mutation.reject(BackendError::failed(
                    "agent_mutation_response_mismatch",
                    "the backend returned timeline items for another Agent session",
                ));
            }
            Ok(items) => {
                for item in &items {
                    let _ = self.state.timeline.apply_live(TimelineLiveEvent {
                        session_id: ticket.session_id.clone(),
                        sequence: item.sequence,
                        item: item.clone(),
                    });
                }
                self.state.latest_mutation.resolve(items);
            }
            Err(error) => self.state.latest_mutation.reject(error),
        }
        true
    }

    pub fn apply_permission_mutation(
        &mut self,
        ticket: &AgentMutationTicket,
        permission_request_id: &str,
        result: BackendResult<TimelineItem>,
    ) -> bool {
        if ticket.kind != AgentMutationKind::ResolvePermission {
            return false;
        }
        if !self.finish_mutation(ticket) {
            return false;
        }
        self.state
            .pending_permission_resolutions
            .remove(permission_request_id);
        match result {
            Ok(item)
                if item.session_id != ticket.session_id
                    || item.sequence <= 0
                    || !matches!(
                        &item.payload,
                        TimelinePayload::PermissionResolution(resolution)
                            if resolution.session_id == ticket.session_id
                                && resolution.request_id.as_str() == permission_request_id
                    ) =>
            {
                self.state.latest_mutation.reject(BackendError::failed(
                    "agent_permission_response_mismatch",
                    "the backend returned a different permission resolution",
                ));
            }
            Ok(item) => {
                let _ = self.state.timeline.apply_live(TimelineLiveEvent {
                    session_id: ticket.session_id.clone(),
                    sequence: item.sequence,
                    item: item.clone(),
                });
                self.state.latest_mutation.resolve(vec![item]);
            }
            Err(error) => self.state.latest_mutation.reject(error),
        }
        true
    }

    pub fn apply_elicitation_mutation(
        &mut self,
        ticket: &AgentMutationTicket,
        elicitation_request_id: &str,
        result: BackendResult<TimelineItem>,
    ) -> bool {
        if ticket.kind != AgentMutationKind::ResolveElicitation {
            return false;
        }
        if !self.finish_mutation(ticket) {
            return false;
        }
        self.state
            .pending_elicitation_resolutions
            .remove(elicitation_request_id);
        match result {
            Ok(item)
                if item.session_id != ticket.session_id
                    || item.sequence <= 0
                    || !matches!(
                        &item.payload,
                        TimelinePayload::ElicitationResolution(resolution)
                            if resolution.session_id == ticket.session_id
                                && resolution.request_id.as_str() == elicitation_request_id
                    ) =>
            {
                self.state.latest_mutation.reject(BackendError::failed(
                    "agent_elicitation_response_mismatch",
                    "the backend returned a different input-request resolution",
                ));
            }
            Ok(item) => {
                let _ = self.state.timeline.apply_live(TimelineLiveEvent {
                    session_id: ticket.session_id.clone(),
                    sequence: item.sequence,
                    item: item.clone(),
                });
                self.state.latest_mutation.resolve(vec![item]);
            }
            Err(error) => self.state.latest_mutation.reject(error),
        }
        true
    }

    pub fn apply_simple_mutation(
        &mut self,
        ticket: &AgentMutationTicket,
        result: BackendResult<bool>,
    ) -> bool {
        if ticket.kind != AgentMutationKind::Interrupt {
            return false;
        }
        if !self.finish_mutation(ticket) {
            return false;
        }
        match result {
            Ok(_) => self.state.latest_mutation.resolve(Vec::new()),
            Err(error) => self.state.latest_mutation.reject(error),
        }
        true
    }

    pub fn apply_event(&mut self, event: BackendEvent) -> AgentEventDecision {
        match event {
            BackendEvent::Timeline(event) => {
                if self.state.selected_session_id.as_ref() != Some(&event.session_id) {
                    return AgentEventDecision::IgnoredStale;
                }
                let changed = self.state.timeline.apply_live(event);
                if self.state.timeline.needs_authoritative_refetch {
                    AgentEventDecision::NeedsAuthoritativeRefetch
                } else if changed {
                    AgentEventDecision::Applied
                } else {
                    AgentEventDecision::IgnoredStale
                }
            }
            BackendEvent::Notification(_) => AgentEventDecision::IgnoredStale,
            BackendEvent::Runtime(event) => {
                if self.state.selected_session_id.as_ref() != Some(&event.session_id) {
                    return AgentEventDecision::IgnoredStale;
                }
                self.state.last_runtime_event = Some(event);
                AgentEventDecision::Applied
            }
            BackendEvent::RuntimeSelection(event) => {
                if self.state.selected_session_id.as_ref() != Some(&event.session_id) {
                    return AgentEventDecision::IgnoredStale;
                }
                self.state.runtime_selection.resolve(event.state);
                AgentEventDecision::Applied
            }
            BackendEvent::ProjectionInvalidated(_) => AgentEventDecision::IgnoredStale,
            BackendEvent::Lagged { refetch, .. } => {
                if refetch.session_id.is_some()
                    && refetch.session_id.as_ref() != self.state.selected_session_id.as_ref()
                {
                    return AgentEventDecision::IgnoredStale;
                }
                if refetch.timeline {
                    self.state.timeline.mark_lagged();
                }
                AgentEventDecision::NeedsAuthoritativeRefetch
            }
            BackendEvent::Disconnected => {
                self.state.connection = AgentConnectionState::Offline;
                AgentEventDecision::Disconnected
            }
        }
    }

    pub fn mark_reconnecting(&mut self) {
        self.state.connection = AgentConnectionState::Reconnecting;
    }

    fn begin_mutation(
        &mut self,
        request_id: &str,
        session_id: &VibexSessionId,
        kind: AgentMutationKind,
        operation: BackendOperation,
    ) -> BackendResult<AgentMutationTicket> {
        self.require(operation)?;
        if self.state.selected_session_id.as_ref() != Some(session_id) {
            return Err(BackendError::conflict(
                "agent_session_generation_stale",
                "the Agent action targets a session that is no longer selected",
            ));
        }
        if !self.state.pending_mutations.insert(request_id.to_string()) {
            return Err(BackendError::conflict(
                "agent_mutation_already_pending",
                "this Agent mutation is already pending",
            ));
        }
        self.state.latest_mutation.begin();
        Ok(AgentMutationTicket {
            generation: self.state.mutation_generation,
            session_id: session_id.clone(),
            request_id: request_id.to_string(),
            kind,
        })
    }

    fn finish_mutation(&mut self, ticket: &AgentMutationTicket) -> bool {
        if self.state.mutation_generation != ticket.generation
            || self.state.selected_session_id.as_ref() != Some(&ticket.session_id)
        {
            return false;
        }
        self.state.pending_mutations.remove(&ticket.request_id)
    }

    fn require(&self, operation: BackendOperation) -> BackendResult<()> {
        if self.capabilities.supports(operation) {
            Ok(())
        } else {
            Err(capability_error(&self.capabilities, operation))
        }
    }

    fn supports(&self, operation: BackendOperation) -> bool {
        self.capabilities.supports(operation)
    }
}

async fn load_complete_timeline(
    backend: &dyn AgentBackend,
    session_id: &VibexSessionId,
) -> BackendResult<Vec<TimelineItem>> {
    let mut after_sequence = 0_i64;
    let mut by_sequence = BTreeMap::new();
    loop {
        let page = backend
            .fetch_timeline(FetchTimelineRequest {
                session_id: session_id.clone(),
                after_sequence: Some(after_sequence),
                limit: AGENT_TIMELINE_PAGE_LIMIT,
            })
            .await?;
        validate_timeline_page(session_id, after_sequence, &page)?;
        for item in page.items {
            if item.session_id == *session_id {
                by_sequence.insert(item.sequence, item);
            }
        }
        if by_sequence.len() > AGENT_TIMELINE_MAX_ITEMS {
            return Err(BackendError::failed(
                "agent_timeline_limit_exceeded",
                "the complete Agent timeline exceeds the bounded client projection",
            ));
        }
        if !page.has_newer {
            return Ok(by_sequence.into_values().collect());
        }
        let Some(next) = page.end_sequence.filter(|next| *next > after_sequence) else {
            return Err(BackendError::failed(
                "agent_timeline_pagination_stalled",
                "the Agent timeline cursor did not advance",
            ));
        };
        after_sequence = next;
    }
}

fn validate_timeline_page(
    session_id: &VibexSessionId,
    after_sequence: i64,
    page: &TimelinePage,
) -> BackendResult<()> {
    if page.session_id != *session_id {
        return Err(BackendError::failed(
            "agent_timeline_session_mismatch",
            "the Agent timeline page belongs to another session",
        ));
    }
    if page
        .items
        .iter()
        .any(|item| item.session_id != *session_id || item.sequence <= after_sequence)
    {
        return Err(BackendError::failed(
            "agent_timeline_page_invalid",
            "the Agent timeline page contains an invalid sequence or session",
        ));
    }
    Ok(())
}

fn capability_error(
    capabilities: &DomainCapabilities,
    operation: BackendOperation,
) -> BackendError {
    use vibex_backend::CapabilityAvailability;
    let code = format!("{}_unavailable", agent_operation_label(operation));
    match capabilities.availability {
        CapabilityAvailability::Offline => {
            BackendError::offline(code, "the authoritative Agent backend is offline")
        }
        CapabilityAvailability::Degraded => BackendError::loading(
            code,
            "the authoritative Agent backend is temporarily degraded",
        ),
        CapabilityAvailability::RequiresPermission => BackendError::permission(
            code,
            "the current device lacks permission for this Agent operation",
        ),
        CapabilityAvailability::Available | CapabilityAvailability::Unsupported => {
            BackendError::unsupported(code, "this Agent operation is not supported")
        }
    }
}

fn agent_operation_label(operation: BackendOperation) -> &'static str {
    use BackendOperation::*;
    match operation {
        AgentListSessions => "agent_list_sessions",
        AgentCreateSession => "agent_create_session",
        AgentOpenSession => "agent_open_session",
        AgentFetchTimeline => "agent_fetch_timeline",
        AgentSendMessage => "agent_send_message",
        AgentContinueTurn => "agent_continue_turn",
        AgentInterrupt => "agent_interrupt",
        AgentResolveApproval => "agent_resolve_approval",
        AgentRespondElicitation => "agent_respond_elicitation",
        AgentManageSession => "agent_manage_session",
        AgentSwitchRuntime => "agent_switch_runtime",
        _ => "agent_operation",
    }
}

fn error_future<T: 'static>(error: BackendError) -> BackendFuture<'static, T> {
    Box::pin(async move { Err(error) })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use vibex_backend::{BackendEventSubscription, MutationRequest};
    use vibex_core::{
        AgentId, AgentMessagePayload, AgentSessionSafety, AgentSessionState, CorrelationId,
        ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationOption,
        ElicitationRequest, ElicitationRequestStatus, ElicitationResolutionAction,
        PermissionActionDetail, PermissionRequest, PermissionResolution, PermissionResponseKind,
        PermissionResponseOption, PermissionRiskCategory, ProjectId, ProviderProfileId, RequestId,
        SessionRuntimeSelection, TimelineItemId, TimelineRedactionState, TimelineSource,
        UserMessagePayload, WorkspaceId, WorkspaceMode,
    };

    #[derive(Clone)]
    struct MockAgentBackend {
        session: AgentSession,
        timeline: Arc<Mutex<Vec<TimelineItem>>>,
        permission_resolution: Arc<Mutex<Option<TimelineItem>>>,
        elicitation_resolution: Arc<Mutex<Option<TimelineItem>>>,
    }

    impl MockAgentBackend {
        fn new(session: AgentSession, timeline: Vec<TimelineItem>) -> Self {
            Self {
                session,
                timeline: Arc::new(Mutex::new(timeline)),
                permission_resolution: Arc::new(Mutex::new(None)),
                elicitation_resolution: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl AgentBackend for MockAgentBackend {
        fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
            Err(BackendError::unsupported("mock", "mock"))
        }

        fn list_sessions(&self, _include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>> {
            let session = self.session.clone();
            Box::pin(async move { Ok(vec![session]) })
        }

        fn open_session(&self, session_id: VibexSessionId) -> BackendFuture<'_, AgentSession> {
            let session = self.session.clone();
            Box::pin(async move {
                if session.id == session_id {
                    Ok(session)
                } else {
                    Err(BackendError::failed("session_not_found", "not found"))
                }
            })
        }

        fn create_session(
            &self,
            _request: MutationRequest<CreateAgentSessionRequest>,
        ) -> BackendFuture<'_, AgentSession> {
            let session = self.session.clone();
            Box::pin(async move { Ok(session) })
        }

        fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage> {
            let timeline = self.timeline.clone();
            Box::pin(async move {
                let after = request.after_sequence.unwrap_or_default();
                let limit = request.limit as usize;
                let items = timeline
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock poisoned"))?
                    .iter()
                    .filter(|item| item.sequence > after)
                    .take(limit)
                    .cloned()
                    .collect::<Vec<_>>();
                let end_sequence = items.last().map(|item| item.sequence);
                let total_after = timeline
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock poisoned"))?
                    .iter()
                    .filter(|item| item.sequence > after)
                    .count();
                Ok(TimelinePage {
                    session_id: request.session_id,
                    start_sequence: items.first().map(|item| item.sequence),
                    end_sequence,
                    has_older: false,
                    has_newer: total_after > items.len(),
                    items,
                })
            })
        }

        fn send_message(
            &self,
            _request: MutationRequest<SendAgentMessageRequest>,
        ) -> BackendFuture<'_, Vec<TimelineItem>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn continue_turn(
            &self,
            _request: MutationRequest<ContinueAgentTurnRequest>,
        ) -> BackendFuture<'_, Vec<TimelineItem>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn interrupt(&self, _request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, bool> {
            Box::pin(async { Ok(true) })
        }

        fn resolve_permission(
            &self,
            _request: MutationRequest<ResolvePermissionRequest>,
        ) -> BackendFuture<'_, TimelineItem> {
            let item = self.permission_resolution.clone();
            Box::pin(async move {
                item.lock()
                    .map_err(|_| BackendError::failed("mock", "mock poisoned"))?
                    .clone()
                    .ok_or_else(|| BackendError::failed("mock", "missing resolution"))
            })
        }

        fn resolve_elicitation(
            &self,
            _request: MutationRequest<ResolveElicitationRequest>,
        ) -> BackendFuture<'_, TimelineItem> {
            let item = self.elicitation_resolution.clone();
            Box::pin(async move {
                item.lock()
                    .map_err(|_| BackendError::failed("mock", "mock poisoned"))?
                    .clone()
                    .ok_or_else(|| BackendError::failed("mock", "missing resolution"))
            })
        }

        fn rename_session(
            &self,
            _request: MutationRequest<RenameAgentSessionRequest>,
        ) -> BackendFuture<'_, AgentSession> {
            let session = self.session.clone();
            Box::pin(async move { Ok(session) })
        }

        fn archive_session(
            &self,
            _request: MutationRequest<VibexSessionId>,
        ) -> BackendFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn delete_session(
            &self,
            _request: MutationRequest<VibexSessionId>,
        ) -> BackendFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn list_runtime_options(&self) -> BackendFuture<'_, SessionRuntimeOptionCatalog> {
            Box::pin(async {
                Ok(SessionRuntimeOptionCatalog {
                    revision: 1,
                    agents: Vec::new(),
                    auth_sources: Vec::new(),
                    options: Vec::new(),
                })
            })
        }

        fn runtime_selection(
            &self,
            _session_id: VibexSessionId,
        ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
            error_future(BackendError::unsupported("mock", "runtime not needed"))
        }

        fn set_desired_runtime(
            &self,
            _request: MutationRequest<vibex_core::SetDesiredAgentSessionRuntimeRequest>,
        ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
            error_future(BackendError::unsupported("mock", "runtime not needed"))
        }

        fn cancel_runtime_switch(
            &self,
            _request: MutationRequest<vibex_core::CancelAgentSessionRuntimeSwitchRequest>,
        ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
            error_future(BackendError::unsupported("mock", "runtime not needed"))
        }
    }

    fn session() -> AgentSession {
        AgentSession {
            id: VibexSessionId::new(),
            title: "Shared workflow".into(),
            project_id: ProjectId::new(),
            workspace_id: WorkspaceId::new(),
            workspace_root: "/fixture".into(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    fn timeline_item(
        session_id: &VibexSessionId,
        sequence: i64,
        payload: TimelinePayload,
    ) -> TimelineItem {
        TimelineItem {
            id: TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: sequence,
            source: if matches!(payload, TimelinePayload::UserMessage(_)) {
                TimelineSource::User
            } else {
                TimelineSource::Agent
            },
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        }
    }

    fn capabilities() -> DomainCapabilities {
        DomainCapabilities::available([
            BackendOperation::AgentListSessions,
            BackendOperation::AgentCreateSession,
            BackendOperation::AgentOpenSession,
            BackendOperation::AgentFetchTimeline,
            BackendOperation::AgentSendMessage,
            BackendOperation::AgentContinueTurn,
            BackendOperation::AgentInterrupt,
            BackendOperation::AgentResolveApproval,
            BackendOperation::AgentRespondElicitation,
            BackendOperation::AgentManageSession,
        ])
    }

    #[tokio::test]
    async fn agent_loads_complete_authoritative_timeline_and_rejects_stale_generation() {
        let session = session();
        let items = vec![
            timeline_item(
                &session.id,
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "go".into(),
                    attachments: Vec::new(),
                }),
            ),
            timeline_item(
                &session.id,
                2,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "done".into(),
                    is_final: true,
                }),
            ),
        ];
        let backend = Arc::new(MockAgentBackend::new(session.clone(), items));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        let stale = controller.begin_session_load(session.id.clone()).unwrap();
        let snapshot = controller.load_session(stale.clone()).await.unwrap();
        let current = controller.begin_session_load(session.id.clone()).unwrap();
        assert!(!controller.apply_session_snapshot(&stale, Ok(snapshot.clone())));
        assert!(controller.apply_session_snapshot(&current, Ok(snapshot)));
        assert_eq!(controller.state.timeline.items.len(), 2);
        assert_eq!(controller.state.conversation_turns().len(), 1);
    }

    #[test]
    fn same_session_refetch_keeps_content_while_a_session_switch_clears_it() {
        let session = session();
        let item = timeline_item(
            &session.id,
            1,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "completed response".into(),
                is_final: true,
            }),
        );
        let backend = Arc::new(MockAgentBackend::new(session.clone(), vec![item.clone()]));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        controller.state.selected_session_id = Some(session.id.clone());
        controller.state.active_session.resolve(session.clone());
        controller.state.timeline_status.resolve(());
        controller
            .state
            .timeline
            .replace_authoritative(session.id.clone(), [item]);

        controller
            .begin_session_load(session.id.clone())
            .expect("same-session refetch should start");

        assert_eq!(controller.state.active_session.phase, AsyncPhase::Loading);
        assert_eq!(controller.state.active_session.value, Some(session));
        assert_eq!(controller.state.timeline_status.value, Some(()));
        assert_eq!(controller.state.timeline.items.len(), 1);
        assert_eq!(controller.state.conversation_turns().len(), 1);

        controller
            .begin_session_load(VibexSessionId::new())
            .expect("session switch should start");

        assert!(controller.state.active_session.value.is_none());
        assert!(controller.state.timeline_status.value.is_none());
        assert!(controller.state.timeline.items.is_empty());
    }

    #[test]
    fn runtime_selection_event_without_switch_projection_applies_to_selected_session() {
        let session = session();
        let selection = SessionRuntimeSelection::provider(
            AgentId::parse("claude").unwrap(),
            ProviderProfileId::new(),
            "claude-sonnet",
        );
        let backend = Arc::new(MockAgentBackend::new(session.clone(), Vec::new()));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        controller.state.selected_session_id = Some(session.id.clone());
        controller.state.runtime_selection.begin();

        let decision = controller.apply_event(BackendEvent::RuntimeSelection(
            vibex_core::AgentSessionRuntimeSelectionEvent {
                session_id: session.id.clone(),
                state: AgentSessionRuntimeSelectionState {
                    desired: selection.clone(),
                    effective: selection,
                    status: vibex_core::SessionRuntimeSelectionStatus::Ready,
                    session_revision: 2,
                    selection_revision: 1,
                    current_binding_id: None,
                    activation_generation: 1,
                    pending_switch_id: None,
                    actionable_error: None,
                },
                event: None,
            },
        ));

        assert_eq!(decision, AgentEventDecision::Applied);
        assert_eq!(controller.state.runtime_selection.phase, AsyncPhase::Ready);
    }

    #[test]
    fn compact_approval_is_prominent_touch_discoverable_and_deduplicated() {
        let session = session();
        let request = PermissionRequest {
            id: RequestId::new(),
            session_id: session.id.clone(),
            project_id: Some(session.project_id.clone()),
            workspace_id: Some(session.workspace_id.clone()),
            provider_request_id: None,
            risk_category: PermissionRiskCategory::Command,
            title: "Run tests?".into(),
            details: vec![PermissionActionDetail {
                label: "Command".into(),
                value: "cargo test".into(),
            }],
            allowed_responses: vec![
                PermissionResponseKind::Approve,
                PermissionResponseKind::Deny,
            ],
            response_options: vec![
                PermissionResponseOption {
                    option_id: "allow-once".into(),
                    label: "Allow".into(),
                    response: PermissionResponseKind::Approve,
                },
                PermissionResponseOption {
                    option_id: "allow-cargo-prefix".into(),
                    label: "Allow commands starting with cargo".into(),
                    response: PermissionResponseKind::AlwaysAllowForSession,
                },
                PermissionResponseOption {
                    option_id: "reject-once".into(),
                    label: "Reject".into(),
                    response: PermissionResponseKind::Deny,
                },
            ],
            status: PermissionRequestStatus::Pending,
            requested_at_ms: 1,
            expires_at_ms: None,
        };
        let backend = Arc::new(MockAgentBackend::new(session.clone(), Vec::new()));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        controller.state.selected_session_id = Some(session.id.clone());
        controller.state.generation = WorkflowViewGeneration(1);
        controller.state.timeline.replace_authoritative(
            session.id.clone(),
            [
                timeline_item(
                    &session.id,
                    1,
                    TimelinePayload::PermissionRequest(request.clone()),
                ),
                timeline_item(
                    &session.id,
                    2,
                    TimelinePayload::PermissionRequest(request.clone()),
                ),
            ],
        );
        let surfaces = controller.state.approval_surfaces(ShellKind::Compact);
        assert_eq!(surfaces.len(), 1);
        assert!(surfaces[0].high_priority);
        assert!(surfaces[0].is_touch_discoverable());
        assert_eq!(surfaces[0].presentation, crate::ApprovalPresentation::Sheet);
        assert_eq!(surfaces[0].response_options, request.response_options);

        let resolution = ResolvePermissionRequest {
            session_id: session.id.clone(),
            request_id: request.id.clone(),
            resolution: PermissionResolution {
                request_id: request.id,
                session_id: session.id.clone(),
                response: PermissionResponseKind::Approve,
                responder_device_id: None,
                provider_resolution_id: None,
                note: None,
                resolved_at_ms: 2,
            },
        };
        let mutation = MutationRequest::new(resolution);
        controller.begin_resolve_permission(&mutation).unwrap();
        assert_eq!(
            controller
                .begin_resolve_permission(&mutation)
                .unwrap_err()
                .code,
            "agent_permission_resolution_pending"
        );
    }

    #[test]
    fn elicitation_draft_builds_typed_answers_and_validates_fields() {
        let session_id = VibexSessionId::new();
        let request = ElicitationRequest {
            id: RequestId::new(),
            session_id: session_id.clone(),
            provider_request_id: None,
            tool_call_id: None,
            message: "Configure the run".into(),
            title: Some("Configuration".into()),
            description: None,
            fields: vec![
                ElicitationField {
                    id: "name".into(),
                    title: "Name".into(),
                    description: None,
                    required: true,
                    kind: ElicitationFieldKind::Text {
                        min_length: Some(2),
                        max_length: Some(20),
                        pattern: None,
                        format: None,
                        default: None,
                        options: Vec::new(),
                    },
                },
                ElicitationField {
                    id: "count".into(),
                    title: "Count".into(),
                    description: None,
                    required: true,
                    kind: ElicitationFieldKind::Integer {
                        minimum: Some(1),
                        maximum: Some(3),
                        default: Some(2),
                    },
                },
                ElicitationField {
                    id: "confirm".into(),
                    title: "Confirm".into(),
                    description: None,
                    required: true,
                    kind: ElicitationFieldKind::Boolean {
                        default: Some(true),
                    },
                },
                ElicitationField {
                    id: "tags".into(),
                    title: "Tags".into(),
                    description: None,
                    required: true,
                    kind: ElicitationFieldKind::MultiSelect {
                        options: vec![
                            ElicitationOption {
                                value: "rust".into(),
                                title: "Rust".into(),
                                description: None,
                            },
                            ElicitationOption {
                                value: "ui".into(),
                                title: "UI".into(),
                                description: None,
                            },
                        ],
                        min_items: Some(1),
                        max_items: Some(2),
                        default: vec!["rust".into()],
                    },
                },
            ],
            status: ElicitationRequestStatus::Pending,
            requested_at_ms: 1,
        };
        let mut draft = ElicitationFormDraft::from_request(&request);
        draft.set_text("name", "Ada");
        draft.toggle_multi_option("tags", "ui");
        let payload = draft
            .resolve_request(&request, ElicitationResolutionAction::Accept, 2)
            .unwrap();
        assert_eq!(payload.session_id, session_id);
        assert_eq!(
            payload.resolution.answers.get("name"),
            Some(&ElicitationAnswerValue::String("Ada".into()))
        );
        assert_eq!(
            payload.resolution.answers.get("count"),
            Some(&ElicitationAnswerValue::Integer(2))
        );
        assert_eq!(
            payload.resolution.answers.get("confirm"),
            Some(&ElicitationAnswerValue::Boolean(true))
        );
        assert_eq!(
            payload.resolution.answers.get("tags"),
            Some(&ElicitationAnswerValue::StringArray(vec![
                "rust".into(),
                "ui".into(),
            ]))
        );

        draft.set_text("count", "not-an-integer");
        assert_eq!(
            draft
                .resolve_request(&request, ElicitationResolutionAction::Accept, 3)
                .unwrap_err()
                .code,
            "elicitation_answer_invalid"
        );
    }

    #[test]
    fn elicitation_surface_and_resolution_are_deduplicated_and_fenced() {
        let session = session();
        let request = ElicitationRequest {
            id: RequestId::new(),
            session_id: session.id.clone(),
            provider_request_id: None,
            tool_call_id: None,
            message: "Continue?".into(),
            title: None,
            description: None,
            fields: Vec::new(),
            status: ElicitationRequestStatus::Pending,
            requested_at_ms: 1,
        };
        let backend = Arc::new(MockAgentBackend::new(session.clone(), Vec::new()));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        controller.state.selected_session_id = Some(session.id.clone());
        controller.state.generation = WorkflowViewGeneration(1);
        controller.state.timeline.replace_authoritative(
            session.id.clone(),
            [
                timeline_item(
                    &session.id,
                    1,
                    TimelinePayload::ElicitationRequest(request.clone()),
                ),
                timeline_item(
                    &session.id,
                    2,
                    TimelinePayload::ElicitationRequest(request.clone()),
                ),
            ],
        );
        let surfaces = controller.state.elicitation_surfaces(ShellKind::Compact);
        assert_eq!(surfaces.len(), 1);
        assert!(surfaces[0].high_priority);
        assert!(surfaces[0].is_touch_discoverable());
        assert_eq!(surfaces[0].presentation, crate::ApprovalPresentation::Sheet);

        let payload = ElicitationFormDraft::from_request(&request)
            .resolve_request(&request, ElicitationResolutionAction::Decline, 2)
            .unwrap();
        let mutation = MutationRequest::new(payload.clone());
        let ticket = controller.begin_resolve_elicitation(&mutation).unwrap();
        assert_eq!(
            controller
                .begin_resolve_elicitation(&mutation)
                .unwrap_err()
                .code,
            "agent_elicitation_resolution_pending"
        );
        let item = timeline_item(
            &session.id,
            3,
            TimelinePayload::ElicitationResolution(payload.resolution),
        );
        assert!(controller.apply_elicitation_mutation(&ticket, request.id.as_str(), Ok(item),));
        assert!(
            !controller
                .state
                .pending_elicitation_resolution(request.id.as_str())
        );
        assert!(
            controller
                .state
                .elicitation_surfaces(ShellKind::Compact)
                .is_empty()
        );
    }

    #[test]
    fn agent_message_ticket_is_session_and_generation_fenced() {
        let session = session();
        let backend = Arc::new(MockAgentBackend::new(session.clone(), Vec::new()));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        controller.state.selected_session_id = Some(session.id.clone());
        controller.state.generation = WorkflowViewGeneration(4);
        controller
            .state
            .timeline
            .replace_authoritative(session.id.clone(), Vec::new());
        let request = MutationRequest::new(SendAgentMessageRequest {
            session_id: session.id.clone(),
            message_idempotency_key: "message-1".into(),
            desired_runtime: SessionRuntimeSelection::provider(
                AgentId::parse("codex").unwrap(),
                ProviderProfileId::new(),
                "model",
            ),
            text: "hello".into(),
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: Some(CorrelationId::new()),
        });
        let ticket = controller.begin_send_message(&request).unwrap();
        // A same-session authoritative reload advances the view generation but
        // must keep the accepted mutation applicable.
        controller.state.generation = WorkflowViewGeneration(5);
        assert!(controller.apply_timeline_mutation(&ticket, Ok(Vec::new())));
        assert_eq!(controller.state.latest_mutation.phase, AsyncPhase::Ready);

        // Switching sessions advances the mutation generation and fences the
        // stale ticket out.
        let stale = controller.begin_send_message(&request).unwrap();
        controller.state.mutation_generation.advance();
        controller.state.pending_mutations.clear();
        assert!(!controller.apply_timeline_mutation(&stale, Ok(Vec::new())));
    }

    #[test]
    fn agent_mutation_rejects_cross_session_timeline_items() {
        let session = session();
        let backend = Arc::new(MockAgentBackend::new(session.clone(), Vec::new()));
        let mut controller = AgentWorkflowController::new(backend, capabilities());
        controller.state.selected_session_id = Some(session.id.clone());
        controller.state.generation = WorkflowViewGeneration(1);
        controller
            .state
            .timeline
            .replace_authoritative(session.id.clone(), Vec::new());
        let request = MutationRequest::new(SendAgentMessageRequest {
            session_id: session.id.clone(),
            message_idempotency_key: "message-2".into(),
            desired_runtime: SessionRuntimeSelection::provider(
                AgentId::parse("codex").unwrap(),
                ProviderProfileId::new(),
                "model",
            ),
            text: "hello".into(),
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        });
        let ticket = controller.begin_send_message(&request).unwrap();
        let other_session = VibexSessionId::new();
        let item = timeline_item(
            &other_session,
            1,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "private wrong-session response".into(),
                is_final: true,
            }),
        );
        assert!(controller.apply_timeline_mutation(&ticket, Ok(vec![item])));
        assert_eq!(
            controller
                .state
                .latest_mutation
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("agent_mutation_response_mismatch")
        );
        assert!(controller.state.timeline.items.is_empty());
    }

    #[test]
    fn agent_state_and_snapshot_debug_redact_session_and_timeline_text() {
        let mut session = session();
        session.title = "private session title".into();
        session.workspace_root = "/private/workspace/root".into();
        let item = timeline_item(
            &session.id,
            1,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "private timeline response".into(),
                is_final: true,
            }),
        );
        let snapshot = AgentSessionSnapshot {
            session: session.clone(),
            timeline: vec![item.clone()],
            runtime_selection: None,
        };
        let mut state = AgentWorkflowState::default();
        state.sessions.resolve(vec![session.clone()]);
        state.active_session.resolve(session.clone());
        state
            .timeline
            .replace_authoritative(session.id.clone(), [item]);
        let debug = format!("{state:?} {snapshot:?}");
        for secret in [
            "private session title",
            "/private/workspace/root",
            "private timeline response",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}");
        }
        assert!(debug.contains("timeline_item_count"));
    }
}

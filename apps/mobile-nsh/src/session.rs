//! The session shell's service: sessions, a transcript and the composer over
//! the connected runtime.
//!
//! It speaks the backend traits directly (`vibex-backend`): list sessions,
//! fetch a session's timeline, fetch its runtime selection and send a message.
//! The rows are the shared projection `vibex-desktop-model` computes, so the
//! phone shows what the desktop shows for the same timeline.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::Arc,
};

use nsh_base::{Terminal, TerminalApi};
use nsh_core::prelude::*;
use vibex_backend::{
    AgentBackend, BackendError, BackendEvent, BackendResult, MutationRequest, TerminalBackend,
};
use vibex_core::{
    AgentCommandDiscoverRequest, AgentCommandTrigger, ApprovalResponder, CreateAgentSessionRequest,
    ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationRequest,
    ElicitationRequestStatus, ElicitationResolution, ElicitationResolutionAction,
    FetchTimelineRequest, MessageProvenance, PermissionRequest, PermissionRequestStatus,
    PermissionResolution, PermissionResponseKind, RequestId, ResolveElicitationRequest,
    ResolvePermissionRequest, SendAgentMessageRequest, TerminalCreateRequest, TerminalId,
    TerminalStatus, TerminalWriteRequest, TimelinePage, TimelinePayload, UserMessageDelivery,
    VibexSessionId, unix_timestamp_ms,
};
use vibex_desktop_model::TimelineModel;
use vibex_mobile_pairing::MobileCredentialBundle;
use vibex_nsh_ui::{
    copy,
    screens::session::{
        CommandEntry, EntryKind, PendingPermission, PendingQuestion, PermissionChoice,
        PermissionOption, QuestionAnswer, QuestionField, QuestionFieldKind, QuestionOption,
        QuestionResponse, SessionHost, SessionState, SessionSummary, TimelineEntry,
    },
};

use crate::pairing::error_text;

/// How many timeline items one read asks for.
const TIMELINE_LIMIT: u32 = 200;

/// The session shell's state and actions.
#[derive(Clone)]
pub struct SessionService {
    backend: Arc<dyn AgentBackend>,
    terminal_backend: Arc<dyn TerminalBackend>,
    sessions: Signal<Vec<SessionSummary>>,
    active: Signal<Option<String>>,
    timeline: Signal<Vec<TimelineEntry>>,
    draft: Signal<String>,
    sending: Signal<bool>,
    error: Signal<Option<SharedString>>,
    runtime: Signal<Option<SharedString>>,
    pending: Signal<Option<PendingPermission>>,
    question: Signal<Option<PendingQuestion>>,
    /// The question the card shows, kept for typed submissions.
    question_request: Rc<RefCell<Option<ElicitationRequest>>>,
    commands: Signal<Vec<CommandEntry>>,
    commands_loaded: Rc<Cell<bool>>,
    terminal_open: Signal<bool>,
    terminal_title: Signal<Option<SharedString>>,
    terminal_error: Signal<Option<SharedString>>,
    terminal_ref: ComponentRef<Terminal>,
    terminal_id: Rc<RefCell<Option<TerminalId>>>,
    /// Bumped when the terminal closes, so its stream stops writing.
    terminal_generation: Rc<Cell<u64>>,
    resolving: Signal<bool>,
    /// The session's projection, kept for the live stream to append to.
    model: Rc<RefCell<TimelineModel>>,
    on_disconnect: Rc<dyn Fn()>,
}

impl SessionService {
    /// Builds the service for a connected runtime and loads its sessions.
    pub fn new(
        bundle: &MobileCredentialBundle,
        on_disconnect: Rc<dyn Fn()>,
    ) -> BackendResult<Self> {
        let backend = bundle.backend()?;
        let terminal_backend: Arc<dyn TerminalBackend> = backend.clone();
        let backend: Arc<dyn AgentBackend> = backend;
        let runtime = bundle.host_label();
        let service = Self {
            backend,
            terminal_backend,
            sessions: signal(Vec::new()),
            active: signal(None),
            timeline: signal(Vec::new()),
            draft: signal(String::new()),
            sending: signal(false),
            error: signal(None),
            runtime: signal(Some(runtime.into())),
            pending: signal(None),
            question: signal(None),
            question_request: Rc::new(RefCell::new(None)),
            commands: signal(Vec::new()),
            commands_loaded: Rc::new(Cell::new(false)),
            terminal_open: signal(false),
            terminal_title: signal(None),
            terminal_error: signal(None),
            terminal_ref: component_ref::<Terminal>(),
            terminal_id: Rc::new(RefCell::new(None)),
            terminal_generation: Rc::new(Cell::new(0)),
            resolving: signal(false),
            model: Rc::new(RefCell::new(TimelineModel::default())),
            on_disconnect,
        };
        service.refresh();
        service.subscribe();
        Ok(service)
    }

    /// The shell's bindings and callbacks.
    pub fn host(&self) -> SessionHost {
        SessionHost {
            sessions: self.sessions,
            active: self.active,
            timeline: self.timeline,
            draft: self.draft,
            sending: self.sending,
            error: self.error,
            runtime: self.runtime,
            pending: self.pending,
            question: self.question,
            commands: self.commands,
            terminal_open: self.terminal_open,
            terminal_title: self.terminal_title,
            terminal_error: self.terminal_error,
            terminal_ref: self.terminal_ref,
            resolving: self.resolving,
            on_select: {
                let service = self.clone();
                Rc::new(move |id: &str| service.open(id))
            },
            on_send: {
                let service = self.clone();
                Rc::new(move |text: &str| service.send(text))
            },
            on_refresh: {
                let service = self.clone();
                Rc::new(move || service.refresh())
            },
            on_disconnect: self.on_disconnect.clone(),
            on_permission: {
                let service = self.clone();
                Rc::new(move |request_id: &str, choice: PermissionChoice| {
                    service.resolve_permission(request_id, choice)
                })
            },
            on_question: {
                let service = self.clone();
                Rc::new(move |request_id: &str, response: QuestionResponse| {
                    service.resolve_question(request_id, response)
                })
            },
            on_command_query: {
                let service = self.clone();
                Rc::new(move |query: &str| service.query_commands(query))
            },
            on_new_session: {
                let service = self.clone();
                Rc::new(move || service.new_session())
            },
            on_open_terminal: {
                let service = self.clone();
                Rc::new(move || service.open_terminal())
            },
            on_close_terminal: {
                let service = self.clone();
                Rc::new(move || service.close_terminal())
            },
            on_terminal_data: {
                let service = self.clone();
                Rc::new(move |data: &str| service.terminal_input(data))
            },
        }
    }

    /// Reloads the session list, opening the newest session when none is open.
    fn refresh(&self) {
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = background(async move { backend.list_sessions(false).await }).await;
            match result {
                Ok(Ok(sessions)) => {
                    let rows = sessions.iter().map(summary).collect::<Vec<_>>();
                    service.sessions.set(rows);
                    if service.active.get_untracked().is_none()
                        && let Some(first) = sessions.first()
                    {
                        service.open(first.id.as_str());
                    }
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("listing sessions did not finish: {error}"),
                )),
            }
        });
    }

    /// Follows the runtime's event stream for the life of the shell.
    ///
    /// The stream is the authority's: a live timeline item is applied to the
    /// same projection the last read filled, so a streamed reply grows in
    /// place. A stream that ends re-syncs the list once; the backend
    /// reconnects on its next call.
    fn subscribe(&self) {
        let subscription = match self.backend.subscribe() {
            Ok(subscription) => subscription,
            Err(_) => return,
        };
        let service = self.clone();
        spawn(async move {
            let mut subscription = subscription;
            // The stream ends (or the transport drops): one refresh re-syncs
            // the list, and the backend reconnects on its next call.
            while let Ok(Some(event)) = subscription.next().await {
                service.apply_event(event);
            }
            service.refresh();
        });
    }

    /// Applies one backend event to what the shell shows.
    fn apply_event(&self, event: BackendEvent) {
        match event {
            BackendEvent::Timeline(live) => {
                if self.active.get_untracked().as_deref() == Some(live.session_id.as_str()) {
                    let changed = self.model.borrow_mut().apply_live(live);
                    if changed {
                        self.publish();
                    }
                }
            }
            BackendEvent::SessionUpdated(session) => {
                let row = summary(&session);
                self.sessions.update(|sessions| {
                    if let Some(existing) =
                        sessions.iter_mut().find(|existing| existing.id == row.id)
                    {
                        *existing = row;
                    } else {
                        sessions.insert(0, row);
                    }
                });
            }
            BackendEvent::SessionsRemoved(removed) => {
                let ids = removed
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect::<Vec<_>>();
                self.sessions
                    .update(|sessions| sessions.retain(|session| !ids.contains(&session.id)));
                let active = self.active.get_untracked();
                if active.as_ref().is_some_and(|active| ids.contains(active)) {
                    self.active.set(None);
                    self.timeline.set(Vec::new());
                    *self.model.borrow_mut() = TimelineModel::default();
                }
            }
            _ => {}
        }
    }

    /// Opens `id`: its transcript replaces what is shown.
    fn open(&self, id: &str) {
        let Ok(session_id) = VibexSessionId::parse(id) else {
            self.fail(&BackendError::failed(
                "session_id_invalid",
                "the session id is not a Vibex session id",
            ));
            return;
        };
        self.active.set(Some(session_id.as_str().to_owned()));
        self.timeline.set(Vec::new());
        self.pending.set(None);
        self.question.set(None);
        *self.question_request.borrow_mut() = None;
        self.commands.set(Vec::new());
        self.commands_loaded.set(false);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = background(fetch_page(backend, session_id.clone())).await;
            match result {
                Ok(Ok(page)) => service.apply_page(&session_id, page),
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("opening the session did not finish: {error}"),
                )),
            }
        });
    }

    /// Sends `text` to the open session and replaces the transcript with the
    /// authority's answer.
    fn send(&self, text: &str) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let text = text.trim().to_owned();
        if text.is_empty() || self.sending.get_untracked() {
            return;
        }
        self.sending.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = background(send_and_refetch(backend, session_id.clone(), text)).await;
            service.sending.set(false);
            match result {
                Ok(Ok(page)) => service.apply_page(&session_id, page),
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("sending the message did not finish: {error}"),
                )),
            }
        });
    }

    /// Projects a fetched page into the shell's rows.
    fn apply_page(&self, session_id: &VibexSessionId, page: TimelinePage) {
        self.model
            .borrow_mut()
            .replace_authoritative(session_id.clone(), page.items);
        self.publish();
    }

    /// Re-projects the kept model into the shell's rows.
    fn publish(&self) {
        let rows = self
            .model
            .borrow()
            .rows()
            .into_iter()
            .map(|row| TimelineEntry {
                id: row.id,
                kind: entry_kind(row.kind),
                title: row.title,
                body: row.body,
                meta: String::new(),
                streaming: row.streaming,
            })
            .collect::<Vec<_>>();
        self.timeline.set(rows);
        self.refresh_pending();
    }

    /// Re-reads the pending permission from the projection.
    ///
    /// The newest pending request wins: a runtime blocks on one at a time per
    /// session, and an older one still marked pending cannot be answered.
    fn refresh_pending(&self) {
        let Some(active) = self.active.get_untracked() else {
            self.pending.set(None);
            self.question.set(None);
            *self.question_request.borrow_mut() = None;
            return;
        };
        let mut pending = None;
        let mut question = None;
        let mut question_request = None;
        {
            let model = self.model.borrow();
            for item in model.items.iter().rev() {
                match &item.payload {
                    TimelinePayload::PermissionRequest(request)
                        if pending.is_none()
                            && request.status == PermissionRequestStatus::Pending
                            && request.session_id.as_str() == active =>
                    {
                        pending = Some(permission_pending(request));
                    }
                    TimelinePayload::ElicitationRequest(request)
                        if question.is_none()
                            && request.status == ElicitationRequestStatus::Pending
                            && request.session_id.as_str() == active =>
                    {
                        question = Some(pending_question(request));
                        question_request = Some(request.clone());
                    }
                    _ => {}
                }
                if pending.is_some() && question.is_some() {
                    break;
                }
            }
        }
        self.pending.set(pending);
        self.question.set(question);
        *self.question_request.borrow_mut() = question_request;
    }

    /// Answers a permission card and re-reads the transcript.
    fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) {
        if self.resolving.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Ok(request_id) = RequestId::parse(request_id) else {
            return;
        };
        let response = match choice {
            PermissionChoice::Allow => PermissionResponseKind::Approve,
            PermissionChoice::AlwaysAllow => PermissionResponseKind::AlwaysAllowForSession,
            PermissionChoice::Deny => PermissionResponseKind::Deny,
        };
        self.resolving.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request_session = session_id.clone();
            let request_request = request_id.clone();
            let result = background(async move {
                backend
                    .resolve_permission(MutationRequest::new(ResolvePermissionRequest {
                        session_id: request_session.clone(),
                        request_id: request_request.clone(),
                        resolution: PermissionResolution {
                            request_id: request_request,
                            session_id: request_session,
                            response,
                            responder_device_id: None,
                            provider_resolution_id: None,
                            note: None,
                            resolved_at_ms: unix_timestamp_ms(),
                            responder: Some(ApprovalResponder::Human),
                        },
                    }))
                    .await
            })
            .await;
            service.resolving.set(false);
            match result {
                Ok(Ok(_)) => {
                    service.pending.set(None);
                    service.reload(&session_id);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("resolving the permission did not finish: {error}"),
                )),
            }
        });
    }

    /// Answers a question card and re-reads the transcript.
    fn resolve_question(&self, request_id: &str, response: QuestionResponse) {
        if self.resolving.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Ok(request_id) = RequestId::parse(request_id) else {
            return;
        };
        let Some(request) = self.question_request.borrow().clone() else {
            return;
        };
        let (action, answers) = match response {
            QuestionResponse::Decline => (ElicitationResolutionAction::Decline, BTreeMap::new()),
            QuestionResponse::Submit(answers) => match answers_for(&request, &answers) {
                Ok(answers) => (ElicitationResolutionAction::Accept, answers),
                Err(message) => {
                    self.fail_text(&message);
                    return;
                }
            },
        };
        self.resolving.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request_session = session_id.clone();
            let request_request = request_id.clone();
            let result = background(async move {
                backend
                    .resolve_elicitation(MutationRequest::new(ResolveElicitationRequest {
                        session_id: request_session.clone(),
                        request_id: request_request.clone(),
                        resolution: ElicitationResolution {
                            request_id: request_request,
                            session_id: request_session,
                            action,
                            answers,
                            responder_device_id: None,
                            resolved_at_ms: unix_timestamp_ms(),
                        },
                    }))
                    .await
            })
            .await;
            service.resolving.set(false);
            match result {
                Ok(Ok(_)) => {
                    service.question.set(None);
                    service.reload(&session_id);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("resolving the question did not finish: {error}"),
                )),
            }
        });
    }

    /// Loads the `/` commands the open session offers.
    ///
    /// One read per session: the palette filters what came back locally as the
    /// person keeps typing, so typing does not make a request per keystroke.
    fn query_commands(&self, _query: &str) {
        if self.commands_loaded.get() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.commands_loaded.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = background(async move {
                backend
                    .discover_agent_commands(AgentCommandDiscoverRequest {
                        agent_id: None,
                        provider_profile_id: None,
                        session_id: Some(session_id),
                        workspace_id: None,
                        trigger: Some(AgentCommandTrigger::Slash),
                        query: None,
                        limit: Some(30),
                    })
                    .await
            })
            .await;
            if let Ok(Ok(discovery)) = result {
                let mut entries = Vec::new();
                let mut seen = std::collections::BTreeSet::new();
                for entry in discovery
                    .response
                    .entries
                    .into_iter()
                    .chain(discovery.quick_phrases)
                {
                    if seen.insert(entry.id.clone()) {
                        entries.push(CommandEntry {
                            id: entry.id,
                            label: entry.label,
                            description: entry.description.unwrap_or_default(),
                            insertion: entry.insertion_text,
                        });
                    }
                }
                service.commands.set(entries);
            }
        });
    }

    /// Starts a session in the open session's workspace with its runtime.
    fn new_session(&self) {
        let Some(active) = self.active.get_untracked() else {
            self.fail_text(copy::text(
                "Open a session first; the new one reuses its workspace.",
                "请先打开一个会话，新会话会复用它的工作区。",
                "請先開啟一個工作階段，新工作階段會沿用它的工作區。",
            ));
            return;
        };
        let Ok(active_id) = VibexSessionId::parse(active) else {
            return;
        };
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = background(async move {
                let session = backend.open_session(active_id.clone()).await?;
                let selection = backend.runtime_selection(active_id).await?.desired;
                backend
                    .create_session(MutationRequest::new(CreateAgentSessionRequest {
                        runtime: selection,
                        workspace_root: session.workspace_root,
                        workspace_mode: session.workspace_mode,
                        title: None,
                        safety: None,
                        session_id: Some(VibexSessionId::new()),
                        defer_runtime_materialization: false,
                        tools: Default::default(),
                    }))
                    .await
            })
            .await;
            match result {
                Ok(Ok(session)) => {
                    service.refresh();
                    service.open(session.id.as_str());
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("creating the session did not finish: {error}"),
                )),
            }
        });
    }

    /// Opens the workspace's terminal full-screen over the shell.
    fn open_terminal(&self) {
        if self.terminal_open.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            self.fail_text(copy::text(
                "Open a session first; its workspace owns the terminal.",
                "请先打开一个会话，终端属于它的工作区。",
                "請先開啟一個工作階段，終端屬於它的工作區。",
            ));
            return;
        };
        let Ok(active_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.terminal_open.set(true);
        self.terminal_error.set(None);
        self.terminal_title
            .set(Some(copy::text("Terminal", "终端", "終端").into()));
        let generation = self.terminal_generation.get().wrapping_add(1);
        self.terminal_generation.set(generation);
        let service = self.clone();
        let backend = self.backend.clone();
        let terminal_backend = self.terminal_backend.clone();
        let opening_terminal = terminal_backend.clone();
        spawn(async move {
            let result = background(async move {
                let session = backend.open_session(active_id).await?;
                let terminals = opening_terminal
                    .list_terminals(session.workspace_id.clone())
                    .await?;
                let terminal = match terminals
                    .into_iter()
                    .find(|terminal| terminal.status == TerminalStatus::Running)
                {
                    Some(terminal) => terminal,
                    None => {
                        opening_terminal
                            .create_terminal(MutationRequest::new(TerminalCreateRequest {
                                workspace_id: session.workspace_id,
                                title: None,
                                shell: None,
                                cwd: None,
                                rows: 24,
                                cols: 80,
                            }))
                            .await?
                    }
                };
                let snapshot = opening_terminal
                    .terminal_snapshot(terminal.id.clone())
                    .await?;
                Ok::<_, BackendError>((terminal, snapshot))
            })
            .await;
            let (terminal, snapshot) = match result {
                Ok(Ok(opened)) => opened,
                Ok(Err(error)) => {
                    service.terminal_error.set(Some(error_text(&error)));
                    service.terminal_open.set(false);
                    return;
                }
                Err(error) => {
                    service
                        .terminal_error
                        .set(Some(format!("the terminal did not open: {error}").into()));
                    service.terminal_open.set(false);
                    return;
                }
            };
            *service.terminal_id.borrow_mut() = Some(terminal.id.clone());
            service
                .terminal_title
                .set(Some(terminal.title.clone().into()));
            for chunk in snapshot.chunks {
                service.write_terminal_output(chunk.data);
            }
            let subscription = match terminal_backend
                .subscribe_terminal(terminal.id.clone(), snapshot.next_sequence)
            {
                Ok(subscription) => subscription,
                Err(error) => {
                    service.terminal_error.set(Some(error_text(&error)));
                    return;
                }
            };
            let mut subscription = subscription;
            while service.terminal_generation.get() == generation {
                match subscription.next().await {
                    Ok(Some(batch)) => {
                        if batch.reset_required {
                            service.reset_terminal_output();
                        }
                        for frame in batch.frames {
                            service.write_terminal_output(
                                String::from_utf8_lossy(&frame.bytes).into_owned(),
                            );
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        });
    }

    /// Closes the terminal surface; the PTY keeps running on the runtime.
    fn close_terminal(&self) {
        self.terminal_generation
            .set(self.terminal_generation.get().wrapping_add(1));
        self.terminal_open.set(false);
        *self.terminal_id.borrow_mut() = None;
    }

    /// Sends what the person typed to the PTY.
    fn terminal_input(&self, data: &str) {
        let Some(terminal_id) = self.terminal_id.borrow().clone() else {
            return;
        };
        let data = data.to_owned();
        let backend = self.terminal_backend.clone();
        spawn(async move {
            let result = background(async move {
                backend
                    .write_terminal(MutationRequest::new(TerminalWriteRequest {
                        terminal_id,
                        data,
                    }))
                    .await
            })
            .await;
            let _ = result;
        });
    }

    fn write_terminal_output(&self, text: String) {
        if let Some(api) = self.terminal_ref.api::<TerminalApi>() {
            api.write(text);
        }
    }

    fn reset_terminal_output(&self) {
        if let Some(api) = self.terminal_ref.api::<TerminalApi>() {
            api.reset();
        }
    }

    /// Re-reads the open session's transcript without clearing it.
    fn reload(&self, session_id: &VibexSessionId) {
        let service = self.clone();
        let backend = self.backend.clone();
        let session_id = session_id.clone();
        spawn(async move {
            let result = background(fetch_page(backend, session_id.clone())).await;
            match result {
                Ok(Ok(page)) => service.apply_page(&session_id, page),
                Ok(Err(error)) => service.fail(&error),
                Err(_) => {}
            }
        });
    }

    fn fail(&self, error: &BackendError) {
        self.sending.set(false);
        self.resolving.set(false);
        self.error.set(Some(error_text(error)));
    }

    fn fail_text(&self, message: &str) {
        self.sending.set(false);
        self.resolving.set(false);
        self.error.set(Some(message.to_owned().into()));
    }
}

/// Lists the sessions and fetches one page of a session's timeline.
async fn fetch_page(
    backend: Arc<dyn AgentBackend>,
    session_id: VibexSessionId,
) -> BackendResult<TimelinePage> {
    backend
        .fetch_timeline(FetchTimelineRequest {
            session_id,
            after_sequence: None,
            before_sequence: None,
            limit: TIMELINE_LIMIT,
            max_turns: None,
            max_bytes: None,
        })
        .await
}

/// Sends `text` and reads the transcript back.
async fn send_and_refetch(
    backend: Arc<dyn AgentBackend>,
    session_id: VibexSessionId,
    text: String,
) -> BackendResult<TimelinePage> {
    // The session's own selection is what the runtime expects the send to
    // carry; asking first also notices a runtime that cannot take a turn.
    let selection = backend.runtime_selection(session_id.clone()).await?.desired;
    backend
        .send_message(MutationRequest::new(SendAgentMessageRequest {
            session_id: session_id.clone(),
            message_idempotency_key: vibex_core::RequestId::new().into_string(),
            desired_runtime: selection,
            text,
            attachments: Vec::new(),
            mentions: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
            delivery: UserMessageDelivery::Prompt,
            prompt_context: None,
            provenance: MessageProvenance::HumanInput,
        }))
        .await?;
    fetch_page(backend, session_id).await
}

fn summary(session: &vibex_core::AgentSession) -> SessionSummary {
    SessionSummary {
        id: session.id.as_str().to_owned(),
        title: if session.title.trim().is_empty() {
            copy::text("New session", "新会话", "新工作階段").to_owned()
        } else {
            session.title.clone()
        },
        agent: session.agent_id.as_str().to_owned(),
        meta: age(session.updated_at_ms.max(session.last_message_at_ms)),
        state: match session.state {
            vibex_core::AgentSessionState::Running
            | vibex_core::AgentSessionState::Initializing => SessionState::Running,
            vibex_core::AgentSessionState::NeedsInput => SessionState::Waiting,
            vibex_core::AgentSessionState::Error => SessionState::Failed,
            _ => SessionState::Idle,
        },
    }
}

/// The card the phone shows for a pending question.
fn pending_question(request: &ElicitationRequest) -> PendingQuestion {
    PendingQuestion {
        request_id: request.id.as_str().to_owned(),
        title: request
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| copy::text("A question", "一个问题", "一個問題").to_owned()),
        message: request
            .description
            .clone()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| request.message.clone()),
        fields: request.fields.iter().filter_map(question_field).collect(),
    }
}

fn question_field(field: &ElicitationField) -> Option<QuestionField> {
    let kind = match &field.kind {
        ElicitationFieldKind::Text {
            default, options, ..
        } if !options.is_empty() => QuestionFieldKind::Choice {
            options: options
                .iter()
                .map(|option| QuestionOption {
                    value: option.value.clone(),
                    label: option.title.clone(),
                })
                .collect(),
            default: default.clone(),
        },
        ElicitationFieldKind::Text { default, .. } => QuestionFieldKind::Text {
            default: default.clone(),
        },
        ElicitationFieldKind::Number { default, .. } => QuestionFieldKind::Text {
            default: default.clone(),
        },
        ElicitationFieldKind::Integer { default, .. } => QuestionFieldKind::Text {
            default: default.map(|value| value.to_string()),
        },
        ElicitationFieldKind::Boolean { default } => {
            QuestionFieldKind::Boolean { default: *default }
        }
        ElicitationFieldKind::MultiSelect {
            options, default, ..
        } => QuestionFieldKind::Multi {
            options: options
                .iter()
                .map(|option| QuestionOption {
                    value: option.value.clone(),
                    label: option.title.clone(),
                })
                .collect(),
            default: default.clone(),
        },
        // A schema the phone cannot draw is left out rather than guessed at.
        ElicitationFieldKind::Unsupported { .. } => return None,
    };
    Some(QuestionField {
        id: field.id.clone(),
        title: field.title.clone(),
        description: field.description.clone(),
        required: field.required,
        kind,
    })
}

/// The typed answers a question's fields expect.
fn answers_for(
    request: &ElicitationRequest,
    answers: &[(String, QuestionAnswer)],
) -> Result<BTreeMap<String, ElicitationAnswerValue>, String> {
    let mut typed = BTreeMap::new();
    for field in &request.fields {
        let answer = answers
            .iter()
            .find(|(id, _)| id == &field.id)
            .map(|(_, answer)| answer);
        let value = match (&field.kind, answer) {
            (ElicitationFieldKind::Text { .. }, Some(QuestionAnswer::Text(text))) => {
                ElicitationAnswerValue::String(text.clone())
            }
            (ElicitationFieldKind::Number { .. }, Some(QuestionAnswer::Text(text))) => {
                ElicitationAnswerValue::Number(text.clone())
            }
            (ElicitationFieldKind::Integer { .. }, Some(QuestionAnswer::Text(text))) => {
                let value = text
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("`{}` is not a whole number.", field.title))?;
                ElicitationAnswerValue::Integer(value)
            }
            (ElicitationFieldKind::Boolean { .. }, Some(QuestionAnswer::Boolean(value))) => {
                ElicitationAnswerValue::Boolean(*value)
            }
            (ElicitationFieldKind::MultiSelect { .. }, Some(QuestionAnswer::Texts(values))) => {
                ElicitationAnswerValue::StringArray(values.clone())
            }
            (ElicitationFieldKind::Unsupported { .. }, _) => continue,
            (_, None) if !field.required => continue,
            _ => return Err(format!("`{}` needs an answer.", field.title)),
        };
        typed.insert(field.id.clone(), value);
    }
    Ok(typed)
}

/// The card the phone shows for a pending request.
fn permission_pending(request: &PermissionRequest) -> PendingPermission {
    let options = if request.response_options.is_empty() {
        request
            .allowed_responses
            .iter()
            .map(|kind| PermissionOption {
                id: format!("{kind:?}"),
                label: default_option_label(*kind).to_owned(),
                choice: choice_of(*kind),
            })
            .collect()
    } else {
        request
            .response_options
            .iter()
            .map(|option| PermissionOption {
                id: option.option_id.clone(),
                label: option.label.clone(),
                choice: choice_of(option.response),
            })
            .collect()
    };
    PendingPermission {
        request_id: request.id.as_str().to_owned(),
        title: request.title.clone(),
        details: request
            .details
            .iter()
            .map(|detail| {
                if detail.value.trim().is_empty() {
                    detail.label.clone()
                } else {
                    format!("{}: {}", detail.label, detail.value)
                }
            })
            .take(6)
            .collect(),
        options,
    }
}

fn choice_of(response: PermissionResponseKind) -> PermissionChoice {
    match response {
        PermissionResponseKind::Approve => PermissionChoice::Allow,
        PermissionResponseKind::AlwaysAllowForSession => PermissionChoice::AlwaysAllow,
        PermissionResponseKind::Deny => PermissionChoice::Deny,
    }
}

fn default_option_label(response: PermissionResponseKind) -> &'static str {
    match response {
        PermissionResponseKind::Approve => copy::text("Allow", "允许", "允許"),
        PermissionResponseKind::AlwaysAllowForSession => {
            copy::text("Always allow", "本会话始终允许", "本工作階段一律允許")
        }
        PermissionResponseKind::Deny => copy::text("Deny", "拒绝", "拒絕"),
    }
}

fn entry_kind(kind: vibex_desktop_model::TimelineRowKind) -> EntryKind {
    use vibex_desktop_model::TimelineRowKind as Row;
    match kind {
        Row::UserMessage => EntryKind::User,
        Row::AgentMessage => EntryKind::Agent,
        Row::Reasoning => EntryKind::Reasoning,
        Row::Error | Row::Retry => EntryKind::Error,
        Row::ToolCall
        | Row::Command
        | Row::FileOperation
        | Row::WebSearch
        | Row::TodoUpdate
        | Row::Plan
        | Row::Collaboration
        | Row::ImageGeneration => EntryKind::Tool,
        Row::GitNotice
        | Row::SystemNotice
        | Row::PermissionRequest
        | Row::PermissionResolution
        | Row::ElicitationRequest
        | Row::ElicitationResolution => EntryKind::Notice,
    }
}

/// A short age for a row ("now", "5m", "2h", "3d").
fn age(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return String::new();
    }
    let seconds = (unix_timestamp_ms() - timestamp_ms).max(0) / 1_000;
    match seconds {
        0..=59 => copy::text("now", "刚刚", "剛剛").to_owned(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

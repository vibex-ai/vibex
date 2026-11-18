//! The session shell's service: sessions, a transcript and the composer over
//! the connected runtime.
//!
//! It speaks the backend traits directly (`vibex-backend`): list sessions,
//! fetch a session's timeline, fetch its runtime selection and send a message.
//! The rows are the shared projection `vibex-desktop-model` computes, so the
//! phone shows what the desktop shows for the same timeline.

use std::{rc::Rc, sync::Arc};

use nsh_core::prelude::*;
use vibex_backend::{AgentBackend, BackendError, BackendResult, MutationRequest};
use vibex_core::{
    FetchTimelineRequest, MessageProvenance, SendAgentMessageRequest, TimelinePage,
    UserMessageDelivery, VibexSessionId, unix_timestamp_ms,
};
use vibex_desktop_model::TimelineModel;
use vibex_mobile_pairing::MobileCredentialBundle;
use vibex_nsh_ui::{
    copy,
    screens::session::{EntryKind, SessionHost, SessionState, SessionSummary, TimelineEntry},
};

use crate::pairing::error_text;

/// How many timeline items one read asks for.
const TIMELINE_LIMIT: u32 = 200;

/// The session shell's state and actions.
#[derive(Clone)]
pub struct SessionService {
    backend: Arc<dyn AgentBackend>,
    sessions: Signal<Vec<SessionSummary>>,
    active: Signal<Option<String>>,
    timeline: Signal<Vec<TimelineEntry>>,
    draft: Signal<String>,
    sending: Signal<bool>,
    error: Signal<Option<SharedString>>,
    runtime: Signal<Option<SharedString>>,
    on_disconnect: Rc<dyn Fn()>,
}

impl SessionService {
    /// Builds the service for a connected runtime and loads its sessions.
    pub fn new(
        bundle: &MobileCredentialBundle,
        on_disconnect: Rc<dyn Fn()>,
    ) -> BackendResult<Self> {
        let backend: Arc<dyn AgentBackend> = bundle.backend()?;
        let runtime = bundle.host_label();
        let service = Self {
            backend,
            sessions: signal(Vec::new()),
            active: signal(None),
            timeline: signal(Vec::new()),
            draft: signal(String::new()),
            sending: signal(false),
            error: signal(None),
            runtime: signal(Some(runtime.into())),
            on_disconnect,
        };
        service.refresh();
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
        let mut model = TimelineModel::default();
        model.replace_authoritative(session_id.clone(), page.items);
        let rows = model
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
    }

    fn fail(&self, error: &BackendError) {
        self.sending.set(false);
        self.error.set(Some(error_text(error)));
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

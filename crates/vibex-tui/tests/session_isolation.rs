//! Session ownership regressions through the public intent/effect interface.

use vibex_core::*;
use vibex_tui::action::Intent;
use vibex_tui::app::LiveState;
use vibex_tui::{App, AppOptions, Effect, Page};

fn app() -> App {
    let facade = vibex_backend::DisconnectedBackend::facade();
    facade.replace_capabilities(vibex_backend::BackendCapabilitySnapshot::desktop_native_v1());
    let mut app = App::new(
        facade,
        AppOptions {
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.live = LiveState::Ready;
    app.resize(120, 40);
    app.runtime_options = Some(SessionRuntimeOptionCatalog {
        revision: 1,
        agents: vec![],
        auth_sources: vec![],
        options: ["codex", "deepseek-harness"]
            .into_iter()
            .map(|agent| SessionRuntimeOption {
                selection: SessionRuntimeSelection::provider(
                    AgentId::parse(agent).unwrap(),
                    ProviderProfileId::new(),
                    "test-model",
                ),
                agent_label: agent.into(),
                auth_source_label: "test".into(),
                model_label: "test-model".into(),
                reasoning_efforts: vec![],
                modes: vec![],
                features: vec![],
                availability: RuntimeOptionAvailability::Available,
            })
            .collect(),
    });
    app
}

fn session(agent: &str) -> AgentSession {
    AgentSession {
        id: VibexSessionId::new(),
        title: format!("{agent} session"),
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        workspace_root: "/test/workspace".into(),
        workspace_mode: WorkspaceMode::CurrentCheckout,
        agent_id: AgentId::parse(agent).unwrap(),
        state: AgentSessionState::Idle,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: 1,
        updated_at_ms: 1,
        last_message_at_ms: 1,
        archived_at_ms: None,
        deleted_at_ms: None,
    }
}

fn message(session: &AgentSession, text: &str) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::new(),
        session_id: session.id.clone(),
        sequence: 1,
        timestamp_ms: 1,
        source: TimelineSource::User,
        kind: TimelineItemKind::UserMessage,
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload: TimelinePayload::UserMessage(UserMessagePayload {
            text: text.into(),
            ..Default::default()
        }),
    }
}

fn open(app: &mut App, session: &AgentSession, history: Vec<TimelineItem>) {
    let ticket = app
        .open_session_effects(session.id.clone())
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::OpenSession { ticket, .. } => Some(ticket),
            _ => None,
        })
        .expect("session load");
    assert!(app.agent.apply_session_snapshot(
        &ticket,
        Ok(vibex_ui::AgentSessionSnapshot {
            session: session.clone(),
            timeline: history,
            runtime_selection: None,
            timeline_has_older: false,
        }),
    ));
    app.sync_transcript();
}

fn create(app: &mut App, agent_index: usize, text: &str) -> Effect {
    app.perform(Intent::GotoSessions);
    app.perform(Intent::NewSession);
    app.new_session_runtime = Some(
        app.runtime_options.as_ref().unwrap().options[agent_index]
            .selection
            .clone(),
    );
    app.composer.insert_str(text);
    app.perform(Intent::SubmitComposer)
        .effects
        .into_iter()
        .find(|effect| matches!(effect, Effect::CreateSession { .. }))
        .expect("creation request")
}

#[test]
fn new_session_has_its_own_identity_and_chosen_agent() {
    let mut app = app();
    let old = session("codex");
    open(&mut app, &old, vec![message(&old, "old history")]);

    let Effect::CreateSession {
        request_id,
        runtime: Some(runtime),
        ..
    } = create(&mut app, 1, "new deepseek message")
    else {
        panic!("creation must carry a runtime");
    };
    assert_ne!(request_id, old.id);
    assert_eq!(runtime.agent_id.as_str(), "deepseek-harness");
    assert_eq!(app.selected_session_id(), Some(&request_id));
    assert_eq!(
        app.transcript.block(0).unwrap().body,
        "new deepseek message"
    );
    assert!(app.transcript.block(1).is_none());
}

#[test]
fn concurrent_creations_reserve_distinct_sessions() {
    let mut app = app();
    let Effect::CreateSession { request_id: a, .. } = create(&mut app, 0, "prompt A") else {
        unreachable!()
    };
    let Effect::CreateSession { request_id: b, .. } = create(&mut app, 1, "prompt B") else {
        unreachable!()
    };
    assert_ne!(a, b);
    assert_eq!(app.selected_session_id(), Some(&b));
    let pending = app.pending_send_for_active().expect("B's first message");
    assert_eq!(pending.session_id.as_ref(), Some(&b));
    assert_eq!(pending.text, "prompt B");
}

#[test]
fn switching_sessions_replaces_the_transcript_before_the_fetch_returns() {
    let mut app = app();
    let a = session("codex");
    let b = session("deepseek-harness");
    open(&mut app, &a, vec![message(&a, "only in A")]);
    assert_eq!(app.transcript.block(0).unwrap().body, "only in A");

    app.open_session_effects(b.id.clone());
    assert_eq!(app.selected_session_id(), Some(&b.id));
    assert!(app.transcript.block(0).is_none());
    assert!(app.projection.rows.is_empty());
}

#[test]
fn a_creating_sessions_prompt_does_not_appear_in_an_existing_session() {
    let mut app = app();
    create(&mut app, 1, "private draft for new session");
    let old = session("codex");
    open(&mut app, &old, vec![]);
    assert!(app.pending_send_for_active().is_none());
    assert!(app.transcript.block(0).is_none());
}

#[test]
fn existing_sessions_keep_separate_unsent_composers() {
    let mut app = app();
    let a = session("codex");
    let b = session("deepseek-harness");
    open(&mut app, &a, vec![]);
    app.composer.insert_str("unsent A");
    open(&mut app, &b, vec![]);
    assert!(app.composer.is_empty());
    app.composer.insert_str("unsent B");
    open(&mut app, &a, vec![]);
    assert_eq!(app.composer.text(), "unsent A");
    open(&mut app, &b, vec![]);
    assert_eq!(app.composer.text(), "unsent B");
}

#[test]
fn leaving_new_session_restores_existing_composer_without_losing_the_new_draft() {
    let mut app = app();
    let old = session("codex");
    open(&mut app, &old, vec![]);
    app.composer.insert_str("old session draft");
    app.perform(Intent::GotoSessions);
    app.perform(Intent::NewSession);
    assert!(app.composer.is_empty());
    app.composer.insert_str("new session draft");
    app.perform(Intent::Back);
    open(&mut app, &old, vec![]);
    assert_eq!(app.composer.text(), "old session draft");
    app.perform(Intent::GotoSessions);
    app.perform(Intent::NewSession);
    assert_eq!(app.page, Page::NewSession);
    assert_eq!(app.composer.text(), "new session draft");
}

#[test]
fn a_second_message_waits_for_its_session_to_be_created() {
    let mut app = app();
    create(&mut app, 0, "first message");
    app.composer.insert_str("second message");
    let outcome = app.perform(Intent::SubmitComposer);
    assert!(
        !outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::SendMessage { .. })),
        "the second message must not reach a session that has not been created"
    );
    assert!(
        app.composer.text() == "second message"
            || app
                .queued_messages
                .iter()
                .any(|message| message.text == "second message"),
        "the second message must remain recoverable"
    );
}

#[test]
fn a_failed_creation_does_not_orphan_a_newer_draft_stored_offscreen() {
    let mut app = app();
    let Effect::CreateSession { request_id, .. } = create(&mut app, 0, "first attempt") else {
        unreachable!()
    };
    app.perform(Intent::GotoSessions);
    app.perform(Intent::NewSession);
    app.composer.insert_str("newer draft B");
    let runtime = app.runtime_options.as_ref().unwrap().options[1]
        .selection
        .clone();
    app.new_session_runtime = Some(runtime.clone());
    app.workspace_path = Some("/test/workspace-B".into());

    app.open_session_effects(request_id.clone());
    assert!(app.composer.is_empty());
    assert!(app.fail_creation(&request_id));
    app.perform(Intent::GotoSessions);
    app.perform(Intent::NewSession);
    assert_eq!(app.composer.text(), "newer draft B");
    assert_eq!(app.new_session_runtime, Some(runtime));
    assert_eq!(app.new_session_workspace(), "/test/workspace-B");
}

#[test]
fn old_matching_history_does_not_confirm_a_new_background_send() {
    let mut app = app();
    let a = session("codex");
    let b = session("deepseek-harness");
    let old = message(&a, "continue");
    open(&mut app, &a, vec![old.clone()]);
    open(&mut app, &b, vec![]);
    app.mark_send_dispatched(Some(&a.id), "continue".into(), vec![]);
    open(&mut app, &a, vec![old]);
    app.settle_pending_send();
    assert!(
        app.pending_send_for_active().is_some(),
        "an old identical message is not acknowledgment of a new send"
    );
}

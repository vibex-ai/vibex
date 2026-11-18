//! The session shell's flows, rendered headlessly through the real engine.
//!
//! The shell is mounted with a fixture host here: the drawer opens, a session
//! opens its transcript, and the composer sends. The app's own wiring to the
//! remote backend is tested where that backend is available; these tests keep
//! the screen's behavior fixed as it grows.

use std::rc::Rc;

use nsh_core::prelude::*;
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use vibex_nsh_ui::screens::session::{
    self, CommandEntry, EntryKind, PendingPermission, PendingQuestion, PermissionChoice,
    PermissionOption, QuestionField, QuestionFieldKind, SessionHost, SessionState, SessionSummary,
    TimelineEntry,
};

fn entry(id: &str, kind: EntryKind, title: &str, body: &str) -> TimelineEntry {
    TimelineEntry {
        id: id.to_owned(),
        kind,
        title: title.to_owned(),
        body: body.to_owned(),
        meta: String::new(),
        streaming: false,
    }
}

fn sessions() -> Vec<SessionSummary> {
    vec![
        SessionSummary {
            id: "s1".to_owned(),
            title: "Good morning".to_owned(),
            agent: "Claude".to_owned(),
            meta: "2m".to_owned(),
            state: SessionState::Running,
        },
        SessionSummary {
            id: "s2".to_owned(),
            title: "Fix the tests".to_owned(),
            agent: "Codex".to_owned(),
            meta: "1h".to_owned(),
            state: SessionState::Idle,
        },
    ]
}

fn transcript(session: &str) -> Vec<TimelineEntry> {
    match session {
        "s1" => vec![
            entry("s1:1", EntryKind::User, "", "Good morning"),
            entry(
                "s1:2",
                EntryKind::Agent,
                "Claude",
                "Morning. The build is green.",
            ),
            entry("s1:3", EntryKind::Tool, "cargo test", "Running 412 tests"),
        ],
        _ => vec![entry("s2:1", EntryKind::User, "", "Please fix the tests")],
    }
}

/// The shell over a fixture that behaves like the app's service for the parts
/// these tests cover.
#[component]
fn Shell() -> impl IntoElement {
    session::session(host_with(None, None))
}

/// The same shell with a permission waiting for an answer.
#[component]
fn PermissionShell() -> impl IntoElement {
    session::session(host_with(
        Some(PendingPermission {
            request_id: "req_1".to_owned(),
            title: "Run cargo test?".to_owned(),
            details: vec!["Command: cargo test".to_owned()],
            options: vec![
                PermissionOption {
                    id: "approve".to_owned(),
                    label: "Allow".to_owned(),
                    choice: PermissionChoice::Allow,
                },
                PermissionOption {
                    id: "deny".to_owned(),
                    label: "Deny".to_owned(),
                    choice: PermissionChoice::Deny,
                },
            ],
        }),
        None,
    ))
}

/// The same shell with a question waiting for typed answers.
#[component]
fn QuestionShell() -> impl IntoElement {
    session::session(host_with(
        None,
        Some(PendingQuestion {
            request_id: "req_q".to_owned(),
            title: "Which branch?".to_owned(),
            message: "Pick the branch the Agent should work on.".to_owned(),
            fields: vec![QuestionField {
                id: "branch".to_owned(),
                title: "Branch".to_owned(),
                description: None,
                required: true,
                kind: QuestionFieldKind::Text { default: None },
            }],
        }),
    ))
}

fn host_with(pending: Option<PendingPermission>, question: Option<PendingQuestion>) -> SessionHost {
    let sessions = signal(sessions());
    let active = signal(Some("s1".to_owned()));
    let timeline = signal(transcript("s1"));
    let draft = signal(String::new());
    let sending = signal(false);
    let error = signal(None::<SharedString>);
    let runtime = signal(Some(SharedString::from("desktop · reachable")));
    let pending = signal(pending);
    let question = signal(question);
    let resolving = signal(false);
    let commands = signal(Vec::new());

    let select = Rc::new(move |id: &str| {
        active.set(Some(id.to_owned()));
        timeline.set(transcript(id));
    });
    let send = Rc::new(move |text: &str| {
        let sequence = timeline.with(|timeline| timeline.len() + 1);
        timeline.update(|timeline| {
            timeline.push(entry(
                &format!("sent:{sequence}"),
                EntryKind::User,
                "",
                text,
            ))
        });
    });
    let on_permission = Rc::new(move |_: &str, _: PermissionChoice| {
        pending.set(None);
    });
    let on_question = Rc::new(move |_: &str, _| {
        question.set(None);
    });
    let on_command_query = Rc::new(move |_: &str| {
        commands.set(vec![CommandEntry {
            id: "review".to_owned(),
            label: "Review changes".to_owned(),
            description: "Ask the Agent to review the working tree".to_owned(),
            insertion: "/review ".to_owned(),
        }]);
    });
    let on_new_session = Rc::new(move || {
        sessions.update(|sessions| {
            sessions.insert(
                0,
                SessionSummary {
                    id: "s3".to_owned(),
                    title: "Fresh session".to_owned(),
                    agent: "Claude".to_owned(),
                    meta: "now".to_owned(),
                    state: SessionState::Idle,
                },
            );
        });
        active.set(Some("s3".to_owned()));
        timeline.set(transcript("s3"));
    });
    SessionHost {
        sessions,
        active,
        timeline,
        draft,
        sending,
        error,
        runtime,
        pending,
        question,
        commands,
        resolving,
        on_select: select,
        on_send: send,
        on_refresh: Rc::new(|| {}),
        on_disconnect: Rc::new(|| {}),
        on_permission,
        on_question,
        on_command_query,
        on_new_session,
    }
}

fn start() -> Headless {
    let mut config = EngineConfig::new(Root::of::<Shell>());
    // A tall phone: the transcript's rows all mount in the list's window.
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app
}

#[test]
fn the_shell_opens_on_the_first_session() {
    let app = start();
    assert!(app.screen().shows("Vibex") || app.screen().shows("Good morning"));
    assert!(app.screen().shows("Morning. The build is green."));
    assert!(app.screen().shows("cargo test"));
}

#[test]
fn the_drawer_lists_sessions_and_switching_one_swaps_the_transcript() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    app.press("Fix the tests").unwrap();
    app.wait_for("Please fix the tests").unwrap();
    app.wait_for_gone(
        "Morning. The build is green.",
        std::time::Duration::from_secs(5),
    )
    .unwrap();
}

#[test]
fn the_composer_appends_what_was_sent() {
    let mut app = start();
    app.type_text("text:Message the Agent…", "Ship it").unwrap();
    app.press("role:button[Send]").unwrap();
    app.wait_for("Ship it").unwrap();
    assert_eq!(
        app.screen().input_text("text:Message the Agent…").unwrap(),
        Some("")
    );
}

/// The app switches to the shell inside a `when` region; that branch creates
/// the session service, which makes signals and starts a task. This keeps that
/// pattern covered even though a real runtime is not reachable from a test.
#[component]
fn Branch() -> impl IntoElement {
    let connected = signal(false);
    view()
        .flex_1()
        .child(
            nsh_base::button()
                .on_press(move || connected.set(!connected.get_untracked()))
                .child(text("toggle")),
        )
        .child(when(
            move || connected.get(),
            || {
                let name = signal("Laptop");
                spawn(async move {
                    name.set("Laptop, reachable");
                });
                view().child(text(move || name.get()))
            },
        ))
}

#[test]
fn a_branch_may_build_a_service_and_start_its_task() {
    let mut app = Headless::start(EngineConfig::new(Root::of::<Branch>())).unwrap();
    app.wait_for("toggle").unwrap();
    app.press("toggle").unwrap();
    app.wait_for("Laptop, reachable").unwrap();
}

#[test]
fn a_permission_card_is_answered_and_leaves() {
    let mut config = EngineConfig::new(Root::of::<PermissionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Run cargo test?").unwrap();
    app.press("role:button[Allow]").unwrap();
    app.wait_for_gone("Run cargo test?", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn a_question_collects_an_answer_and_leaves() {
    let mut config = EngineConfig::new(Root::of::<QuestionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Which branch?").unwrap();
    app.type_text("text:Branch", "main").unwrap();
    app.press("role:button[Submit]").unwrap();
    app.wait_for_gone("Which branch?", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn the_slash_palette_inserts_a_command() {
    let mut app = start();
    app.type_text("text:Message the Agent…", "/").unwrap();
    app.wait_for("Review changes").unwrap();
    app.press("Review changes").unwrap();
    app.wait_for("text:/review ").unwrap();
}

#[test]
fn a_new_session_reuses_the_open_workspace_and_opens() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    app.press("role:button[New session]").unwrap();
    app.wait_for("Fresh session").unwrap();
}

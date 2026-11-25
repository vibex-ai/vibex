//! The session shell's flows, rendered headlessly through the real engine.
//!
//! The shell is mounted with a fixture host here: the sessions page opens, a
//! session opens its transcript, and the composer sends. The app's own wiring to the
//! remote backend is tested where that backend is available; these tests keep
//! the screen's behavior fixed as it grows.

use std::rc::Rc;

use nsh_base::{Terminal, TerminalApi};
use nsh_core::prelude::*;
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use vibex_nsh_ui::screens::connecting::ConnectionState;
use vibex_nsh_ui::screens::files::{FileEntry, FilePreview, FilePreviewKindView, FileSearchRow};
use vibex_nsh_ui::screens::git::{
    BranchRow, CommitDetailPreview, CommitRow, GitChangeRow, GitDiffPreview, WorktreeConflictRow,
    WorktreeOperationRow, WorktreeRow,
};
use vibex_nsh_ui::screens::new_session::{
    DirectoryEntry, ProjectBrowserHost, RuntimeChoice, WorkspaceChoice,
};
use vibex_nsh_ui::screens::session::{
    self, ActivityDetail, ActivityGlyph, ActivityView, AttachmentItem, CommandEntry,
    ComposerHistoryEntry, ConversationState, EntryKind, GoalPhaseView, GoalView, PendingPermission,
    PendingQuestion, PermissionChoice, PermissionOption, PlanStepState, PlanStepView, PlanView,
    QuestionField, QuestionFieldKind, QueuedMessageView, RuntimeChoiceView, RuntimeOptionView,
    RuntimeSummaryView, SessionHost, SessionState, SessionSummary, SidebarRowKind, SidebarRowView,
    SidebarSectionKind, SidebarSectionView, SidebarTree, TimelineEntry, TokenUsageView,
    TurnFileChange,
};
use vibex_nsh_ui::screens::team::TeamTaskView;
use vibex_nsh_ui::screens::terminal::TerminalChoice;

const COMPOSER: &str = "text:/ commands @files $skills";
/// The open session's own menu, on the conversation's header. Its name
/// carries the session's title after it.
const CONVERSATION_MENU: &str = "role:button[/^Conversation menu/]";
/// The control that starts a session, on the sessions page. The conversation's
/// header carries one of the same name, so the page says which.
const NEW_SESSION: &str = "id:sessions-page >> role:button[New session]";

/// Opens the find bar the way a reader does: from the open session's menu.
fn find_in_conversation(app: &mut Headless) {
    app.press(CONVERSATION_MENU).unwrap();
    app.press("role:button[Find in conversation]").unwrap();
    app.wait_for("text:Find in conversation").unwrap();
}

fn entry(id: &str, kind: EntryKind, title: &str, body: &str) -> TimelineEntry {
    TimelineEntry {
        id: id.to_owned(),
        kind,
        title: title.to_owned(),
        body: body.to_owned(),
        meta: String::new(),
        streaming: false,
        turn_id: id.to_owned(),
        expanded: false,
        expandable: false,
        attachments: Vec::new(),
        last_sequence: 0,
        started_at_ms: 0,
        ended_at_ms: 0,
        complete: true,
        ..TimelineEntry::default()
    }
}

fn sessions() -> Vec<SessionSummary> {
    vec![
        SessionSummary {
            id: "s1".to_owned(),
            title: "Good morning".to_owned(),
            agent: "Claude".to_owned(),
            meta: "2m".to_owned(),
            state: SessionState::Idle,
            terminal: false,
            project_id: "p1".to_owned(),
            project_name: "vibex".to_owned(),
            workspace_id: "w1".to_owned(),
            workspace_label: "main".to_owned(),
        },
        SessionSummary {
            id: "s2".to_owned(),
            title: "Fix the tests".to_owned(),
            agent: "Codex".to_owned(),
            meta: "1h".to_owned(),
            state: SessionState::Idle,
            terminal: false,
            project_id: "p1".to_owned(),
            project_name: "vibex".to_owned(),
            workspace_id: "w1".to_owned(),
            workspace_label: "main".to_owned(),
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
    on_back_press(session::handle_back);
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

/// A turn with a working fold and a picture in its user message.
#[component]
fn TurnShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    let mut header = entry("h1", EntryKind::ProcessHeader, "Worked for 2s", "");
    let now = nsh_core::time::unix_ms() as i64;
    header.expanded = true;
    header.expandable = true;
    header.turn_id = "t1".to_owned();
    header.started_at_ms = now - 2_000;
    header.ended_at_ms = now;
    let mut user = entry("u1", EntryKind::User, "", "Look at this");
    user.turn_id = "t1".to_owned();
    user.last_sequence = 24;
    user.attachments = vec![AttachmentItem {
        id: "a1".to_owned(),
        label: "photo.jpg".to_owned(),
        mime_type: "image/jpeg".to_owned(),
        uri: "data:image/jpeg;base64,".to_owned(),
    }];
    host.editable_sequence = signal(24);
    let preview = host.attachment_preview;
    host.on_open_attachment = Rc::new(move |item: &AttachmentItem| {
        preview.set(Some(item.clone()));
    });
    let mut tool = entry("t1", EntryKind::Tool, "cargo test", "Running");
    tool.expandable = true;
    let mut permission = entry(
        "p1",
        EntryKind::Permission,
        "Run cargo test?",
        "Command: cargo test",
    );
    permission.meta = "Allowed".to_owned();
    let mut changes = entry("c1", EntryKind::Changes, "1 file changed", "");
    changes.changed_files.push(TurnFileChange {
        path: "src/main.rs".to_owned(),
        row_id: tool.id.clone(),
        deleted: false,
    });
    // The answer is the turn's conclusion, so it is the row that carries the
    // actions; the process rows above it are the Agent's working notes.
    // A process row: the Agent thinking aloud between tool calls. It is prose
    // alone — no name above it, no actions under it.
    let mut process = entry("pr1", EntryKind::Agent, "Claude", "Checking the build first.");
    process.turn_id = "t1".to_owned();
    let mut answer = entry(
        "a1",
        EntryKind::Agent,
        "Claude",
        "![shot](https://example.com/shot.png)\nDone",
    );
    answer.conclusion = true;
    host.timeline = signal(vec![user, header, process, tool, permission, changes, answer]);
    session::session(host)
}

/// A shell with one authority folder.
#[component]
fn FolderShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sidebar_sections.set(vec![SidebarSectionView {
        kind: SidebarSectionKind::Folder,
        pinned: false,
        project_id: Some("p1".to_owned()),
        workspace_id: None,
        parent_id: None,
        depth: 0,
        collapsed: false,
        id: "folder-1".to_owned(),
        name: "Work".to_owned(),
        session_ids: vec!["s2".to_owned()],
        auto_archive_after_days: None,
        auto_continue: false,
    }]);
    session::session(host)
}

/// A shell whose first session is parked on the reader.
#[component]
fn WaitingShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sessions.update(|sessions| {
        if let Some(session) = sessions.first_mut() {
            session.state = SessionState::Waiting;
        }
    });
    session::session(host)
}

/// A title far wider than the drawer, for the ellipsis check.
const DRAWER_LONG_TITLE: &str =
    "A session title long enough that the drawer has to cut it with an ellipsis";

/// A shell whose first session has a title wider than the drawer.
#[component]
fn DrawerLongTitleShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sessions.update(|sessions| {
        if let Some(session) = sessions.first_mut() {
            session.title = DRAWER_LONG_TITLE.to_owned();
        }
    });
    session::session(host)
}

/// A shell with one session group.
#[component]
fn GroupShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sidebar_sections.set(vec![SidebarSectionView {
        kind: SidebarSectionKind::Group,
        pinned: false,
        project_id: Some("p1".to_owned()),
        workspace_id: None,
        parent_id: None,
        depth: 0,
        collapsed: false,
        id: "group-1".to_owned(),
        name: "Pair".to_owned(),
        session_ids: vec!["s1".to_owned(), "s2".to_owned()],
        auto_archive_after_days: None,
        auto_continue: false,
    }]);
    session::session(host)
}

/// A shell with a collapsed authority folder.
#[component]
fn CollapsedFolderShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sidebar_sections.set(vec![SidebarSectionView {
        kind: SidebarSectionKind::Folder,
        pinned: false,
        project_id: Some("p1".to_owned()),
        workspace_id: None,
        parent_id: None,
        depth: 0,
        collapsed: true,
        id: "folder-1".to_owned(),
        name: "Work".to_owned(),
        session_ids: vec!["s2".to_owned()],
        auto_archive_after_days: None,
        auto_continue: false,
    }]);
    session::session(host)
}

/// A shell with the active session pinned.
#[component]
fn PinnedShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.pinned.set(vec!["s1".to_owned()]);
    session::session(host)
}

/// A shell with its team sheet open.
#[component]
fn TeamShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.team_open.set(true);
    host.team_tasks.set(vec![
        TeamTaskView {
            id: "vibex://task/1".to_owned(),
            title: "Review the parser".to_owned(),
            phase: "Awaiting review".to_owned(),
            detail: "The child finished its changes.".to_owned(),
            can_accept: true,
            can_cancel: true,
            can_interrupt: true,
        },
        TeamTaskView {
            id: "vibex://task/2".to_owned(),
            title: "Port the tests".to_owned(),
            phase: "Active".to_owned(),
            detail: String::new(),
            can_accept: false,
            can_cancel: true,
            can_interrupt: true,
        },
    ]);
    session::session(host)
}

/// A shell with one discovered skill.
#[component]
fn SkillShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.skills = signal(vec![CommandEntry {
        id: "skill-1".to_owned(),
        label: "My skill".to_owned(),
        description: "A reusable skill".to_owned(),
        insertion: "$my-skill ".to_owned(),
    }]);
    session::session(host)
}

/// A shell with earlier prompts to recall.
#[component]
fn HistoryShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.history = signal(vec![ComposerHistoryEntry {
        id: "h1".to_owned(),
        text: "Ship it".to_owned(),
        sequence: 1,
    }]);
    host.history_open = signal(true);
    session::session(host)
}

/// A shell whose session has a runtime to change.
#[component]
fn RuntimeShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.runtime_summary = signal(Some(RuntimeSummaryView {
        agent: "Claude".to_owned(),
        model: "Claude 4".to_owned(),
        reasoning: "High".to_owned(),
        mode: "Default".to_owned(),
        options_set: false,
        reasoning_set: true,
        mode_set: false,
        available: true,
    }));
    host.runtime_options = signal(vec![
        RuntimeOptionView {
            id: "0".to_owned(),
            agent_id: "claude".to_owned(),
            agent: "Claude".to_owned(),
            model: "Claude 4".to_owned(),
            detail: "Pro".to_owned(),
            selected: true,
            available: true,
        },
        RuntimeOptionView {
            id: "1".to_owned(),
            agent_id: "codex".to_owned(),
            agent: "Codex".to_owned(),
            model: "GPT-5".to_owned(),
            detail: "Team".to_owned(),
            selected: false,
            available: true,
        },
    ]);
    host.runtime_reasoning = signal(vec![RuntimeChoiceView {
        value: "default".to_owned(),
        label: "Default".to_owned(),
        selected: false,
    }]);
    host.needs_continue = signal(true);
    host.token_usage = signal(Some(TokenUsageView {
        percent: Some(42),
        used: 42_000,
        size: 100_000,
        input: 10_000,
        output: 2_000,
    }));
    host.plan = signal(Some(PlanView {
        title: "Current plan".to_owned(),
        steps: vec![PlanStepView {
            title: "Inspect".to_owned(),
            state: PlanStepState::Running,
        }],
        completed: 0,
        current: 0,
    }));
    host.goal = signal(Some(GoalView {
        objective: "Ship P1".to_owned(),
        phase: GoalPhaseView::Active,
        progress: Some("2 rounds".to_owned()),
        can_pause: true,
        can_resume: false,
        can_clear: true,
    }));
    host.queued = signal(vec![QueuedMessageView {
        id: 1,
        text: "Hold this".to_owned(),
        attachment_count: 1,
    }]);
    let runtime_open = signal(false);
    let close = runtime_open;
    host.runtime_open = runtime_open;
    host.on_open_runtime = Rc::new(move || runtime_open.set(true));
    host.on_close_runtime = Rc::new(move || close.set(false));
    session::session(host)
}

/// A transcript that knows it has older history, and is long enough to scroll.
#[component]
fn OlderShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.has_older = signal(true);
    // Enough turns to fill the viewport several times: a list that fits cannot
    // be scrolled, and the older page is asked for by scrolling.
    let mut entries = Vec::new();
    for index in 0..24 {
        // Each is a turn's answer, the row the reader came for.
        let mut answer = entry(
            &format!("s1:pad:{index}"),
            EntryKind::Agent,
            "Claude",
            &format!("A paragraph of history, number {index}, long enough to take a row or two of the transcript."),
        );
        answer.conclusion = true;
        entries.push(answer);
    }
    host.timeline = signal(entries);
    // The app's own seam: asking for the older page puts the strip up while the
    // page is in flight.
    let loading = signal(false);
    host.loading_older = loading;
    host.on_load_older = Rc::new(move || loading.set(true));
    session::session(host)
}

/// The same shell while its runtime is unreachable and its transcript is on
/// screen: the conversation stays, with a reconnecting strip.
#[component]
fn OfflineTranscriptShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.connection = signal(ConnectionState::Offline);
    host.connection_error = signal(None);
    session::session(host)
}

/// The same shell while its runtime is unreachable.
#[component]
fn OfflineShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.connection = signal(ConnectionState::Offline);
    host.connection_error = signal(Some(SharedString::from(
        "The runtime could not be reached.",
    )));
    host.timeline = signal(Vec::new());
    session::session(host)
}

fn sidebar_fixture(
    sessions: Signal<Vec<SessionSummary>>,
    active: Signal<Option<String>>,
    pinned: Signal<Vec<String>>,
    sections: Signal<Vec<SidebarSectionView>>,
) -> SidebarTree {
    SidebarTree {
        rows: Rc::new(move |query| {
            let sessions = sessions.get();
            let sections = sections.get();
            let active = active.get();
            let pinned = pinned.get();
            let make_row = |session: &SessionSummary, indent: f32| SidebarRowView {
                id: session.id.clone(),
                title: session.title.clone(),
                detail: session.agent.clone(),
                metadata: session.meta.clone(),
                active: active.as_deref() == Some(&session.id),
                pinned: pinned.contains(&session.id),
                state: Some(session.state),
                indent,
                ..Default::default()
            };
            let query = query.to_lowercase();
            let matches = |session: &&SessionSummary| session.title.to_lowercase().contains(&query);
            let mut rows = vec![SidebarRowView {
                id: "p1".to_owned(),
                kind: SidebarRowKind::Project,
                title: "vibex".to_owned(),
                count: sessions.len(),
                ..Default::default()
            }];
            rows.extend(
                sessions
                    .iter()
                    .filter(matches)
                    .filter(|session| {
                        !sections
                            .iter()
                            .any(|section| section.session_ids.contains(&session.id))
                    })
                    .map(|session| make_row(session, 12.0)),
            );
            for section in sections {
                rows.push(SidebarRowView {
                    id: section.id.clone(),
                    kind: if section.kind == SidebarSectionKind::Group {
                        SidebarRowKind::Group
                    } else {
                        SidebarRowKind::Folder
                    },
                    title: section.name,
                    collapsed: section.collapsed,
                    indent: 12.0,
                    ..Default::default()
                });
                if !section.collapsed {
                    rows.extend(
                        sessions
                            .iter()
                            .filter(matches)
                            .filter(|session| section.session_ids.contains(&session.id))
                            // A folder's sessions hang one step in from it, as
                            // they do in the app: the step is what tells a
                            // folder's subtree from the rows around it.
                            .map(|session| {
                                make_row(
                                    session,
                                    12.0 + vibex_nsh_ui::theme::SIDEBAR_FOLDER_CHILD_INDENT,
                                )
                            }),
                    );
                }
            }
            rows
        }),
        detailed: signal(false),
        on_toggle_hierarchy: Rc::new(|| {}),
        on_toggle_row: Rc::new(move |row| {
            sections.update(|sections| {
                if let Some(section) = sections.iter_mut().find(|section| section.id == row.id) {
                    section.collapsed = !section.collapsed;
                }
            })
        }),
        on_collapse_all: Rc::new(|| {}),
        on_reveal_active: Rc::new(|| {}),
        on_move_row: Rc::new(|_, _| {}),
        on_move_row_step: Rc::new(|_, _| {}),
        row_move_targets: Rc::new(|_| (false, false)),
        on_new_session: Rc::new(|_| {}),
        on_new_folder: Rc::new(|_| {}),
        batch_scope: Rc::new(move |row| {
            if row.kind == SidebarRowKind::Project {
                sessions
                    .with(|sessions| sessions.iter().map(|session| session.id.clone()).collect())
            } else {
                Vec::new()
            }
        }),
    }
}

fn host_with(pending: Option<PendingPermission>, question: Option<PendingQuestion>) -> SessionHost {
    let sessions = signal(sessions());
    let active = signal(Some("s1".to_owned()));
    let pinned = signal(Vec::new());
    let auto_continue_sessions = signal(Vec::new());
    let sidebar_sections = signal(Vec::<SidebarSectionView>::new());
    let folder_open = signal(false);
    let folder_name = signal(String::new());
    let move_open = signal(None::<String>);
    let timeline = signal(transcript("s1"));
    let draft = signal(String::new());
    let sending = signal(false);
    let error = signal(None::<SharedString>);
    let connection = signal(ConnectionState::Online);
    let connection_error = signal(None::<SharedString>);
    let has_older = signal(false);
    let loading_older = signal(false);
    let runtime_summary = signal(None);
    let runtime_options = signal(Vec::new());
    let runtime_reasoning = signal(Vec::new());
    let runtime_modes = signal(Vec::new());
    let runtime_features = signal(Vec::new());
    let git_amend = signal(false);
    let git_push_after = signal(false);
    let git_remote_busy = signal(false);
    let git_branches = signal(vec![BranchRow {
        name: "main".to_owned(),
        current: true,
        detail: "origin/main".to_owned(),
        ahead: 2,
        behind: 1,
    }]);
    let git_branch_menu = signal(false);
    let worktrees = signal(vec![WorktreeRow {
        id: "wt1".to_owned(),
        branch: "feature/tree".to_owned(),
        path: "/tmp/vibex-tree".to_owned(),
        status: "Active".to_owned(),
        can_archive: true,
        can_restore: false,
        can_merge: true,
        can_discard: false,
    }]);
    let worktree_menu = signal(false);
    let worktree_busy = signal(false);
    let worktree_error = signal(None::<SharedString>);
    let worktree_operations = signal(vec![WorktreeOperationRow {
        id: "op1".to_owned(),
        label: "feature/tree".to_owned(),
        status: "Needs resolution".to_owned(),
        can_continue: true,
        can_abort: true,
        conflicts: vec![WorktreeConflictRow {
            path: "src/lib.rs".to_owned(),
        }],
    }]);
    let runtime_open = signal(false);
    let runtime_busy = signal(false);
    let token_usage = signal(None);
    let needs_continue = signal(false);
    let team_open = signal(false);
    let team_tasks = signal(Vec::new());
    let team_busy = signal(false);
    let team_error = signal(None::<SharedString>);
    let attachment_preview = signal(None::<AttachmentItem>);
    let show_token_usage = signal(true);
    let show_plan_goal = signal(true);
    let show_generation_status = signal(true);
    let find_open = signal(false);
    let show_reasoning = signal(true);
    let auto_continue = signal(false);
    let plan = signal(None);
    let goal = signal(None);
    let history = signal(Vec::new());
    let history_open = signal(false);
    let queued = signal(Vec::new());
    let queue_paused = signal(false);
    let queue_manual = signal(false);
    let editable_sequence = signal(0);
    let schedule_open = signal(false);
    let scheduled = signal(false);
    let editing = signal(false);
    let runtime = signal(Some(SharedString::from("desktop · reachable")));
    let pending = signal(pending);
    let question = signal(question);
    let resolving = signal(false);
    let commands = signal(Vec::new());
    let mentions = signal(Vec::new());
    let skills = signal(Vec::new());
    let new_title = signal(String::new());
    let new_prompt = signal(String::new());
    let new_attachments = signal(Vec::new());
    let attachments = signal(Vec::new());
    let on_attach_image = Rc::new(move || {
        attachments.update(|items| {
            items.push(AttachmentItem {
                id: "attachment-1".to_owned(),
                label: "photo.jpg".to_owned(),
                mime_type: "image/jpeg".to_owned(),
                uri: "data:image/jpeg;base64,".to_owned(),
            });
        });
    });
    let on_remove_attachment = Rc::new(move |id: &str| {
        attachments.update(|items| items.retain(|item| item.id != id));
    });
    let terminal_open = signal(false);
    let terminal_title = signal(Some(SharedString::from("Terminal")));
    let terminal_error = signal(None::<SharedString>);
    let terminal_ref = component_ref::<Terminal>();
    let terminal_choices = signal(vec![
        TerminalChoice {
            id: "t1".to_owned(),
            title: "Terminal".to_owned(),
            status: "running".to_owned(),
        },
        TerminalChoice {
            id: "t2".to_owned(),
            title: "Build".to_owned(),
            status: "running".to_owned(),
        },
    ]);
    let terminal_id = signal(Some("t1".to_owned()));
    let on_select_terminal = Rc::new(move |id: &str| {
        if let Some(choice) = terminal_choices.with(|choices| {
            choices
                .iter()
                .find(|choice| choice.id == id)
                .map(|choice| choice.title.clone())
        }) {
            terminal_title.set(Some(choice.into()));
            terminal_id.set(Some(id.to_owned()));
        }
    });
    let files_open = signal(false);
    let files_entries = signal(vec![
        FileEntry {
            path: "src".to_owned(),
            name: "src".to_owned(),
            directory: true,
            depth: 1,
            expanded: false,
            loading: false,
            git: None,
            icon: Default::default(),
            ignored: false,
        },
        FileEntry {
            path: "README.md".to_owned(),
            name: "README.md".to_owned(),
            directory: false,
            depth: 1,
            expanded: false,
            loading: false,
            git: None,
            icon: Default::default(),
            ignored: false,
        },
    ]);
    let files_preview = signal(None::<FilePreview>);
    let files_path = signal(None::<String>);
    let files_search = signal(vec![FileSearchRow {
        path: "src/main.rs".to_owned(),
        name: "main.rs".to_owned(),
        line: Some(3),
        snippet: "hello".to_owned(),
    }]);
    let files_error = signal(None::<SharedString>);
    let on_open_dir = Rc::new(move |path: &str| {
        files_entries.update(|rows| {
            let Some(index) = rows.iter().position(|row| row.path == path) else {
                return;
            };
            rows[index].expanded = !rows[index].expanded;
            if rows[index].expanded {
                rows.insert(
                    index + 1,
                    FileEntry {
                        path: "src/main.rs".to_owned(),
                        name: "main.rs".to_owned(),
                        directory: false,
                        depth: 2,
                        expanded: false,
                        loading: false,
                        git: None,
                        icon: Default::default(),
                        ignored: false,
                    },
                );
            } else {
                rows.remove(index + 1);
            }
        });
    });
    let on_open_file = Rc::new(move |path: &str, line| {
        let markdown = path.ends_with(".md");
        files_path.set(Some(path.to_owned()));
        files_preview.set(Some(FilePreview {
            title: path.rsplit('/').next().unwrap_or(path).to_owned(),
            body: if markdown {
                "# Hello".to_owned()
            } else {
                "fn main() {\n    let greeting = \"hello\";\n    println!(\"{greeting}\");\n}"
                    .to_owned()
            },
            truncated: false,
            language: if markdown { "markdown" } else { "rust" }.to_owned(),
            kind: if markdown {
                FilePreviewKindView::Markdown
            } else {
                FilePreviewKindView::Text
            },
            image_uri: None,
            line,
        }));
    });
    let on_files_back = Rc::new(move || {
        files_preview.set(None);
        files_path.set(None);
    });
    let git_open = signal(false);
    let git_branch = signal(Some(SharedString::from("main")));
    let git_changes = signal(vec![GitChangeRow {
        path: "src/main.rs".to_owned(),
        name: "main.rs".to_owned(),
        directory: false,
        depth: 0,
        expanded: false,
        segments: Vec::new(),
        selection: Default::default(),
        status: "M".to_owned(),
        staged: false,
        unstaged: true,
        additions: 3,
        deletions: 1,
    }]);
    let git_diff = signal(None::<GitDiffPreview>);
    let git_error = signal(None::<SharedString>);
    let on_open_git_diff = Rc::new(move |path: &str, _staged: bool| {
        git_diff.set(Some(GitDiffPreview {
            title: path.to_owned(),
            body: "diff --git a/src/main.rs b/src/main.rs".to_owned(),
            truncated: false,
            path: path.to_owned(),
            staged: false,
        }));
    });
    let on_git_back = Rc::new(move || git_diff.set(None));
    let git_selected = signal(vec!["src/main.rs".to_owned()]);
    let git_message = signal(String::new());
    let git_committing = signal(false);
    let git_history = signal(vec![CommitRow {
        hash: "abc123".to_owned(),
        short_hash: "abc123".to_owned(),
        subject: "Initial commit".to_owned(),
        detail: "Ada".to_owned(),
    }]);
    let git_show_history = signal(false);
    let git_commit_detail = signal(None::<CommitDetailPreview>);
    let on_open_git_commit = Rc::new(move |hash: &str| {
        git_commit_detail.set(Some(CommitDetailPreview {
            title: hash.to_owned(),
            body: "commit detail body".to_owned(),
            truncated: false,
        }));
    });
    let on_revert_git = Rc::new(move |_path: &str| git_diff.set(None));
    let on_commit_git = Rc::new(move || {
        if !git_message.get_untracked().trim().is_empty() {
            git_message.set(String::new());
        }
    });
    let on_toggle_git_history = Rc::new(move || {
        git_show_history.set(!git_show_history.get_untracked());
    });
    let on_stage = Rc::new(move |path: &str| {
        git_changes.update(|changes| {
            if let Some(change) = changes.iter_mut().find(|change| change.path == path) {
                change.staged = true;
            }
        });
    });
    let on_unstage = Rc::new(move |path: &str| {
        git_changes.update(|changes| {
            if let Some(change) = changes.iter_mut().find(|change| change.path == path) {
                change.staged = false;
            }
        });
    });
    let session_actions = signal(None::<String>);
    let session_rename = signal(String::new());
    let session_busy = signal(false);
    let on_session_menu = Rc::new(move |id: &str, title: &str| {
        session_rename.set(title.to_owned());
        session_actions.set(Some(id.to_owned()));
    });
    let on_rename_session = Rc::new(move || {
        let Some(id) = session_actions.get_untracked() else {
            return;
        };
        let title = session_rename.get_untracked();
        sessions.update(|sessions| {
            if let Some(session) = sessions.iter_mut().find(|session| session.id == id) {
                session.title = title.clone();
            }
        });
        session_actions.set(None);
    });
    let on_archive_session = Rc::new(move || {
        let Some(id) = session_actions.get_untracked() else {
            return;
        };
        sessions.update(|sessions| sessions.retain(|session| session.id != id));
        session_actions.set(None);
    });
    let on_delete_session = Rc::new(move || {
        let Some(id) = session_actions.get_untracked() else {
            return;
        };
        sessions.update(|sessions| sessions.retain(|session| session.id != id));
        session_actions.set(None);
    });
    let new_session_open = signal(false);
    let new_workspaces = signal(vec![WorkspaceChoice {
        id: "w1".to_owned(),
        name: "Vibex".to_owned(),
        detail: "/code/vibex".to_owned(),
    }]);
    let new_runtimes = signal(vec![RuntimeChoice {
        id: "0".to_owned(),
        title: "Claude · Sonnet".to_owned(),
        detail: "Subscription".to_owned(),
    }]);
    let new_workspace = signal(Some("w1".to_owned()));
    let new_runtime = signal(Some("0".to_owned()));
    let new_busy = signal(false);
    let new_error = signal(None::<SharedString>);
    let new_worktree_branch = signal(String::new());
    let new_base_ref = signal(String::new());
    let project_dirs = signal(vec![DirectoryEntry {
        name: "demo".to_owned(),
        path: "/workspace/demo".to_owned(),
    }]);
    let project_browser_open = signal(false);
    let project_dir_path = signal(Some("/workspace".to_owned()));
    let project_dir_parent = signal(None);
    // Two roots, as a runtime configured with more than its home offers: the
    // sheet lists them so the reader can jump between them.
    let project_roots = signal(vec!["/workspace".to_owned(), "/srv/projects".to_owned()]);
    let on_create_worktree_session = Rc::new(move || {
        if new_worktree_branch.get_untracked().trim().is_empty() {
            return;
        }
        sessions.update(|sessions| {
            sessions.insert(
                0,
                SessionSummary {
                    id: "s4".to_owned(),
                    title: "Worktree session".to_owned(),
                    agent: "Claude".to_owned(),
                    meta: "now".to_owned(),
                    state: SessionState::Idle,
                    terminal: false,
                    project_id: "p1".to_owned(),
                    project_name: "vibex".to_owned(),
                    workspace_id: "w1".to_owned(),
                    workspace_label: "main".to_owned(),
                },
            );
        });
        active.set(Some("s4".to_owned()));
        timeline.set(transcript("s4"));
        new_session_open.set(false);
    });
    let on_create_session = Rc::new(move || {
        sessions.update(|sessions| {
            sessions.insert(
                0,
                SessionSummary {
                    id: "s3".to_owned(),
                    title: "Created session".to_owned(),
                    agent: "Claude".to_owned(),
                    meta: "now".to_owned(),
                    state: SessionState::Idle,
                    terminal: false,
                    project_id: "p1".to_owned(),
                    project_name: "vibex".to_owned(),
                    workspace_id: "w1".to_owned(),
                    workspace_label: "main".to_owned(),
                },
            );
        });
        active.set(Some("s3".to_owned()));
        timeline.set(transcript("s3"));
        new_session_open.set(false);
    });

    let select = Rc::new(move |id: &str| {
        active.set(Some(id.to_owned()));
        timeline.set(transcript(id));
    });
    let send = Rc::new(move |text: &str| {
        draft.set(String::new());
        let sequence = timeline.with(|timeline| timeline.len() + 1);
        let body = if text.is_empty() {
            "sent an attachment".to_owned()
        } else {
            text.to_owned()
        };
        timeline.update(|timeline| {
            timeline.push(entry(
                &format!("sent:{sequence}"),
                EntryKind::User,
                "",
                &body,
            ))
        });
    });
    let on_permission = Rc::new(move |_: &str, _: PermissionChoice| {
        pending.set(None);
    });
    let on_question = Rc::new(move |_: &str, _| {
        question.set(None);
    });
    let on_mention_query = Rc::new(move |_: &str| {
        mentions.set(vec![CommandEntry {
            id: "file:src/main.rs".to_owned(),
            label: "src/main.rs".to_owned(),
            description: "Workspace file".to_owned(),
            insertion: "@src/main.rs ".to_owned(),
        }]);
    });
    let on_command_query = Rc::new(move |_: &str| {
        commands.set(vec![CommandEntry {
            id: "review".to_owned(),
            label: "Review changes".to_owned(),
            description: "Ask the Agent to review the working tree".to_owned(),
            insertion: "/review ".to_owned(),
        }]);
    });
    let on_open_terminal = Rc::new(move || terminal_open.set(true));
    let on_new_session = Rc::new(move || new_session_open.set(true));
    let on_interrupt = Rc::new(move || {
        sessions.update(|sessions| {
            if let Some(session) = sessions.first_mut() {
                session.state = SessionState::Idle;
            }
        });
    });
    SessionHost {
        sessions,
        active,
        pinned,
        auto_continue_sessions,
        sidebar: sidebar_fixture(sessions, active, pinned, sidebar_sections),
        on_set_session_auto_continue: Rc::new(|_, _| {}),
        sidebar_sections,
        folder_open,
        folder_name,
        on_open_folder: Rc::new(move || folder_open.set(true)),
        on_open_subfolder: Rc::new(|_| {}),
        on_close_folder: Rc::new(move || folder_open.set(false)),
        on_create_folder: Rc::new(|| {}),
        on_rename_folder: Rc::new(|_, _| {}),
        on_delete_folder: Rc::new(|_| {}),
        on_rename_group: Rc::new(|_, _| {}),
        on_delete_group: Rc::new(|_| {}),
        on_set_group_members: Rc::new(|_, _| {}),
        on_set_group_pinned: Rc::new(|_, _| {}),
        on_set_group_auto_continue: Rc::new(|_, _| {}),
        on_create_group: Rc::new(|_, _| {}),
        move_open,
        on_open_move: Rc::new(move || {
            if let Some(id) = session_actions.get_untracked() {
                move_open.set(Some(id));
            }
        }),
        on_close_move: Rc::new(move || move_open.set(None)),
        on_move_session: Rc::new(|_, _| {}),
        on_toggle_pin: Rc::new(|_| {}),
        timeline,
        conversation: signal(ConversationState::Ready),
        draft,
        sending,
        error,
        connection,
        connection_error,
        has_older,
        loading_older,
        runtime_summary,
        runtime_options,
        runtime_reasoning,
        runtime_modes,
        runtime_features,
        runtime_open,
        runtime_busy,
        token_usage,
        needs_continue,
        on_continue_turn: Rc::new(|| {}),
        team_open,
        team_tasks,
        team_busy,
        team_error,
        on_open_team: Rc::new(|| {}),
        on_close_team: Rc::new(|| {}),
        on_refresh_team: Rc::new(|| {}),
        on_accept_team: Rc::new(|_| {}),
        on_cancel_team: Rc::new(|_| {}),
        on_interrupt_team: Rc::new(|_| {}),
        attachment_preview,
        on_open_attachment: Rc::new(|_| {}),
        on_close_attachment: Rc::new(|| {}),
        show_token_usage,
        show_plan_goal,
        show_generation_status,
        find_open,
        show_reasoning,
        auto_continue,
        on_toggle_auto_continue: Rc::new(|| {}),
        on_toggle_find: Rc::new(move || find_open.update(|open| *open = !*open)),
        plan,
        goal,
        history,
        history_open,
        queued,
        queue_paused,
        queue_manual,
        on_toggle_queue_manual: Rc::new(|| {}),
        editable_sequence,
        editing,
        on_edit_message: Rc::new(|_, _| {}),
        on_cancel_edit: Rc::new(|| {}),
        on_replace_message: Rc::new(|_| {}),
        on_send_queued: Rc::new(|_| {}),
        on_remove_queued: Rc::new(|_| {}),
        on_move_queued: Rc::new(|_, _| {}),
        schedule_open,
        scheduled,
        on_open_schedule: Rc::new(move || schedule_open.set(true)),
        on_close_schedule: Rc::new(move || schedule_open.set(false)),
        on_schedule: Rc::new(move |_| {
            schedule_open.set(false);
            scheduled.set(true);
        }),
        on_cancel_schedule: Rc::new(move || scheduled.set(false)),
        on_clear_queue: Rc::new(|| {}),
        on_toggle_queue_paused: Rc::new(|| {}),
        on_open_history: Rc::new(|| {}),
        on_close_history: Rc::new(|| {}),
        on_recall_history: Rc::new(|_| {}),
        on_goal_control: Rc::new(|_| {}),
        on_open_runtime: Rc::new(|| {}),
        on_close_runtime: Rc::new(|| {}),
        on_choose_runtime: Rc::new(|_| {}),
        on_choose_reasoning: Rc::new(|_| {}),
        on_choose_mode: Rc::new(|_| {}),
        on_choose_feature: Rc::new(|_, _| {}),
        on_retry: Rc::new(|| {}),
        runtime,
        pending,
        question,
        commands,
        mentions,
        skills,
        on_skill_query: Rc::new(|_| {}),
        on_arm_entry: Rc::new(|_| {}),
        attachments,
        on_attach_image,
        on_attach_camera: Rc::new(|| {}),
        on_attach_file: Rc::new(|| {}),
        on_remove_attachment,
        resolving,
        on_select: select,
        on_send: send,
        on_refresh: Rc::new(|| {}),
        on_disconnect: Rc::new(|| {}),
        on_permission,
        on_question,
        on_command_query,
        on_mention_query,
        on_new_session,
        on_new_project: Rc::new(move || project_browser_open.set(true)),
        on_open_settings: Rc::new(|| {}),
        on_toggle_turn: Rc::new(|_| {}),
        on_reveal_row: Rc::new(|_, _| {}),
        on_toggle_detail: Rc::new(|_, _| {}),
        on_fork: Rc::new(|_| {}),
        on_load_older: Rc::new(|| {}),
        terminal_open,
        terminal_visible: signal(false),
        terminal_title,
        terminal_error,
        terminal_ref,
        on_open_terminal,
        on_terminal_data: Rc::new(|_| {}),
        on_terminal_resize: Rc::new(|_, _| {}),
        on_terminal_title: Rc::new(|_| {}),
        on_interrupt,
        on_switch_runtime: Rc::new(|| {}),
        terminal_choices,
        terminal_id,
        terminal_busy: signal(false),
        terminal_output_revision: signal(0),
        on_retry_terminal: Rc::new(|| {}),
        on_kill_terminal: Rc::new(move |id| {
            terminal_choices.update(|choices| choices.retain(|choice| choice.id != id));
        }),
        on_select_terminal,
        on_new_terminal: Rc::new(|| {}),
        files_open,
        files_entries,
        files_search,
        files_searching: signal(false),
        files_path,
        on_search_files: Rc::new(|_, _| {}),
        files_preview,
        files_error,
        on_open_files: Rc::new(move || files_open.set(true)),
        on_refresh_files: Rc::new(|| {}),
        on_collapse_files: Rc::new(|| {}),
        on_open_dir,
        on_open_file,
        on_files_back,
        git_open,
        git_branch,
        git_totals: signal((3, 1)),
        git_changes,
        git_diff,
        git_error,
        on_open_git: Rc::new(move || git_open.set(true)),
        on_open_git_diff,
        on_stage_git: on_stage,
        on_unstage_git: on_unstage,
        git_selected,
        on_toggle_git_selection: Rc::new(move |_| {
            git_selected.update(|paths| {
                if paths.is_empty() {
                    paths.push("src/main.rs".to_owned());
                } else {
                    paths.clear();
                }
            });
        }),
        on_toggle_git_directory: Rc::new(|_| {}),
        on_toggle_git_directories: Rc::new(|| {}),
        on_revert_git_selection: Rc::new(|_| {}),
        git_message,
        git_committing,
        git_amend,
        git_push_after,
        git_remote_busy,
        git_branches,
        git_branch_menu,
        on_open_git_branches: Rc::new(move || git_branch_menu.set(true)),
        on_close_git_branches: Rc::new(move || git_branch_menu.set(false)),
        on_select_git_branch: Rc::new(|_| {}),
        worktrees,
        worktree_menu,
        worktree_busy,
        worktree_operations,
        worktree_error,
        on_open_worktrees: Rc::new(move || worktree_menu.set(true)),
        on_close_worktrees: Rc::new(move || worktree_menu.set(false)),
        on_archive_worktree: Rc::new(|_| {}),
        on_merge_worktree: Rc::new(|_| {}),
        on_discard_worktree: Rc::new(|_| {}),
        on_continue_worktree: Rc::new(|_| {}),
        on_abort_worktree: Rc::new(|_| {}),
        on_resolve_worktree: Rc::new(|_, _, _| {}),
        on_restore_worktree: Rc::new(|_| {}),
        on_toggle_git_amend: Rc::new(|| {}),
        on_toggle_git_push_after: Rc::new(|| {}),
        on_git_push: Rc::new(|| {}),
        on_git_pull: Rc::new(|| {}),
        on_git_fetch: Rc::new(|| {}),
        git_history,
        git_show_history,
        git_commit_detail,
        on_commit_git,
        on_toggle_git_history,
        on_open_git_commit,
        on_revert_git,
        on_git_back,
        on_refresh_git: Rc::new(|| {}),
        session_actions,
        session_rename,
        session_busy,
        auto_continue_paused: signal(Vec::new()),
        scheduled_sends: signal(Vec::new()),
        on_resume_auto_continue: Rc::new(|_| {}),
        on_cancel_scheduled_send: Rc::new(|_, _| {}),
        on_reschedule_scheduled_send: Rc::new(|_, _, _| {}),
        on_set_folder_auto_archive: Rc::new(|_, _| {}),
        on_toggle_project_new_session_location: Rc::new(|_| {}),
        on_remove_project: Rc::new(|_| {}),
        on_session_menu,
        on_close_session_actions: Rc::new(|| {}),
        on_rename_session,
        on_archive_session,
        on_pin_sessions: Rc::new(|_| {}),
        on_fork_sessions: Rc::new(|_| {}),
        on_archive_sessions: Rc::new(|_| {}),
        on_delete_sessions: Rc::new(|_| {}),
        on_delete_session,
        new_session_open,
        new_workspaces,
        new_runtimes,
        new_workspace,
        new_runtime,
        new_busy,
        new_error,
        on_create_session,
        on_create_temporary_session: Rc::new(|| {}),
        project_browser: ProjectBrowserHost {
            open: project_browser_open,
            dirs: project_dirs,
            current: project_dir_path,
            parent: project_dir_parent,
            roots: project_roots,
            busy: signal(false),
            error: signal(None),
            on_browse: Rc::new(move |path| project_dir_path.set(path.map(str::to_owned))),
            on_open: Rc::new(move |path| {
                assert_eq!(path, "/workspace/demo");
                project_browser_open.set(false);
            }),
        },
        new_worktree_branch,
        new_base_ref,
        new_title,
        new_prompt,
        new_attachments,
        on_attach_new_image: Rc::new(|| {}),
        on_attach_new_file: Rc::new(|| {}),
        on_remove_new_attachment: Rc::new(|_| {}),
        on_create_worktree_session,
        on_close_new_session: Rc::new(move || new_session_open.set(false)),
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
fn an_unreachable_runtime_shows_the_connecting_pages_ways_back() {
    let mut config = EngineConfig::new(Root::of::<OfflineShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Desktop is unavailable").unwrap();
    assert!(app.screen().shows("The runtime could not be reached."));
    for action in ["Retry", "Switch runtime", "Disconnect"] {
        assert!(
            app.screen().shows(action),
            "`{action}` is offered on the unavailable page"
        );
    }
}

#[test]
fn a_dropped_connection_keeps_the_transcript_and_offers_retry() {
    let mut config = EngineConfig::new(Root::of::<OfflineTranscriptShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Desktop is unavailable").unwrap();
    assert!(app.screen().shows("Morning. The build is green."));
    assert!(app.screen().shows("Retry"));
}

#[test]
fn the_composer_opens_the_runtime_choices() {
    let mut config = EngineConfig::new(Root::of::<RuntimeShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Claude 4").unwrap();
    app.wait_for("role:button[Token usage]").unwrap();
    assert!(app.screen().shows("Ship P1"));
    assert!(app.screen().shows("Continue"));
    assert!(app.screen().shows("Current plan"));
    assert!(app.screen().shows("Pause"));
    assert!(app.screen().shows("Manual"));
    assert!(app.screen().shows("Queued · 1"));
    assert!(app.screen().shows("Hold this (+1)"));
    for step in ["Move up", "Move down", "Send now", "Remove"] {
        assert!(
            app.screen()
                .matches(&format!("role:button[{step}]"))
                .unwrap(),
            "a queued prompt offers {step}"
        );
    }
    app.press("Claude 4").unwrap();
    app.wait_for("Agent settings").unwrap();
    assert!(!app.screen().shows("GPT-5"));
    app.press("role:button[Codex]").unwrap();
    app.wait_for("GPT-5").unwrap();
    app.press("role:button[Close]").unwrap();
    app.wait_for_gone("GPT-5", std::time::Duration::from_secs(3))
        .unwrap();
    // The Agent's own options open the same sheet on their panel.
    app.press("role:button[Agent options]").unwrap();
    app.wait_for("role:button[Reasoning]").unwrap();
    app.wait_for("Default").unwrap();
    app.press("Default").unwrap();
    app.wait_for_gone("Default", std::time::Duration::from_secs(3))
        .unwrap();
}

#[test]
fn the_attachment_sheet_explains_the_three_sources_and_keeps_the_draft() {
    let mut app = start();
    app.type_text(COMPOSER, "Keep this draft").unwrap();
    app.press("role:button[Add to message]").unwrap();
    app.wait_for("role:button[Photo]").unwrap();
    assert!(app.screen().shows("Scaled down before it is sent"));
    app.wait_for("role:button[Camera]").unwrap();
    assert!(app.screen().shows("Sent as it is, up to 8 MB"));
    app.press("role:button[Close]").unwrap();
    assert_eq!(
        app.screen().input_text("text:Keep this draft").unwrap(),
        Some("Keep this draft")
    );
}

#[test]
fn the_team_sheet_lists_tasks_and_their_actions() {
    let mut config = EngineConfig::new(Root::of::<TeamShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Review the parser").unwrap();
    assert!(app.screen().shows("Awaiting review"));
    assert!(app.screen().shows("Accept"));
    assert!(app.screen().shows("Interrupt"));
    assert!(app.screen().shows("Port the tests"));
}

#[test]
fn the_dollar_palette_inserts_a_skill() {
    let mut config = EngineConfig::new(Root::of::<SkillShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.type_text(COMPOSER, "$").unwrap();
    app.wait_for("My skill").unwrap();
    app.press("My skill").unwrap();
    app.wait_for("$my-skill").unwrap();
}

#[test]
fn the_history_sheet_lists_earlier_prompts() {
    let mut config = EngineConfig::new(Root::of::<HistoryShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Earlier messages").unwrap();
    assert!(app.screen().shows("Ship it"));
}

#[test]
fn older_history_loads_itself_as_the_reader_scrolls_to_the_top() {
    let mut config = EngineConfig::new(Root::of::<OlderShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    // No button to press: the older messages come when the reader reaches for
    // them, so the strip is not in the way of the transcript.
    assert!(
        !app.screen().shows("Load earlier messages"),
        "the older messages are not behind a button"
    );
    // Scrolling up is what asks for them.
    app.scroll("id:transcript", px(-2000.0)).unwrap();
    app.wait_for("Loading earlier...").unwrap();
}

#[test]
fn a_turn_shows_its_fold_and_the_attachments_it_carried() {
    let mut config = EngineConfig::new(Root::of::<TurnShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Worked for 2s").unwrap();
    assert!(app.screen().shows("cargo test"));
    app.wait_for("role:button[cargo test]").unwrap();
    assert!(app.screen().shows("1 file changed"));
    assert!(app.screen().shows("Run cargo test?"));
    assert!(app.screen().shows("Allowed"));
    assert!(app.screen().shows("photo.jpg"));
    for action in ["Edit", "Copy", "Share", "Fork"] {
        assert!(
            app.screen()
                .matches(&format!("role:button[{action}]"))
                .unwrap()
        );
    }
    app.press("photo.jpg").unwrap();
    app.wait_for("Attachment").unwrap();
}

#[test]
fn a_changed_file_in_the_timeline_opens_the_workbench_reader() {
    let mut config = EngineConfig::new(Root::of::<TurnShell>());
    config.viewport = nsh_core::geometry::Size::new(px(320.0), px(900.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[src/main.rs]").unwrap();
    app.press("role:button[src/main.rs]").unwrap();
    app.wait_for("role:button[Files]").unwrap();
    app.wait_for("role:button[Find in file]").unwrap();
    app.wait_for("main.rs").unwrap();
}

#[component]
fn ActivityTimelineShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    let mut fold = entry(
        "turn-header:example",
        EntryKind::ProcessHeader,
        "Worked for 2s",
        "",
    );
    fold.expandable = true;
    fold.started_at_ms = 1_000;
    fold.ended_at_ms = 3_000;
    let mut group = entry(
        "activity-group:one",
        EntryKind::ActivityGroup,
        "Read 2 files",
        "",
    );
    group.expandable = true;
    group.parent_id = Some(fold.id.clone());
    let reads = ["src/model.rs", "src/sidebar.rs"]
        .into_iter()
        .map(|path| {
            let mut row = entry(path, EntryKind::Tool, "Read", "");
            row.parent_id = Some(group.id.clone());
            row.expandable = true;
            row.activity = Some(ActivityView {
                glyph: ActivityGlyph::Read,
                target: path.split('/').next_back().unwrap().to_owned(),
                path: Some(path.to_owned()),
                failed: false,
                error: None,
                duration: None,
                details_pending: false,
                details_loading: false,
                details: vec![ActivityDetail {
                    label: "Output".to_owned(),
                    text: format!("Contents of {path}\nFetched from the worktree"),
                    copy: format!("Contents of {path}\nFetched from the worktree"),
                }],
            });
            row
        })
        .collect::<Vec<_>>();
    let timeline = signal(vec![
        fold,
        group,
        reads[0].clone(),
        reads[1].clone(),
        entry("answer", EntryKind::Agent, "Claude", "Here is the answer."),
    ]);
    host.timeline = timeline;
    host.on_reveal_row = Rc::new(move |id, detail| {
        if id.starts_with("src/") {
            timeline.update(|rows| {
                rows[0].expanded = true;
                rows[1].expanded = true;
                if detail && let Some(row) = rows.iter_mut().find(|row| row.id == id) {
                    row.expanded = true;
                }
            });
        }
    });
    let toggle = move |id: &str| {
        timeline.update(|rows| {
            if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
                row.expanded = !row.expanded;
            }
        });
    };
    host.on_toggle_turn = Rc::new(toggle);
    host.on_toggle_detail = Rc::new(move |id, _| toggle(id));
    session::session(host)
}

#[test]
fn searching_reveals_a_collapsed_activity_without_expanding_its_siblings() {
    let mut config = EngineConfig::new(Root::of::<ActivityTimelineShell>());
    config.viewport = nsh_core::geometry::Size::new(px(320.0), px(860.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Here is the answer.").unwrap();
    assert!(!app.screen().shows("Read 2 files"));
    find_in_conversation(&mut app);
    app.type_text("text:Find in conversation", "src/model.rs")
        .unwrap();
    app.wait_for("1/1").unwrap();
    app.wait_for("Read 2 files").unwrap();
    app.wait_for("role:button[Read src/model.rs]").unwrap();
    app.press("role:button[Read src/model.rs]").unwrap();
    app.wait_for("text:Contents of src/model.rs\nFetched from the worktree")
        .unwrap();
    app.press("role:button[Copy Output]").unwrap();
    assert_eq!(
        app.screen().clipboard(),
        Some("Contents of src/model.rs\nFetched from the worktree")
    );
    assert!(
        app.screen()
            .shows("Contents of src/model.rs\nFetched from the worktree")
    );
    assert!(
        !app.screen()
            .shows("Contents of src/sidebar.rs\nFetched from the worktree")
    );
    app.press("role:button[Read 2 files]").unwrap();
    app.wait_for_gone(
        "text:Contents of src/model.rs\nFetched from the worktree",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
    assert!(app.screen().shows("Read 2 files"));
}

#[test]
fn a_match_in_tool_output_opens_only_the_matching_detail() {
    let mut config = EngineConfig::new(Root::of::<ActivityTimelineShell>());
    config.viewport = nsh_core::geometry::Size::new(px(320.0), px(860.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Here is the answer.").unwrap();
    find_in_conversation(&mut app);
    app.type_text("text:Find in conversation", "Fetched from the worktree")
        .unwrap();
    app.wait_for("1/2").unwrap();
    app.wait_for("role:button[Copy Output]").unwrap();
    assert!(
        !app.screen()
            .shows("Contents of src/sidebar.rs\nFetched from the worktree")
    );
    app.press("role:button[Next match]").unwrap();
    app.wait_for("2/2").unwrap();
    app.wait_for("text:Contents of src/sidebar.rs\nFetched from the worktree")
        .unwrap();
}

#[test]
fn a_photo_alone_can_be_sent() {
    let mut app = start();
    app.press("role:button[Add to message]").unwrap();
    app.press("role:button[Photo]").unwrap();
    app.press("role:button[Send]").unwrap();
    app.wait_for("sent an attachment").unwrap();
}

#[test]
fn the_drawer_lists_sessions_and_switching_one_swaps_the_transcript() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    assert!(
        app.screen().enabled("role:button[Settings]").unwrap(),
        "the drawer carries the way into settings"
    );
    assert!(app.screen().shows("vibex"), "the project groups the rows");
    app.type_text("text:Search sessions", "zzz").unwrap();
    app.wait_for_gone("Fix the tests", std::time::Duration::from_secs(5))
        .unwrap();
    app.press("role:button[Clear search]").unwrap();
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
fn selected_sessions_can_become_a_group() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[Sidebar options]").unwrap();
    app.press("Select or reorder").unwrap();
    app.wait_for("Fix the tests").unwrap();
    app.press("Fix the tests").unwrap();
    app.wait_for("1 selected").unwrap();
    assert!(!app.screen().enabled("role:button[Group]").unwrap());
    app.press("role:button[Select: vibex]").unwrap();
    app.wait_for("2 selected").unwrap();
    app.press("role:button[Group]").unwrap();
    app.wait_for("Group name").unwrap();
}

#[test]
fn a_collapsed_folder_hides_its_sessions() {
    let mut config = EngineConfig::new(Root::of::<CollapsedFolderShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Work").unwrap();
    assert!(!app.screen().shows("Fix the tests"));
}

#[test]
fn a_long_row_title_is_ellipsized_instead_of_widening_the_drawer() {
    let mut config = EngineConfig::new(Root::of::<DrawerLongTitleShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for(DRAWER_LONG_TITLE).unwrap();
    let node = app
        .screen()
        .layout_nodes()
        .into_iter()
        .find(|node| {
            node.text
                .as_ref()
                .is_some_and(|text| text.text.text == DRAWER_LONG_TITLE)
        })
        .expect("the long title is on screen");
    let text = node.text.expect("the node carries its text");
    assert_eq!(text.text.max_lines, 1, "the title is one line");
    assert_eq!(text.text.ellipsize, nsh_core::EllipsizeMode::Tail);
    assert!(
        text.content.size.width.0 < 340.0,
        "the title is cut to the row, not laid out at {}pt wide",
        text.content.size.width.0
    );
}

#[test]
fn a_group_opens_its_actions() {
    let mut config = EngineConfig::new(Root::of::<GroupShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Pair").unwrap();
    app.long_press("role:button[Pair]").unwrap();
    app.wait_for("Pin").unwrap();
    assert!(app.screen().shows("Auto continue"));
    assert!(app.screen().shows("Dissolve group"));
    app.press("Edit members").unwrap();
    app.wait_for("Group members").unwrap();
}

#[test]
fn a_folder_can_be_named_from_the_drawer() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.long_press("Projects").unwrap();
    app.wait_for("New folder").unwrap();
    app.press("New folder").unwrap();
    app.wait_for("Folder name").unwrap();
    app.type_text("text:Folder name", "Work").unwrap();
    assert!(app.screen().shows("Create"));
}

#[test]
fn add_project_opens_the_chosen_runtime_directory() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[New project]").unwrap();
    app.press("demo").unwrap();
    app.wait_for("/workspace/demo").unwrap();
    app.press("role:button[Open project]").unwrap();
    app.wait_for_gone("/workspace/demo", std::time::Duration::from_secs(3))
        .unwrap();
    app.wait_for("role:button[Back to session]").unwrap();
}

#[test]
fn a_sidebar_folder_groups_its_sessions() {
    let mut config = EngineConfig::new(Root::of::<FolderShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Work").unwrap();
    assert!(app.screen().shows("Fix the tests"));
    app.long_press("role:button[Work]").unwrap();
    app.wait_for("Rename").unwrap();
}

#[test]
fn the_project_row_menu_carries_the_desktop_actions() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("role:button[vibex]").unwrap();
    app.long_press("role:button[vibex]").unwrap();
    app.wait_for("New session").unwrap();
    assert!(app.screen().shows("New folder"));
    assert!(app.screen().shows("Start sessions in a new worktree"));
    assert!(app.screen().shows("Remove project"));
}

#[test]
fn the_folder_row_menu_carries_the_desktop_actions() {
    let mut config = EngineConfig::new(Root::of::<FolderShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Work").unwrap();
    app.long_press("role:button[Work]").unwrap();
    app.wait_for("New chat in folder Work").unwrap();
    assert!(app.screen().shows("Scheduled archive · Off"));
    assert!(app.screen().shows("Delete"));
}

#[test]
fn the_session_row_menu_carries_the_desktop_actions() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Sessions").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.long_press("role:button[Fix the tests]").unwrap();
    app.wait_for("Fork session").unwrap();
    assert!(app.screen().shows("Auto continue"));
    assert!(app.screen().shows("Pin"));
}

#[test]
fn a_pinned_session_keeps_its_marker() {
    let mut config = EngineConfig::new(Root::of::<PinnedShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("label:Pinned").unwrap();
}

#[test]
fn the_drawer_selects_sessions_for_a_batch() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    app.wait_for("role:button[New session here]").unwrap();
    app.press("role:button[Sidebar options]").unwrap();
    app.press("Select or reorder").unwrap();
    app.wait_for("Archive").unwrap();
    app.wait_for_gone(
        "role:button[New session here]",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
    app.press("Fix the tests").unwrap();
    app.wait_for("1 selected").unwrap();
    app.press("role:button[Done]").unwrap();
    app.wait_for_gone("role:button[Archive]", std::time::Duration::from_secs(5))
        .unwrap();
    app.wait_for("role:button[New session here]").unwrap();
}

#[test]
fn a_message_can_be_scheduled() {
    let mut app = start();
    app.type_text(COMPOSER, "Later").unwrap();
    app.press("role:button[Add to message]").unwrap();
    app.press("role:button[Schedule message]").unwrap();
    app.wait_for("In 5 minutes").unwrap();
    app.press("In 5 minutes").unwrap();
    app.wait_for("Message scheduled").unwrap();
}

#[test]
fn the_conversation_can_be_searched() {
    let mut app = start();
    find_in_conversation(&mut app);
    app.type_text("text:Find in conversation", "green").unwrap();
    app.wait_for("1/1").unwrap();
    app.press("role:button[Next match]").unwrap();
    app.press("Close").unwrap();
    app.wait_for_gone("1/1", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn the_composer_appends_what_was_sent() {
    let mut app = start();
    app.type_text(COMPOSER, "Ship it").unwrap();
    app.press("role:button[Send]").unwrap();
    app.wait_for("Ship it").unwrap();
    assert_eq!(app.screen().input_text(COMPOSER).unwrap(), Some(""));
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
    app.type_text(COMPOSER, "/").unwrap();
    app.wait_for("Review changes").unwrap();
    app.press("Review changes").unwrap();
    app.wait_for("text:/review ").unwrap();
}

#[test]
fn a_new_session_picks_a_workspace_and_runtime_then_opens() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    assert!(
        app.screen().enabled("role:button[Settings]").unwrap(),
        "the drawer carries the way into settings"
    );
    assert!(app.screen().shows("vibex"), "the project groups the rows");
    app.type_text("text:Search sessions", "zzz").unwrap();
    app.wait_for_gone("Fix the tests", std::time::Duration::from_secs(5))
        .unwrap();
    app.press(NEW_SESSION).unwrap();
    // The session is written on the conversation page, which the sessions
    // page gives way to.
    app.wait_for("Claude · Sonnet").unwrap();
    app.wait_for_gone(
        "role:button[Back to session]",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
    assert!(app.screen().shows("What are we working on?"));
    app.press("role:button[Create]").unwrap();
    app.wait_for("Created session").unwrap();
}

#[test]
fn a_session_is_written_under_the_conversations_own_header() {
    let mut app = start();
    // "New session" lives in the drawer: the conversation's header has no
    // second way to the same page.
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("role:button[New session]").unwrap();
    app.press("role:button[New session]").unwrap();
    app.wait_for("role:button[Choose workspace]").unwrap();
    // The transcript gives its place to the blank page, and comes back when
    // the session is given up.
    assert!(!app.screen().shows("Morning. The build is green."));
    // A way to begin writes its message into the prompt card.
    app.press("Tour this project").unwrap();
    app.wait_for("text:/^Give me a tour of this project/")
        .unwrap();
    app.press("role:button[Cancel]").unwrap();
    app.wait_for("Morning. The build is green.").unwrap();
    assert!(
        !app.screen()
            .matches("role:button[Choose workspace]")
            .unwrap()
    );
}

/// The shell with its terminal open; it writes a line into the terminal after
/// the first frame, as the runtime's snapshot would.
#[component]
fn TerminalShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    on_back_press(session::handle_back);
    host.terminal_open.set(true);
    let terminal_ref = host.terminal_ref;
    host.on_terminal_data = Rc::new(move |data| {
        if let Some(api) = terminal_ref.api::<TerminalApi>() {
            let display = data
                .replace('\r', "[return]")
                .replace('\u{7f}', "[delete]")
                .replace('\u{3}', "[ctrl-c]");
            api.write(format!("\r\ninput: {display}"));
        }
    });
    set_timeout(std::time::Duration::from_millis(50), move || {
        if let Some(api) = terminal_ref.api::<TerminalApi>() {
            api.write("hello from the pty\n".to_owned());
        }
    });
    session::session(host)
}

#[test]
fn the_terminal_opens_and_shows_the_programs_output() {
    let mut config = EngineConfig::new(Root::of::<TerminalShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("hello from the pty").unwrap();
    assert!(app.screen().shows("Esc"));
    assert!(app.screen().shows("Ctrl"));
    app.press("role:button[Find in terminal]").unwrap();
    app.type_text("text:Find in terminal", "hello").unwrap();
    app.wait_for("1 matches").unwrap();
    app.back().unwrap();
    app.wait_for_gone("1 matches", std::time::Duration::from_secs(3))
        .unwrap();
    app.wait_for("role:button[Keyboard]").unwrap();
}

#[test]
fn the_terminal_keyboard_sends_native_edits_and_return() {
    let mut config = EngineConfig::new(Root::of::<TerminalShell>());
    config.viewport = nsh_core::geometry::Size::new(px(320.0), px(640.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[Keyboard]").unwrap();
    app.press("role:button[Keyboard]").unwrap();
    app.press_key("x").unwrap();
    app.wait_for("text:input: x").unwrap();
    app.press_key("backspace").unwrap();
    app.wait_for("text:input: [delete]").unwrap();
    app.press_key("enter").unwrap();
    app.wait_for("text:input: [return]").unwrap();
}

#[test]
fn the_at_palette_inserts_a_workspace_reference() {
    let mut app = start();
    app.type_text(COMPOSER, "@src").unwrap();
    app.wait_for("src/main.rs").unwrap();
    app.press("src/main.rs").unwrap();
    app.wait_for("text:@src/main.rs ").unwrap();
}

/// The shell with its files surface open.
#[component]
fn FilesShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.files_open.set(true);
    session::session(host)
}

#[test]
fn the_files_surface_lists_a_directory_and_reads_a_file() {
    let mut config = EngineConfig::new(Root::of::<FilesShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("README.md").unwrap();
    app.press("src").unwrap();
    app.wait_for("main.rs").unwrap();
    assert!(app.screen().shows("README.md"));
    app.press("src").unwrap();
    app.wait_for_gone("main.rs", std::time::Duration::from_secs(3))
        .unwrap();
    app.press("src").unwrap();
    app.press("main.rs").unwrap();
    app.wait_for("role:button[Go to line]").unwrap();
    assert!(!app.screen().matches("role:button[Save]").unwrap());
}

/// The shell with its Git surface open.
#[component]
fn GitShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.git_open.set(true);
    session::session(host)
}

#[test]
fn the_file_browser_searches_content() {
    let mut config = EngineConfig::new(Root::of::<FilesShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("README.md").unwrap();
    app.press("role:button[Content]").unwrap();
    app.wait_for("role:button[Case sensitive]").unwrap();
    app.type_text("text:Search files", "hello").unwrap();
    app.wait_for("main.rs:3").unwrap();
    app.press("main.rs:3").unwrap();
    app.wait_for("role:button[Go to line]").unwrap();
}

#[test]
fn a_markdown_file_is_drawn_as_markdown() {
    let mut config = EngineConfig::new(Root::of::<FilesShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("README.md").unwrap();
    app.press("README.md").unwrap();
    app.wait_for("Hello").unwrap();
}

#[test]
fn the_git_worktree_sheet_lists_managed_worktrees() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("src/main.rs").unwrap();
    app.press("role:button[Worktrees]").unwrap();
    app.wait_for("/tmp/vibex-tree").unwrap();
    assert!(app.screen().shows("Merge"));
    assert!(app.screen().shows("Continue"));
    assert!(app.screen().shows("Abort"));
    assert!(app.screen().shows("src/lib.rs"));
    assert!(app.screen().shows("Target"));
}

#[test]
fn the_git_branch_sheet_lists_local_branches() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("src/main.rs").unwrap();
    app.press("role:button[Branches]").unwrap();
    app.wait_for("origin/main").unwrap();
}

#[test]
fn the_git_surface_lists_changes_and_shows_a_diff() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("src/main.rs").unwrap();
    for tool in ["Fetch", "Pull", "Push", "Amend", "Push after"] {
        assert!(
            app.screen()
                .matches(&format!("role:button[{tool}]"))
                .unwrap(),
            "the Git tools offer {tool}"
        );
    }
    app.press("src/main.rs").unwrap();
    app.wait_for("diff --git a/src/main.rs b/src/main.rs")
        .unwrap();
}

#[test]
fn a_git_change_can_be_staged_from_the_surface() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("src/main.rs").unwrap();
    app.press("role:button[Stage]").unwrap();
    app.wait_for("role:button[Unstage]").unwrap();
}

#[test]
fn the_git_surface_commits_and_shows_history() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Commit").unwrap();
    app.type_text("text:Commit message", "init").unwrap();
    app.press("role:button[Commit]").unwrap();
    // The composer clears when the commit is accepted.
    app.wait_for("text:Commit message").unwrap();
    app.press("role:button[Commits]").unwrap();
    app.wait_for("Initial commit").unwrap();
    app.type_text("text:Search commits", "zzz").unwrap();
    app.wait_for_gone("Initial commit", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn the_read_only_file_viewer_can_find_in_source() {
    let mut config = EngineConfig::new(Root::of::<FilesShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("README.md").unwrap();
    app.press("src").unwrap();
    app.wait_for("main.rs").unwrap();
    app.press("main.rs").unwrap();
    app.wait_for("role:button[Find in file]").unwrap();
    app.press("role:button[Find in file]").unwrap();
    app.type_text("text:Find in file", "greeting").unwrap();
    app.wait_for("0/2").unwrap();
    app.press("role:button[Next match]").unwrap();
    app.wait_for("1/2").unwrap();
}

#[test]
fn a_history_row_opens_the_commit_detail() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Commit").unwrap();
    app.press("role:button[Commits]").unwrap();
    app.wait_for("Initial commit").unwrap();
    app.press("Initial commit").unwrap();
    app.wait_for("commit detail body").unwrap();
}

#[test]
fn a_photo_can_be_attached_and_removed() {
    let mut app = start();
    app.press("role:button[Add to message]").unwrap();
    app.press("role:button[Photo]").unwrap();
    app.wait_for("photo.jpg").unwrap();
    app.press("photo.jpg").unwrap();
    app.wait_until(
        "the chip leaves",
        std::time::Duration::from_secs(5),
        |screen| screen.texts().iter().all(|text| *text != "photo.jpg"),
    )
    .unwrap();
}

/// The shell with a single session, so the drawer's action button is unique.
#[component]
fn OneSessionShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sessions.set(vec![SessionSummary {
        id: "s1".to_owned(),
        title: "Fix the tests".to_owned(),
        agent: "Claude".to_owned(),
        meta: "2m".to_owned(),
        state: SessionState::Running,
        terminal: false,
        project_id: "p1".to_owned(),
        project_name: "vibex".to_owned(),
        workspace_id: "w1".to_owned(),
        workspace_label: "main".to_owned(),
    }]);
    session::session(host)
}

#[component]
fn AgentTerminalShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sessions.update(|sessions| sessions[0].terminal = true);
    host.timeline.set(Vec::new());
    session::session(host)
}

#[component]
fn FailedConversationShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.timeline.set(Vec::new());
    host.conversation
        .set(ConversationState::Failed("Conversation read failed".into()));
    let status = host.conversation;
    host.on_refresh = Rc::new(move || status.set(ConversationState::Ready));
    session::session(host)
}

#[test]
fn a_session_can_be_renamed_from_the_drawer() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Sessions").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.long_press("role:button[Fix the tests]").unwrap();
    app.press("role:button[Rename]").unwrap();
    app.wait_for("Rename session").unwrap();
    app.type_text("text:Fix the tests", " v2").unwrap();
    app.press("role:button[Save]").unwrap();
    app.wait_for("Fix the tests v2").unwrap();
}

#[test]
fn a_session_can_be_archived_from_the_drawer() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Sessions").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.long_press("role:button[Fix the tests]").unwrap();
    app.press("role:button[Archive]").unwrap();
    app.wait_until(
        "the row leaves",
        std::time::Duration::from_secs(5),
        |screen| screen.texts().iter().all(|text| *text != "Fix the tests"),
    )
    .unwrap();
}

#[test]
fn a_session_can_be_deleted_from_the_drawer() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Sessions").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.long_press("role:button[Fix the tests]").unwrap();
    app.press("role:button[Delete]").unwrap();
    app.wait_until(
        "the row leaves",
        std::time::Duration::from_secs(5),
        |screen| screen.texts().iter().all(|text| *text != "Fix the tests"),
    )
    .unwrap();
}

#[test]
fn a_running_turn_can_be_stopped_from_the_composer() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[Stop]").unwrap();
    app.press("role:button[Stop]").unwrap();
    app.wait_for("role:button[Send]").unwrap();
}

#[test]
fn a_draft_can_be_submitted_while_a_turn_is_running() {
    let mut app = Headless::start(EngineConfig::new(Root::of::<OneSessionShell>())).unwrap();
    app.wait_for("role:button[Stop]").unwrap();
    app.type_text(COMPOSER, "Run the next check").unwrap();
    app.press("role:button[Queue for the turn]").unwrap();
    app.wait_for("Run the next check").unwrap();
    app.wait_for("role:button[Stop]").unwrap();
}

#[test]
fn both_side_pages_open_and_close_with_horizontal_drags() {
    let mut app = start();
    let travel = |distance| nsh_core::geometry::Point::new(px(distance), px(0.0));
    app.drag("role:button[Sessions]", travel(130.0)).unwrap();
    app.wait_for("role:button[Back to session]").unwrap();
    app.drag("role:button[Back to session]", travel(-130.0))
        .unwrap();
    app.wait_for_gone(
        "role:button[Back to session]",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
    app.drag("role:button[Files, Git, and terminal]", travel(-130.0))
        .unwrap();
    app.wait_for("role:button[Changes]").unwrap();
    app.drag("role:button[Changes]", travel(130.0)).unwrap();
    app.wait_for_gone("role:button[Changes]", std::time::Duration::from_secs(3))
        .unwrap();
}

#[test]
fn vertical_drag_keeps_the_conversation_open() {
    let mut app = start();
    app.drag(
        "role:button[Sessions]",
        nsh_core::geometry::Point::new(px(20.0), px(130.0)),
    )
    .unwrap();
    assert!(
        app.screen()
            .matches("role:button[Files, Git, and terminal]")
            .unwrap()
    );
    assert!(
        !app.screen()
            .matches("role:button[Back to session]")
            .unwrap()
    );
    assert!(!app.screen().matches("role:button[Changes]").unwrap());
}

#[test]
fn back_closes_the_drawer_menu_before_the_drawer() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.long_press("Projects").unwrap();
    app.wait_for("Select or reorder").unwrap();
    app.back().unwrap();
    app.wait_for_gone("Select or reorder", std::time::Duration::from_secs(3))
        .unwrap();
    app.wait_for("role:button[Back to session]").unwrap();
    app.back().unwrap();
    app.wait_for_gone(
        "role:button[Back to session]",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
}

#[test]
fn a_terminal_session_opens_the_rail_without_an_acp_composer() {
    let mut app = Headless::start(EngineConfig::new(Root::of::<AgentTerminalShell>())).unwrap();
    app.wait_for("role:button[Changes]").unwrap();
    app.wait_for("role:button[Find in terminal]").unwrap();
    assert!(!app.screen().matches("role:button[Terminals]").unwrap());
    app.press("role:button[Close]").unwrap();
    app.wait_for("role:button[Open terminal]").unwrap();
    assert!(!app.screen().matches(COMPOSER).unwrap());
}

#[test]
fn a_failed_conversation_is_not_presented_as_empty() {
    let mut app =
        Headless::start(EngineConfig::new(Root::of::<FailedConversationShell>())).unwrap();
    app.wait_for("Conversation read failed").unwrap();
    assert!(!app.screen().shows("No messages yet"));
    app.press("role:button[Retry]").unwrap();
    app.wait_for("No messages yet").unwrap();
}

#[test]
fn the_rail_tabs_are_reachable_on_a_narrow_phone() {
    let mut config = EngineConfig::new(Root::of::<Shell>());
    config.viewport = nsh_core::geometry::Size::new(px(320.0), px(640.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[Files, Git, and terminal]")
        .unwrap();
    app.press("role:button[Files, Git, and terminal]").unwrap();
    app.wait_for("role:button[Terminal]").unwrap();
    app.press("role:button[Terminal]").unwrap();
    app.wait_for("role:button[Find in terminal]").unwrap();
    app.press("role:button[Close]").unwrap();
    app.wait_for("role:button[Send]").unwrap();
}

#[test]
fn workspace_tabs_keep_the_preview_and_the_conversation_draft() {
    let mut app = start();
    app.type_text(COMPOSER, "Keep this draft").unwrap();
    app.press("role:button[Files, Git, and terminal]").unwrap();
    app.wait_for("README.md").unwrap();
    app.press("README.md").unwrap();
    app.wait_for("Hello").unwrap();
    app.press("role:button[Changes]").unwrap();
    app.wait_for("src/main.rs").unwrap();
    app.wait_for("↓1").unwrap();
    app.wait_for("↑2").unwrap();
    app.press("role:button[Files]").unwrap();
    app.wait_for("Hello").unwrap();
    app.back().unwrap();
    app.wait_for_gone("Hello", std::time::Duration::from_secs(3))
        .unwrap();
    app.wait_for("role:button[Changes]").unwrap();
    app.back().unwrap();
    app.wait_for_gone("role:button[Changes]", std::time::Duration::from_secs(3))
        .unwrap();
    assert_eq!(
        app.screen().input_text("text:Keep this draft").unwrap(),
        Some("Keep this draft")
    );
}

#[test]
fn terminal_tabs_switch_and_can_close_a_terminal() {
    let mut config = EngineConfig::new(Root::of::<TerminalShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[Close terminal: Build]").unwrap();
    app.press("Build").unwrap();
    app.press("role:button[Close terminal: Build]").unwrap();
    app.wait_for("Close this terminal and stop its running process?")
        .unwrap();
    app.press("Cancel").unwrap();
    app.wait_for("role:button[Close terminal: Build]").unwrap();
    app.press("role:button[Close terminal: Build]").unwrap();
    app.press("Close terminal").unwrap();
    app.wait_for_gone(
        "role:button[Close terminal: Build]",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
}

#[test]
fn a_new_worktree_session_can_be_started_from_the_creation_page() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press(NEW_SESSION).unwrap();
    app.wait_for("role:button[Choose workspace]").unwrap();
    app.press("role:button[Choose workspace]").unwrap();
    app.press("New worktree").unwrap();
    app.wait_for("text:Branch name").unwrap();
    app.type_text("text:Branch name", "feat/mobile").unwrap();
    app.press("role:button[Create]").unwrap();
    app.wait_for("Worktree session").unwrap();
}

#[test]
fn backing_out_of_creation_returns_to_the_open_conversation() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press(NEW_SESSION).unwrap();
    app.wait_for("role:button[Choose workspace]").unwrap();
    app.back().unwrap();
    app.wait_for_gone(
        "role:button[Choose workspace]",
        std::time::Duration::from_secs(3),
    )
    .unwrap();
    app.wait_for("Morning. The build is green.").unwrap();
    assert!(
        !app.screen()
            .matches("role:button[Back to session]")
            .unwrap()
    );
}

#[component]
fn LongTitleShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sessions.update(|sessions| {
        sessions[0].title =
            "A very long session title that must not cover the controls ".repeat(12);
    });
    session::session(host)
}

#[test]
fn a_long_title_does_not_push_header_controls_out_of_reach() {
    let mut config = EngineConfig::new(Root::of::<LongTitleShell>());
    config.viewport = nsh_core::geometry::Size::new(px(320.0), px(640.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[Sessions]").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("role:button[Back to session]").unwrap();
    app.press("role:button[Back to session]").unwrap();
    find_in_conversation(&mut app);
    app.press("role:button[Files, Git, and terminal]").unwrap();
    app.wait_for("role:button[Terminal]").unwrap();
}

#[test]
fn dragging_an_open_folder_row_collapses_it_and_carries_the_row_with_the_finger() {
    let mut config = EngineConfig::new(Root::of::<FolderShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("role:button[Work]").unwrap();
    assert!(
        app.screen().shows("Fix the tests"),
        "the folder starts open"
    );
    // Every node drawing the folder's title, top to bottom: the row's own
    // place and, once it is carried, the copy the page draws under the
    // finger.
    let rows = |app: &Headless| {
        let mut tops: Vec<f32> = app
            .screen()
            .layout_nodes()
            .into_iter()
            .filter(|node| {
                node.text
                    .as_ref()
                    .is_some_and(|text| text.text.text == "Work")
            })
            .map(|node| node.bounds.origin.y.0)
            .collect();
        tops.sort_by(|a, b| a.partial_cmp(b).expect("row tops are finite"));
        tops
    };
    let resting = rows(&app);
    assert_eq!(resting.len(), 1, "the folder row is on screen once");
    app.long_press("role:button[Work]").unwrap();
    app.wait_for("Rename").unwrap();
    app.drag_and_hold(
        "role:button[Work]",
        nsh_core::geometry::Point::new(px(0.0), px(80.0)),
    )
    .unwrap();
    // The row is carried with the finger: the page draws it where the finger
    // is, not where it was picked up.
    let carried = rows(&app);
    let followed = carried
        .iter()
        .any(|top| (top - resting[0] - 70.0).abs() < 12.0);
    assert!(
        followed,
        "the carried row follows the finger: it rested at {resting:?} and is at {carried:?}"
    );
    // The menu gives way to the drag, and the open folder's children leave
    // the list so the row is carried alone.
    assert!(!app.screen().shows("Rename"), "the drag puts the menu away");
    assert!(
        !app.screen().shows("Fix the tests"),
        "an open folder's children collapse as it is carried"
    );
    app.drop_held().unwrap();
    assert!(
        !app.screen().shows("Fix the tests"),
        "the folder stays collapsed once the row is dropped"
    );
}

#[test]
fn a_waiting_session_row_draws_the_desktops_question_mark_icon() {
    let mut config = EngineConfig::new(Root::of::<WaitingShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("role:button[Good morning]").unwrap();
    // The desktop marks the state with `icons/circle-question-mark.svg`; the
    // phone typed a "?" glyph instead, which is the one thing a shared mark
    // must not be. The mark is an icon, so the row carries no "?" text.
    let question_texts: Vec<_> = app
        .screen()
        .layout_nodes()
        .into_iter()
        .filter(|node| {
            node.text
                .as_ref()
                .is_some_and(|text| text.text.text.trim() == "?")
        })
        .collect();
    assert!(
        question_texts.is_empty(),
        "the waiting mark is the desktop's icon, not a typed glyph"
    );
}

#[test]
fn the_new_project_sheet_offers_the_runtimes_places_and_a_path_to_type() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[New project]").unwrap();
    app.wait_for("demo").unwrap();
    // The roots the runtime allows are listed, so a reader can jump to one
    // instead of climbing back to it.
    // Each place is named by its last segment, as a file browser names them.
    assert!(
        app.screen().shows("workspace") && app.screen().shows("projects"),
        "both browse roots are offered as places"
    );
    // And the path is a field: a directory the list would take many steps to
    // reach can be typed instead.
    // The path is the field: tapping it turns the line into an input, so a
    // directory the list would take many steps to reach can be typed.
    // The field opens on the directory being browsed, so the reader edits a
    // path rather than retyping one.
    app.press("text:/workspace").unwrap();
    app.clear("id:project-path").unwrap();
    app.type_text("id:project-path", "/srv/projects/vibex")
        .unwrap();
    app.press("role:button[Open]").unwrap();
    app.wait_for("/srv/projects/vibex").unwrap();
}

#[test]
fn the_conversation_header_raises_itself_when_its_menu_opens() {
    let mut app = start();
    app.wait_for("Good morning").unwrap();
    // The header is a bar on the page until its menu is asked for; opening the
    // menu lifts it out of the page the way a held drawer row is lifted.
    let header_text = |app: &Headless| {
        app.screen()
            .layout_nodes()
            .into_iter()
            .filter(|node| {
                node.text
                    .as_ref()
                    .is_some_and(|text| text.text.text == "Good morning")
            })
            .map(|node| node.bounds.origin.y.0)
            .collect::<Vec<_>>()
    };
    let before = header_text(&app);
    app.press(CONVERSATION_MENU).unwrap();
    app.wait_for("Find in conversation").unwrap();
    let after = header_text(&app);
    // The lifted card is drawn over the page, so the title appears twice: once
    // in the header's place and once in the card standing over it.
    assert!(
        after.len() > before.len(),
        "the header is lifted into a card: {before:?} -> {after:?}"
    );
}

#[test]
fn a_process_row_is_its_prose_without_a_name_or_actions() {
    let mut config = EngineConfig::new(Root::of::<TurnShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Worked for 2s").unwrap();
    // The turn's answer carries the actions; the process rows above it do not
    // repeat them, so the transcript has one set of controls per turn.
    assert!(
        app.screen().matches("role:button[Fork]").unwrap(),
        "the answer keeps the row actions"
    );
    // The prose is the prose: the Agent is named by the turn's fold above, so
    // no paragraph — the answer's included — repeats its name.
    let named = app
        .screen()
        .layout_nodes()
        .into_iter()
        .filter(|node| {
            node.text
                .as_ref()
                .is_some_and(|text| text.text.text == "Claude")
        })
        .count();
    assert_eq!(named, 0, "no paragraph repeats the Agent's name");
}

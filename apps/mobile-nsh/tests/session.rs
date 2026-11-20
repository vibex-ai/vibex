//! The session shell's flows, rendered headlessly through the real engine.
//!
//! The shell is mounted with a fixture host here: the drawer opens, a session
//! opens its transcript, and the composer sends. The app's own wiring to the
//! remote backend is tested where that backend is available; these tests keep
//! the screen's behavior fixed as it grows.

use std::rc::Rc;

use nsh_base::{Terminal, TerminalApi};
use nsh_core::prelude::*;
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use vibex_nsh_ui::screens::connecting::ConnectionState;
use vibex_nsh_ui::screens::files::{FileEntry, FilePreview, FilePreviewKindView};
use vibex_nsh_ui::screens::git::{
    BranchRow, CommitDetailPreview, CommitRow, GitChangeRow, GitDiffPreview, WorktreeConflictRow,
    WorktreeOperationRow, WorktreeRow,
};
use vibex_nsh_ui::screens::new_session::{RuntimeChoice, WorkspaceChoice};
use vibex_nsh_ui::screens::session::{
    self, AttachmentItem, CommandEntry, ComposerHistoryEntry, EntryKind, GoalPhaseView, GoalView,
    PendingPermission, PendingQuestion, PermissionChoice, PermissionOption, PlanStepState,
    PlanStepView, PlanView, QuestionField, QuestionFieldKind, QueuedMessageView, RuntimeChoiceView,
    RuntimeOptionView, RuntimeSummaryView, SessionHost, SessionState, SessionSummary,
    SidebarSectionKind, SidebarSectionView, TimelineEntry, TokenUsageView,
};
use vibex_nsh_ui::screens::team::TeamTaskView;
use vibex_nsh_ui::screens::terminal::TerminalChoice;

fn entry(id: &str, kind: EntryKind, title: &str, body: &str) -> TimelineEntry {
    TimelineEntry {
        id: id.to_owned(),
        kind,
        title: title.to_owned(),
        body: body.to_owned(),
        meta: String::new(),
        streaming: false,
        turn_id: id.to_owned(),
        process_count: 0,
        expanded: false,
        expandable: false,
        attachments: Vec::new(),
        last_sequence: 0,
        started_at_ms: 0,
        ended_at_ms: 0,
        complete: true,
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
    header.process_count = 2;
    header.expanded = true;
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
    host.timeline = signal(vec![
        user,
        header,
        tool,
        permission,
        entry("c1", EntryKind::Changes, "1 files changed", "src/main.rs"),
        entry(
            "a1",
            EntryKind::Agent,
            "Claude",
            "![shot](https://example.com/shot.png)\nDone",
        ),
    ]);
    session::session(host)
}

/// A shell with one authority folder.
#[component]
fn FolderShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sidebar_sections.set(vec![SidebarSectionView {
        kind: SidebarSectionKind::Folder,
        parent_id: None,
        depth: 0,
        collapsed: false,
        id: "folder-1".to_owned(),
        name: "Work".to_owned(),
        session_ids: vec!["s2".to_owned()],
    }]);
    session::session(host)
}

/// A shell with one session group.
#[component]
fn GroupShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sidebar_sections.set(vec![SidebarSectionView {
        kind: SidebarSectionKind::Group,
        parent_id: None,
        depth: 0,
        collapsed: false,
        id: "group-1".to_owned(),
        name: "Pair".to_owned(),
        session_ids: vec!["s1".to_owned(), "s2".to_owned()],
    }]);
    session::session(host)
}

/// A shell with a collapsed authority folder.
#[component]
fn CollapsedFolderShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.sidebar_sections.set(vec![SidebarSectionView {
        kind: SidebarSectionKind::Folder,
        parent_id: None,
        depth: 0,
        collapsed: true,
        id: "folder-1".to_owned(),
        name: "Work".to_owned(),
        session_ids: vec!["s2".to_owned()],
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
        available: true,
    }));
    host.runtime_options = signal(vec![
        RuntimeOptionView {
            id: "0".to_owned(),
            agent: "Claude".to_owned(),
            model: "Claude 4".to_owned(),
            detail: "Pro".to_owned(),
            selected: true,
            available: true,
        },
        RuntimeOptionView {
            id: "1".to_owned(),
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

/// A transcript that knows it has older history.
#[component]
fn OlderShell() -> impl IntoElement {
    let mut host = host_with(None, None);
    host.has_older = signal(true);
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

fn host_with(pending: Option<PendingPermission>, question: Option<PendingQuestion>) -> SessionHost {
    let sessions = signal(sessions());
    let active = signal(Some("s1".to_owned()));
    let pinned = signal(Vec::new());
    let unread = signal(Vec::new());
    let auto_continue_sessions = signal(Vec::new());
    let sidebar_project_order = signal(Vec::new());
    let sidebar_workspace_order = signal(Vec::new());
    let sidebar_session_order = signal(Vec::new());
    let collapsed_projects = signal(Vec::new());
    let collapsed_workspaces = signal(Vec::new());
    let sidebar_sections = signal(Vec::new());
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
    let terminal_menu = signal(false);
    let on_select_terminal = Rc::new(move |id: &str| {
        if let Some(choice) = terminal_choices.with(|choices| {
            choices
                .iter()
                .find(|choice| choice.id == id)
                .map(|choice| choice.title.clone())
        }) {
            terminal_title.set(Some(choice.into()));
            terminal_menu.set(false);
        }
    });
    let files_open = signal(false);
    let files_path = signal(String::new());
    let files_entries = signal(vec![
        FileEntry {
            path: "src".to_owned(),
            name: "src".to_owned(),
            directory: true,
        },
        FileEntry {
            path: "README.md".to_owned(),
            name: "README.md".to_owned(),
            directory: false,
        },
    ]);
    let files_preview = signal(None::<FilePreview>);
    let files_error = signal(None::<SharedString>);
    let files_content = signal(String::new());
    let files_dirty = signal(false);
    let files_saving = signal(false);
    let on_open_dir = Rc::new(move |path: &str| {
        files_path.set(path.to_owned());
        files_entries.set(vec![FileEntry {
            path: "src/main.rs".to_owned(),
            name: "main.rs".to_owned(),
            directory: false,
        }]);
    });
    let on_open_file = Rc::new(move |path: &str| {
        let markdown = path.ends_with(".md");
        files_preview.set(Some(FilePreview {
            title: path.rsplit('/').next().unwrap_or(path).to_owned(),
            body: if markdown {
                "# Hello".to_owned()
            } else {
                String::new()
            },
            truncated: false,
            language: if markdown { "markdown" } else { "rust" }.to_owned(),
            kind: if markdown {
                FilePreviewKindView::Markdown
            } else {
                FilePreviewKindView::Text
            },
            image_uri: None,
        }));
        files_content.set(if markdown { "# Hello" } else { "edited" }.to_owned());
        files_dirty.set(!markdown);
    });
    let on_save_file = Rc::new(move || {
        files_content.set("saved".to_owned());
        files_dirty.set(false);
    });
    let on_files_back = Rc::new(move || files_preview.set(None));
    let git_open = signal(false);
    let git_branch = signal(Some(SharedString::from("main")));
    let git_changes = signal(vec![GitChangeRow {
        path: "src/main.rs".to_owned(),
        status: "M".to_owned(),
        staged: false,
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
    let git_staged = signal(1_u32);
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
    let new_project_path = signal(String::new());
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
    let on_close_terminal = Rc::new(move || terminal_open.set(false));
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
        unread,
        auto_continue_sessions,
        sidebar_project_order,
        sidebar_workspace_order,
        sidebar_session_order,
        collapsed_projects,
        collapsed_workspaces,
        on_set_project_collapsed: Rc::new(|_, _| {}),
        on_set_workspace_collapsed: Rc::new(|_, _| {}),
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
        on_toggle_folder_collapsed: Rc::new(|_, _| {}),
        on_toggle_group_collapsed: Rc::new(|_, _| {}),
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
        on_open_settings: Rc::new(|| {}),
        on_toggle_turn: Rc::new(|_| {}),
        on_toggle_detail: Rc::new(|_, _| {}),
        on_fork: Rc::new(|_| {}),
        on_load_older: Rc::new(|| {}),
        terminal_open,
        terminal_title,
        terminal_error,
        terminal_ref,
        on_open_terminal,
        on_close_terminal,
        on_terminal_data: Rc::new(|_| {}),
        on_terminal_resize: Rc::new(|_, _| {}),
        on_terminal_title: Rc::new(|_| {}),
        on_interrupt,
        on_switch_runtime: Rc::new(|| {}),
        terminal_choices,
        terminal_menu,
        on_terminal_menu: Rc::new(move || terminal_menu.set(true)),
        on_close_terminal_menu: Rc::new(move || terminal_menu.set(false)),
        on_select_terminal,
        on_new_terminal: Rc::new(|| {}),
        files_open,
        files_path,
        files_entries,
        files_preview,
        files_error,
        on_open_files: Rc::new(move || files_open.set(true)),
        on_close_files: Rc::new(move || files_open.set(false)),
        on_open_dir,
        on_open_file,
        on_files_back,
        files_content,
        files_dirty,
        files_saving,
        on_save_file,
        git_open,
        git_branch,
        git_changes,
        git_diff,
        git_error,
        on_open_git: Rc::new(move || git_open.set(true)),
        on_close_git: Rc::new(move || git_open.set(false)),
        on_open_git_diff,
        on_stage_git: on_stage,
        on_unstage_git: on_unstage,
        git_staged,
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
        on_session_menu,
        on_close_session_actions: Rc::new(|| {}),
        on_rename_session,
        on_archive_session,
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
        new_project_path,
        on_open_project: Rc::new(|| {}),
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
    app.wait_for("42%").unwrap();
    assert!(app.screen().shows("Ship P1"));
    assert!(app.screen().shows("Continue"));
    assert!(app.screen().shows("Current plan"));
    assert!(app.screen().shows("Pause"));
    assert!(app.screen().shows("Queued · 1"));
    assert!(app.screen().shows("Hold this (+1)"));
    assert!(app.screen().shows("↑"));
    assert!(app.screen().shows("↓"));
    app.press("Claude 4").unwrap();
    app.wait_for("Agent and model").unwrap();
    app.wait_for("Reasoning").unwrap();
    app.wait_for("Codex · GPT-5").unwrap();
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
    app.type_text("text:Message the Agent…", "$").unwrap();
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
fn older_history_offers_its_read() {
    let mut config = EngineConfig::new(Root::of::<OlderShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Load earlier messages").unwrap();
}

#[test]
fn a_turn_shows_its_fold_and_the_attachments_it_carried() {
    let mut config = EngineConfig::new(Root::of::<TurnShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Worked for 2s").unwrap();
    assert!(app.screen().shows("cargo test"));
    assert!(app.screen().shows("Detail"));
    assert!(app.screen().shows("1 files changed"));
    assert!(app.screen().shows("Run cargo test?"));
    assert!(app.screen().shows("Allowed"));
    assert!(app.screen().shows("photo.jpg"));
    app.press("photo.jpg").unwrap();
    app.wait_for("Attachment").unwrap();
    assert!(app.screen().shows("Edit"));
    assert!(app.screen().shows("Copy"));
    assert!(app.screen().shows("Share"));
    assert!(app.screen().shows("Fork"));
}

#[test]
fn a_photo_alone_can_be_sent() {
    let mut app = start();
    app.press("role:button[Attach a photo]").unwrap();
    app.press("role:button[Send]").unwrap();
    app.wait_for("sent an attachment").unwrap();
}

#[test]
fn the_drawer_lists_sessions_and_switching_one_swaps_the_transcript() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    assert!(
        app.screen().shows("Settings"),
        "the drawer carries the way into settings"
    );
    assert!(app.screen().shows("vibex"), "the project groups the rows");
    assert!(app.screen().shows("main"), "the workspace is a subheader");
    app.press("role:button[Search sessions]").unwrap();
    app.type_text("text:Search sessions", "zzz").unwrap();
    app.wait_for_gone("Fix the tests", std::time::Duration::from_secs(5))
        .unwrap();
    app.press("role:button[Search sessions]").unwrap();
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
    app.press("role:button[Select sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    app.press("Fix the tests").unwrap();
    app.wait_for("1 selected").unwrap();
    app.press("Group").unwrap();
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
fn a_group_opens_its_actions() {
    let mut config = EngineConfig::new(Root::of::<GroupShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Pair").unwrap();
    app.press("role:button[Folder actions]").unwrap();
    app.wait_for("Session group").unwrap();
    app.press("Members").unwrap();
    app.wait_for("Group members").unwrap();
}

#[test]
fn a_folder_can_be_named_from_the_drawer() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[New folder]").unwrap();
    app.wait_for("Folder name").unwrap();
    app.type_text("text:Folder name", "Work").unwrap();
    assert!(app.screen().shows("Create"));
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
    app.press("role:button[Folder actions]").unwrap();
    app.wait_for("Rename").unwrap();
}

#[test]
fn a_pinned_session_keeps_its_marker() {
    let mut config = EngineConfig::new(Root::of::<PinnedShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Good morning").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("★").unwrap();
}

#[test]
fn the_drawer_selects_sessions_for_a_batch() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    app.press("role:button[Select sessions]").unwrap();
    app.wait_for("Archive").unwrap();
    app.press("Fix the tests").unwrap();
    app.wait_for("1 selected").unwrap();
    app.press("Done").unwrap();
    app.wait_for_gone("Archive", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn a_message_can_be_scheduled() {
    let mut app = start();
    app.type_text("text:Message the Agent…", "Later").unwrap();
    app.press("role:button[Schedule message]").unwrap();
    app.wait_for("In 5 minutes").unwrap();
    app.press("In 5 minutes").unwrap();
    app.wait_for("Message scheduled").unwrap();
}

#[test]
fn the_conversation_can_be_searched() {
    let mut app = start();
    app.press("role:button[Find in conversation]").unwrap();
    app.type_text("text:Find in conversation", "green").unwrap();
    app.wait_for("1/1").unwrap();
    app.press("›").unwrap();
    app.press("Close").unwrap();
    app.wait_for_gone("1/1", std::time::Duration::from_secs(5))
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
fn a_new_session_picks_a_workspace_and_runtime_then_opens() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.wait_for("Fix the tests").unwrap();
    assert!(
        app.screen().shows("Settings"),
        "the drawer carries the way into settings"
    );
    assert!(app.screen().shows("vibex"), "the project groups the rows");
    assert!(app.screen().shows("main"), "the workspace is a subheader");
    app.press("role:button[Search sessions]").unwrap();
    app.type_text("text:Search sessions", "zzz").unwrap();
    app.wait_for_gone("Fix the tests", std::time::Duration::from_secs(5))
        .unwrap();
    app.press("role:button[New session]").unwrap();
    app.wait_for("Claude · Sonnet").unwrap();
    app.press("role:button[Create]").unwrap();
    app.wait_for("Created session").unwrap();
}

/// The shell with its terminal open; it writes a line into the terminal after
/// the first frame, as the runtime's snapshot would.
#[component]
fn TerminalShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.terminal_open.set(true);
    let terminal_ref = host.terminal_ref;
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
    assert!(app.screen().shows("Ctrl+C"));
    app.press("role:button[Copy terminal]").unwrap();
    app.press("role:button[Find in terminal]").unwrap();
    app.type_text("text:Find in terminal", "hello").unwrap();
    app.wait_for("1 matches").unwrap();
}

#[test]
fn the_at_palette_inserts_a_workspace_reference() {
    let mut app = start();
    app.type_text("text:Message the Agent…", "@src").unwrap();
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
    app.press("main.rs").unwrap();
    app.wait_for("role:button[Save]").unwrap();
}

/// The shell with its Git surface open.
#[component]
fn GitShell() -> impl IntoElement {
    let host = host_with(None, None);
    host.git_open.set(true);
    session::session(host)
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
    for tool in ["Pull", "Push", "Amend", "Push after"] {
        assert!(app.screen().shows(tool), "the Git tools offer {tool}");
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
    app.press("Stage").unwrap();
    app.wait_for("Unstage").unwrap();
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
    app.press("role:button[History]").unwrap();
    app.wait_for("Initial commit").unwrap();
    app.type_text("text:Search commits", "zzz").unwrap();
    app.wait_for_gone("Initial commit", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn a_files_save_follows_the_edited_state() {
    let mut config = EngineConfig::new(Root::of::<FilesShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("README.md").unwrap();
    app.press("src").unwrap();
    app.wait_for("main.rs").unwrap();
    app.press("main.rs").unwrap();
    app.wait_for("role:button[Save]").unwrap();
    assert!(app.screen().enabled("role:button[Save]").unwrap());
}

#[test]
fn a_history_row_opens_the_commit_detail() {
    let mut config = EngineConfig::new(Root::of::<GitShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Commit").unwrap();
    app.press("role:button[History]").unwrap();
    app.wait_for("Initial commit").unwrap();
    app.press("Initial commit").unwrap();
    app.wait_for("commit detail body").unwrap();
}

#[test]
fn a_photo_can_be_attached_and_removed() {
    let mut app = start();
    app.press("role:button[Attach a photo]").unwrap();
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
        project_id: "p1".to_owned(),
        project_name: "vibex".to_owned(),
        workspace_id: "w1".to_owned(),
        workspace_label: "main".to_owned(),
    }]);
    session::session(host)
}

#[test]
fn a_session_can_be_renamed_from_the_drawer() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Sessions").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[Session actions]").unwrap();
    app.wait_for("role:button[Rename]").unwrap();
    app.type_text("text:Fix the tests", " v2").unwrap();
    app.press("role:button[Rename]").unwrap();
    app.wait_for("Fix the tests v2").unwrap();
}

#[test]
fn a_session_can_be_archived_from_the_drawer() {
    let mut config = EngineConfig::new(Root::of::<OneSessionShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Sessions").unwrap();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[Session actions]").unwrap();
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
    app.press("role:button[Session actions]").unwrap();
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
fn the_terminal_list_switches_between_terminals() {
    let mut config = EngineConfig::new(Root::of::<TerminalShell>());
    config.viewport = nsh_core::geometry::Size::new(px(390.0), px(1600.0));
    let mut app = Headless::start(config).unwrap();
    app.wait_for("role:button[Terminals]").unwrap();
    app.press("role:button[Terminals]").unwrap();
    app.wait_for("Build").unwrap();
    app.press("Build").unwrap();
    app.wait_until(
        "the title follows",
        std::time::Duration::from_secs(5),
        |screen| screen.texts().contains(&"Build"),
    )
    .unwrap();
}

#[test]
fn a_new_worktree_session_can_be_started_from_the_sheet() {
    let mut app = start();
    app.press("role:button[Sessions]").unwrap();
    app.press("role:button[New session]").unwrap();
    app.wait_for("text:Branch name").unwrap();
    app.type_text("text:Branch name", "feat/mobile").unwrap();
    app.press("role:button[Create worktree]").unwrap();
    app.wait_for("Worktree session").unwrap();
}

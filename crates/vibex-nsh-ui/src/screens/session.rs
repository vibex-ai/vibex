//! The session shell: the drawer of sessions, the timeline and the composer.
//!
//! The compact phone composition the GPUI client uses, drawn with `nsh`: a
//! top bar that opens the session drawer, the transcript as a virtualized list
//! of rows, and a composer pinned above the keyboard. The app's session service
//! owns the data and the actions; this module binds them and decides how a row
//! looks.
//!
//! One row per timeline projection (`vibex-desktop-model`'s `TimelineRow`):
//! user messages as bubbles, Agent messages as prose, tools and notices as
//! cards, errors in the destructive tone. Markdown is still plain text here;
//! the `TextView` renderer replaces it once the row shapes settle.

use std::rc::Rc;

use nsh_base::{
    ButtonElement, SheetEvent, button, sheet,
    text_view::{MarkdownOptions, text_view},
};
use nsh_core::{
    element::{ImageSrc, Position},
    prelude::*,
};

use crate::{
    copy,
    screens::connecting::{self as connecting_screen, ConnectionState},
    theme,
    ui::{action, brand, field},
};

/// The height of the top bar, above the safe area.
const HEADER_HEIGHT: f32 = 48.0;
/// The drawer's width on a phone.
const DRAWER_WIDTH: f32 = 300.0;

/// How a session is doing, for the drawer's dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// The Agent is working.
    Running,
    /// Waiting for a person: a question or a permission.
    Waiting,
    /// Idle.
    Idle,
    /// The last turn failed.
    Failed,
}

impl SessionState {
    fn dot(self) -> Hsla {
        match self {
            SessionState::Running => theme::color_token("chart-1"),
            SessionState::Waiting => theme::color_token("warning"),
            SessionState::Idle => theme::muted_foreground(),
            SessionState::Failed => theme::accent_red(),
        }
    }
}

/// A session in the drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// The Agent's name.
    pub agent: String,
    /// When it last spoke, already formatted ("2m", "14:05").
    pub meta: String,
    pub state: SessionState,
}

/// What kind of timeline row this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// What the person wrote.
    User,
    /// What the Agent answered.
    Agent,
    /// The Agent's reasoning.
    Reasoning,
    /// A tool call or command.
    Tool,
    /// A system or Git notice.
    Notice,
    /// A failure.
    Error,
}

/// One row of the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEntry {
    pub id: String,
    pub kind: EntryKind,
    /// A tool's name, the Agent's name, a row's heading.
    pub title: String,
    /// The text itself.
    pub body: String,
    /// A short status ("done", "running", a path).
    pub meta: String,
    pub streaming: bool,
}

/// The answer a permission card offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionChoice {
    /// Allow this one action.
    Allow,
    /// Allow this kind of action for the session.
    AlwaysAllow,
    /// Refuse it.
    Deny,
}

/// Answers a permission card.
type PermissionAction = Rc<dyn Fn(&str, PermissionChoice)>;

/// One button of a permission card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub choice: PermissionChoice,
}

/// A permission the Agent is waiting on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingPermission {
    pub request_id: String,
    pub title: String,
    /// What the action would do, one line each.
    pub details: Vec<String>,
    pub options: Vec<PermissionOption>,
}

/// The answer to one question field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionAnswer {
    /// Text, a number, or a chosen option's value.
    Text(String),
    /// A yes/no field.
    Boolean(bool),
    /// The values chosen in a multi-select field.
    Texts(Vec<String>),
}

/// One field of a question the Agent asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionField {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: QuestionFieldKind,
}

/// What a question field shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionFieldKind {
    /// A text field (a number is typed here too).
    Text { default: Option<String> },
    /// A choice among options.
    Choice {
        options: Vec<QuestionOption>,
        default: Option<String>,
    },
    /// Yes or no.
    Boolean { default: Option<bool> },
    /// Any number of the options.
    Multi {
        options: Vec<QuestionOption>,
        default: Vec<String>,
    },
}

/// One option of a choice field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    pub value: String,
    pub label: String,
}

/// A question the Agent is waiting on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    pub request_id: String,
    pub title: String,
    pub message: String,
    pub fields: Vec<QuestionField>,
}

/// What the person submitted for a question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionResponse {
    /// Every field's answer, in field order.
    Submit(Vec<(String, QuestionAnswer)>),
    /// Leave the question unanswered.
    Decline,
}

/// Answers a question card.
type QuestionAction = Rc<dyn Fn(&str, QuestionResponse)>;

/// What the person typed into the terminal.
type TerminalInput = Rc<dyn Fn(&str)>;

/// A file or photo waiting to be sent with the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentItem {
    pub id: String,
    pub label: String,
    pub mime_type: String,
    /// A `data:` URL the runtime materializes.
    pub uri: String,
}

/// Opens a path (a directory or a file) in the files surface.
type OpenPath = Rc<dyn Fn(&str)>;

/// Opens a path's diff, staged or not.
type OpenDiff = Rc<dyn Fn(&str, bool)>;

/// Opens the actions sheet for a session, with its id and title.
type SessionMenu = Rc<dyn Fn(&str, &str)>;

use crate::screens::new_session::{
    NewSessionHost, RuntimeChoice, WorkspaceChoice, new_session as new_session_sheet,
};

/// A callback with no arguments.
type Action = Rc<dyn Fn()>;

/// One entry of the composer's `/` palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    /// What pressing it puts in the composer.
    pub insertion: String,
}

/// Asks for the commands matching what follows `/`.
type CommandQuery = Rc<dyn Fn(&str)>;

/// Asks for the references matching what follows `@`.
type MentionQuery = Rc<dyn Fn(&str)>;

/// Which prefix opened the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteKind {
    Slash,
    Mention,
}

/// What the shell may do, and the state it shows.
#[derive(Clone)]
pub struct SessionHost {
    pub sessions: Signal<Vec<SessionSummary>>,
    pub active: Signal<Option<String>>,
    pub timeline: Signal<Vec<TimelineEntry>>,
    pub draft: Signal<String>,
    pub sending: Signal<bool>,
    pub error: Signal<Option<SharedString>>,
    /// Where the connection to the runtime stands.
    pub connection: Signal<ConnectionState>,
    /// Why the last connection attempt failed, when it did.
    pub connection_error: Signal<Option<SharedString>>,
    /// Asks the service to reconnect now.
    pub on_retry: Action,
    /// The connected runtime's label, shown in the drawer.
    pub runtime: Signal<Option<SharedString>>,
    /// The permission the Agent waits on, shown above the composer.
    pub pending: Signal<Option<PendingPermission>>,
    /// The question the Agent waits on, shown above the composer.
    pub question: Signal<Option<PendingQuestion>>,
    /// An answer to either is in flight.
    pub resolving: Signal<bool>,
    pub on_select: Rc<dyn Fn(&str)>,
    pub on_send: Rc<dyn Fn(&str)>,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_disconnect: Rc<dyn Fn()>,
    pub on_permission: PermissionAction,
    pub on_question: QuestionAction,
    /// The `/` commands the session offers, filtered by what is typed.
    pub commands: Signal<Vec<CommandEntry>>,
    pub on_command_query: CommandQuery,
    /// The `@` references the session offers.
    pub mentions: Signal<Vec<CommandEntry>>,
    pub on_mention_query: MentionQuery,
    /// Files and photos waiting to be sent with the message.
    pub attachments: Signal<Vec<AttachmentItem>>,
    pub on_attach_image: Action,
    pub on_attach_file: Action,
    pub on_remove_attachment: Rc<dyn Fn(&str)>,
    /// Starts a new session in the open session's workspace.
    pub on_new_session: Action,
    /// Opens the settings sheet over the shell.
    pub on_open_settings: Action,
    /// The terminal surface, when it is open over the shell.
    pub terminal_open: Signal<bool>,
    pub terminal_title: Signal<Option<SharedString>>,
    pub terminal_error: Signal<Option<SharedString>>,
    pub terminal_ref: ComponentRef<nsh_base::Terminal>,
    pub on_open_terminal: Action,
    pub on_close_terminal: Action,
    pub on_terminal_data: TerminalInput,
    pub on_terminal_resize: Rc<dyn Fn(u32, u32)>,
    pub on_terminal_title: Rc<dyn Fn(&str)>,
    pub terminal_choices: Signal<Vec<crate::screens::terminal::TerminalChoice>>,
    pub terminal_menu: Signal<bool>,
    pub on_terminal_menu: Action,
    pub on_close_terminal_menu: Action,
    pub on_select_terminal: Rc<dyn Fn(&str)>,
    pub on_new_terminal: Action,
    /// Stops the active session's turn.
    pub on_interrupt: Action,
    /// Opens the runtime switcher over the live session.
    pub on_switch_runtime: Action,
    /// The files surface, when it is open over the shell.
    pub files_open: Signal<bool>,
    pub files_path: Signal<String>,
    pub files_entries: Signal<Vec<crate::screens::files::FileEntry>>,
    pub files_preview: Signal<Option<crate::screens::files::FilePreview>>,
    pub files_error: Signal<Option<SharedString>>,
    pub on_open_files: Action,
    pub on_close_files: Action,
    pub on_open_dir: OpenPath,
    pub on_open_file: OpenPath,
    pub on_files_back: Action,
    pub files_content: Signal<String>,
    pub files_dirty: Signal<bool>,
    pub files_saving: Signal<bool>,
    pub on_save_file: Action,
    /// The new-session sheet, and what it offers.
    pub new_session_open: Signal<bool>,
    pub new_workspaces: Signal<Vec<WorkspaceChoice>>,
    pub new_runtimes: Signal<Vec<RuntimeChoice>>,
    pub new_workspace: Signal<Option<String>>,
    pub new_runtime: Signal<Option<String>>,
    pub new_busy: Signal<bool>,
    pub new_error: Signal<Option<SharedString>>,
    pub on_create_session: Action,
    pub new_worktree_branch: Signal<String>,
    pub on_create_worktree_session: Action,
    pub on_close_new_session: Action,
    /// The id of the session whose actions are open, if any.
    pub session_actions: Signal<Option<String>>,
    /// The title being edited in that sheet.
    pub session_rename: Signal<String>,
    pub session_busy: Signal<bool>,
    pub on_session_menu: SessionMenu,
    pub on_close_session_actions: Action,
    pub on_rename_session: Action,
    pub on_archive_session: Action,
    pub on_delete_session: Action,
    /// The Git surface, when it is open over the shell.
    pub git_open: Signal<bool>,
    pub git_branch: Signal<Option<SharedString>>,
    pub git_changes: Signal<Vec<crate::screens::git::GitChangeRow>>,
    pub git_diff: Signal<Option<crate::screens::git::GitDiffPreview>>,
    pub git_error: Signal<Option<SharedString>>,
    pub on_open_git: Action,
    pub on_close_git: Action,
    pub on_open_git_diff: OpenDiff,
    pub on_stage_git: OpenPath,
    pub on_unstage_git: OpenPath,
    pub git_staged: Signal<u32>,
    pub git_message: Signal<String>,
    pub git_committing: Signal<bool>,
    pub git_history: Signal<Vec<crate::screens::git::CommitRow>>,
    pub git_show_history: Signal<bool>,
    pub git_commit_detail: Signal<Option<crate::screens::git::CommitDetailPreview>>,
    pub on_commit_git: Action,
    pub on_toggle_git_history: Action,
    pub on_open_git_commit: OpenPath,
    pub on_revert_git: OpenPath,
    pub on_git_back: Action,
    pub on_refresh_git: Action,
}

/// The session shell, or the terminal or files surface over it.
///
/// A connection that is not up yet (or came back down) shows the connecting
/// page while there is no transcript to hold on to; once a transcript exists,
/// the shell stays and a banner says the connection is being restored.
pub fn session(host: SessionHost) -> impl IntoElement {
    let connection = host.connection;
    let timeline = host.timeline;
    let error = host.connection_error;
    let on_retry = host.on_retry.clone();
    let on_switch_runtime = host.on_switch_runtime.clone();
    let on_disconnect = host.on_disconnect.clone();
    let shell_host = host.clone();
    view().flex_1().bg(theme::background).child(
        when(
            move || {
                connection.get() != ConnectionState::Online
                    && timeline.with(|timeline| timeline.is_empty())
            },
            move || {
                connecting_screen::connecting(connecting_screen::ConnectingHost {
                    state: connection,
                    error,
                    on_retry: on_retry.clone(),
                    on_switch_runtime: on_switch_runtime.clone(),
                    on_disconnect: on_disconnect.clone(),
                })
            },
        )
        .otherwise(move || surfaces(shell_host.clone())),
    )
}

/// The shell and the full-screen surfaces it can open over itself.
fn surfaces(host: SessionHost) -> impl IntoElement {
    let terminal_open = host.terminal_open;
    let files_open = host.files_open;
    let git_open = host.git_open;
    let terminal = crate::screens::terminal::TerminalHost {
        title: host.terminal_title,
        error: host.terminal_error,
        term_ref: host.terminal_ref,
        on_data: host.on_terminal_data.clone(),
        on_resize: host.on_terminal_resize.clone(),
        on_title: host.on_terminal_title.clone(),
        terminals: host.terminal_choices,
        terminal_menu: host.terminal_menu,
        on_open_menu: host.on_terminal_menu.clone(),
        on_close_menu: host.on_close_terminal_menu.clone(),
        on_select_terminal: host.on_select_terminal.clone(),
        on_new_terminal: host.on_new_terminal.clone(),
        on_close: host.on_close_terminal.clone(),
    };
    let files = crate::screens::files::FilesHost {
        open: host.files_open,
        path: host.files_path,
        entries: host.files_entries,
        file: host.files_preview,
        error: host.files_error,
        on_open_dir: host.on_open_dir.clone(),
        on_open_file: host.on_open_file.clone(),
        on_back: host.on_files_back.clone(),
        on_close: host.on_close_files.clone(),
        content: host.files_content,
        dirty: host.files_dirty,
        saving: host.files_saving,
        on_save: host.on_save_file.clone(),
    };
    let git = crate::screens::git::GitHost {
        open: host.git_open,
        branch: host.git_branch,
        changes: host.git_changes,
        diff: host.git_diff,
        error: host.git_error,
        on_open_diff: host.on_open_git_diff.clone(),
        on_stage: host.on_stage_git.clone(),
        on_unstage: host.on_unstage_git.clone(),
        staged: host.git_staged,
        message: host.git_message,
        committing: host.git_committing,
        history: host.git_history,
        show_history: host.git_show_history,
        commit_detail: host.git_commit_detail,
        on_open_commit: host.on_open_git_commit.clone(),
        on_revert: host.on_revert_git.clone(),
        on_commit: host.on_commit_git.clone(),
        on_toggle_history: host.on_toggle_git_history.clone(),
        on_back: host.on_git_back.clone(),
        on_refresh: host.on_refresh_git.clone(),
        on_close: host.on_close_git.clone(),
    };
    let shell_host = host;
    view()
        .flex_1()
        .bg(theme::background)
        .child(when(move || git_open.get(), {
            let git = git.clone();
            move || crate::screens::git::git(git.clone())
        }))
        .child(when(move || !git_open.get() && files_open.get(), {
            let files = files.clone();
            move || crate::screens::files::files(files.clone())
        }))
        .child(when(
            move || !git_open.get() && !files_open.get() && terminal_open.get(),
            {
                let terminal = terminal.clone();
                move || crate::screens::terminal::terminal(terminal.clone())
            },
        ))
        .child(when(
            move || !git_open.get() && !files_open.get() && !terminal_open.get(),
            move || shell(shell_host.clone()),
        ))
}

/// The shell proper: header, transcript, cards, composer and drawer.
fn shell(host: SessionHost) -> impl IntoElement {
    let drawer_open = signal(false);
    let connection = host.connection;
    let connection_error = host.connection_error;
    let on_retry = host.on_retry.clone();

    view()
        .flex_1()
        .bg(theme::background)
        .flex_col()
        .child(header(host.clone(), drawer_open))
        .child(when(
            move || connection.get() != ConnectionState::Online,
            move || connection_banner(connection, connection_error, on_retry.clone()),
        ))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .child(timeline_view(host.clone()))
                .child(when(move || host.error.get().is_some(), {
                    let error = host.error;
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red)
                            .px(px(theme::SPACING_LG))
                            .pb(px(theme::SPACING_SM))
                    }
                })),
        )
        .child(when(
            move || {
                host.draft.with(|draft| draft.starts_with('/'))
                    && !host.commands.with(|commands| commands.is_empty())
            },
            {
                let commands = host.commands;
                let draft = host.draft;
                move || command_palette(commands, draft, PaletteKind::Slash)
            },
        ))
        .child(when(
            move || {
                mention_query(&host.draft.get()).is_some()
                    && !host.mentions.with(|mentions| mentions.is_empty())
            },
            {
                let mentions = host.mentions;
                let draft = host.draft;
                move || command_palette(mentions, draft, PaletteKind::Mention)
            },
        ))
        .child(when(move || host.pending.get().is_some(), {
            let pending = host.pending;
            let resolving = host.resolving;
            let on_permission = host.on_permission.clone();
            move || permission_card(pending, resolving, on_permission.clone())
        }))
        .child(when(move || host.question.get().is_some(), {
            let question = host.question;
            let resolving = host.resolving;
            let on_question = host.on_question.clone();
            move || question_card(question, resolving, on_question.clone())
        }))
        .child(composer(host.clone()))
        .child(drawer(host.clone(), drawer_open))
        .child(session_actions_sheet(
            host.session_actions,
            host.session_rename,
            host.session_busy,
            host.on_close_session_actions.clone(),
            host.on_rename_session.clone(),
            host.on_archive_session.clone(),
            host.on_delete_session.clone(),
        ))
        .child(new_session_sheet(NewSessionHost {
            open: host.new_session_open,
            workspaces: host.new_workspaces,
            runtimes: host.new_runtimes,
            workspace: host.new_workspace,
            runtime: host.new_runtime,
            busy: host.new_busy,
            error: host.new_error,
            on_close: host.on_close_new_session.clone(),
            on_create: host.on_create_session.clone(),
            worktree_branch: host.new_worktree_branch,
            on_create_worktree: host.on_create_worktree_session.clone(),
        }))
}

/// The top bar: the drawer, the title, a refresh.
fn header(host: SessionHost, drawer_open: Signal<bool>) -> impl IntoElement {
    let sessions = host.sessions;
    let active = host.active;
    let refresh = host.on_refresh.clone();
    view()
        .w_full()
        .pt_safe_area()
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .child(
            view()
                .w_full()
                .h(px(HEADER_HEIGHT))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .px(px(theme::SPACING_SM))
                .child(action::icon_button(
                    icon(include_str!("../../assets/icons/menu.svg")),
                    copy::text("Sessions", "会话", "工作階段"),
                    move || drawer_open.set(true),
                ))
                .child(
                    text(move || {
                        let active = active.get();
                        sessions
                            .with(|sessions| {
                                active.and_then(|active| {
                                    sessions
                                        .iter()
                                        .find(|session| session.id == active)
                                        .map(|session| session.title.clone())
                                })
                            })
                            .unwrap_or_else(|| "Vibex".to_owned())
                    })
                    .flex_1()
                    .truncate()
                    .text_size(px(theme::font_heading()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::foreground),
                )
                .child(action::icon_button(
                    icon(include_str!("../../assets/icons/refresh.svg")),
                    copy::text("Refresh", "刷新", "重新整理"),
                    move || refresh(),
                )),
        )
}

/// The strip under the header while the connection is being restored.
///
/// A transcript already on screen is the reader's, so a drop never replaces
/// it with the connecting page; the strip says what happened and offers the
/// retry the page would have.
fn connection_banner(
    connection: Signal<ConnectionState>,
    error: Signal<Option<SharedString>>,
    on_retry: Action,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_XS))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            view()
                .w(px(8.0))
                .h(px(8.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .bg(move || match connection.get() {
                    ConnectionState::Connecting => theme::color_token("warning"),
                    ConnectionState::Online => theme::color_token("chart-1"),
                    ConnectionState::Offline => theme::accent_red(),
                }),
        )
        .child(
            text(move || {
                error.get().unwrap_or_else(|| {
                    match connection.get() {
                        ConnectionState::Connecting => {
                            copy::text("Reconnecting...", "正在重新连接…", "正在重新連線…")
                        }
                        ConnectionState::Online => copy::text("Connected", "已连接", "已連線"),
                        ConnectionState::Offline => {
                            copy::text("Desktop is unavailable", "桌面端不可用", "桌面版無法使用")
                        }
                    }
                    .into()
                })
            })
            .flex_1()
            .truncate()
            .text_size(px(theme::font_caption()))
            .text_color(theme::text_secondary),
        )
        .child(
            action::secondary(
                copy::text("Retry", "重试", "重試"),
                icon(include_str!("../../assets/icons/refresh.svg")),
                false,
                move || on_retry(),
            )
            .disabled(move || connection.get() == ConnectionState::Connecting)
            .disabled_style(|style| style.opacity(0.4)),
        )
}

/// The transcript: a row per entry, or what to do when there is none.
fn timeline_view(host: SessionHost) -> impl IntoElement {
    let timeline = host.timeline;
    let active = host.active;
    let error = host.error;
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(when(
            move || active.get().is_none(),
            move || {
                empty_hint(copy::text(
                    "Choose a session",
                    "选择一个会话",
                    "選擇一個工作階段",
                ))
            },
        ))
        .child(when(
            move || active.get().is_some() && timeline.with(|entries| entries.is_empty()),
            move || empty_hint(copy::text("No messages yet", "还没有消息", "還沒有訊息")),
        ))
        .child(when(
            move || active.get().is_some() && error.get().is_some(),
            move || {
                view().flex_1().items_center().justify_center().child(
                    text(move || error.get().unwrap_or_default())
                        .text_size(px(theme::font_body()))
                        .text_color(theme::accent_red),
                )
            },
        ))
        .child(when(
            move || timeline.with(|entries| !entries.is_empty()),
            move || {
                virtual_list(
                    move || timeline.with(|entries| entries.len()),
                    move |at: Position| entry_row(at, timeline),
                )
                .keys(move || {
                    timeline.with(|entries| {
                        entries
                            .iter()
                            .map(|entry| entry.id.clone())
                            .collect::<Vec<_>>()
                    })
                })
                .estimated_item_size(px(72.0))
                // A chat opens at its newest message and stays there while a
                // reply streams in, unless the reader has scrolled up.
                .alignment(ListAlignment::Bottom)
                .follow_end(px(120.0))
                .flex_1()
            },
        ))
}

fn empty_hint(message: &'static str) -> impl IntoElement {
    view().flex_1().items_center().justify_center().child(
        text(message)
            .text_size(px(theme::font_detail()))
            .text_color(theme::text_muted),
    )
}

/// One timeline row, shaped by its kind.
///
/// The row is made once and reused by the list, so each shape is a region
/// bound to the row's kind: when a cell shows another kind, its branch swaps
/// without rebuilding the cell.
fn entry_row(at: Position, entries: Signal<Vec<TimelineEntry>>) -> impl IntoElement {
    let entry = computed(move || entries.with(|entries| entries.get(at.index()).cloned()));
    view()
        .w_full()
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .child(entry_element(entry))
}

fn entry_element(entry: Memo<Option<TimelineEntry>>) -> impl IntoElement {
    let kind = computed(move || entry.with(|e| e.as_ref().map(|e| e.kind)));
    let body =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.body.clone()).unwrap_or_default()));
    let title =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.title.clone()).unwrap_or_default()));
    let meta =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.meta.clone()).unwrap_or_default()));
    let streaming = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.streaming)));

    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::User)),
            move || user_bubble(body),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Agent)),
            move || agent_prose(title, body, streaming),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Reasoning)),
            move || muted_prose(copy::text("Thought", "思考", "思考"), body),
        ))
        .child(when(
            move || kind.with(|kind| matches!(kind, Some(EntryKind::Tool | EntryKind::Notice))),
            move || tool_card(title, body, meta),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Error)),
            move || error_card(body),
        ))
}

fn user_bubble(body: Memo<String>) -> impl IntoElement {
    view().w_full().flex_row().justify_end().child(
        view()
            .max_w(px(320.0))
            .rounded(px(14.0))
            .bg(theme::background_dim)
            .px(px(theme::SPACING_MD))
            .py(px(theme::SPACING_SM))
            .child(
                text(move || body.get())
                    .text_size(px(theme::font_body()))
                    .line_height(theme::line_height_for(theme::font_body()))
                    .text_color(theme::foreground),
            ),
    )
}

fn agent_prose(title: Memo<String>, body: Memo<String>, streaming: Memo<bool>) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .child(
                    text(move || title.get())
                        .text_size(px(theme::font_micro()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_muted),
                )
                .child(when(
                    move || streaming.get(),
                    || {
                        text("…")
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted)
                    },
                )),
        )
        .child(
            // Markdown, with the open syntax of a still-arriving reply repaired
            // as it streams. `nsh` lays the blocks out natively and only shows
            // the blocks that changed again.
            text_view()
                .markdown(move || Some(body.get().into()))
                .options(move || MarkdownOptions {
                    streaming: streaming.get(),
                    ..MarkdownOptions::default()
                })
                .selectable(true),
        )
}

fn muted_prose(label: &'static str, body: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::font_detail()))
                .line_height(theme::line_height_for(theme::font_detail()))
                .text_color(theme::text_secondary),
        )
}

fn tool_card(title: Memo<String>, body: Memo<String>, meta: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .p(px(theme::SPACING_MD))
        .gap(px(theme::SPACING_XS))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || title.get())
                        .flex_1()
                        .truncate()
                        .text_size(px(theme::font_detail()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::foreground),
                )
                .child(
                    text(move || meta.get())
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                ),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::font_caption()))
                .line_height(theme::line_height_for(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
}

fn error_card(body: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::accent_red)
        .p(px(theme::SPACING_MD))
        .child(
            text(move || body.get())
                .text_size(px(theme::font_detail()))
                .text_color(theme::accent_red),
        )
}

/// The `/` command palette, above the composer.
///
/// The list is what the session's runtime offers, narrowed by the text after
/// the slash; pressing a row writes its insertion into the composer.
fn command_palette(
    commands: Signal<Vec<CommandEntry>>,
    draft: Signal<String>,
    kind: PaletteKind,
) -> impl IntoElement {
    view()
        .w_full()
        .max_h(px(240.0))
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            scroll()
                .max_h(px(240.0))
                .child(view().flex_col().p(px(theme::SPACING_XS)).child(each(
                    move || {
                        let query = draft.with(|draft| {
                            draft
                                .strip_prefix('/')
                                .unwrap_or_default()
                                .trim()
                                .to_lowercase()
                        });
                        commands.with(|commands| {
                            commands
                                .iter()
                                .filter(|command| {
                                    query.is_empty()
                                        || command.label.to_lowercase().contains(&query)
                                        || command.insertion.to_lowercase().contains(&query)
                                })
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                    },
                    |command| command.id.clone(),
                    move |command, _| command_row(command, draft, kind),
                ))),
        )
}

fn command_row(
    command: ReadSignal<CommandEntry>,
    draft: Signal<String>,
    kind: PaletteKind,
) -> impl IntoElement {
    button()
        .on_press(move || {
            let insertion = command.get_untracked().insertion;
            draft.set(match kind {
                PaletteKind::Slash => insertion,
                PaletteKind::Mention => insert_mention(&draft.get_untracked(), &insertion),
            });
        })
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .px(px(theme::SPACING_SM))
        .py(px(theme::SPACING_XS))
        .flex_col()
        .items_start()
        .active(|style| style.opacity(0.85))
        .child(
            text(move || command.get().label)
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::foreground),
        )
        .child(
            text(move || command.get().description)
                .truncate()
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
}

/// The permission card, above the composer.
///
/// The Agent is blocked while this shows, so it is a first-class action, not a
/// timeline row: the title says what is being asked, the lines say what it
/// would do, and every answer the runtime allows is a button.
fn permission_card(
    pending: Signal<Option<PendingPermission>>,
    resolving: Signal<bool>,
    on_permission: PermissionAction,
) -> impl IntoElement {
    let request = pending.get_untracked().expect("shown while pending");
    let request_id = request.request_id.clone();
    let buttons = request.options.into_iter().map({
        let on_permission = on_permission.clone();
        let request_id = request_id.clone();
        move |option| {
            let id = request_id.clone();
            let choice = option.choice;
            let on_permission = on_permission.clone();
            let button = if choice == PermissionChoice::Allow {
                action::primary(
                    option.label,
                    icon(include_str!("../../assets/icons/check.svg")),
                    move || on_permission(&id, choice),
                )
            } else {
                action::secondary(
                    option.label,
                    icon(include_str!("../../assets/icons/x.svg")),
                    false,
                    move || on_permission(&id, choice),
                )
            };
            button
                .w_full()
                .disabled(move || resolving.get())
                .disabled_style(|style| style.opacity(0.5))
        }
    });
    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_MD))
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            text(request.title)
                .text_size(px(theme::font_body()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
        .children(request.details.into_iter().map(|detail| {
            text(detail)
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary)
        }))
        .child(
            view()
                .flex_col()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .pt(px(theme::SPACING_XS))
                .children(buttons),
        )
}

/// The question card, above the composer.
///
/// A question carries typed fields; the card draws a field per kind (a text
/// field, a row of choices, yes/no), checks the required ones, and submits or
/// declines. The Agent is blocked while it shows.
fn question_card(
    question: Signal<Option<PendingQuestion>>,
    resolving: Signal<bool>,
    on_question: QuestionAction,
) -> impl IntoElement {
    let request = question.get_untracked().expect("shown while pending");
    let request_id = request.request_id;
    let mut elements = Vec::new();
    let mut texts: Vec<(String, Signal<String>)> = Vec::new();
    let mut choices: Vec<(String, Signal<Option<String>>)> = Vec::new();
    let mut booleans: Vec<(String, Signal<Option<bool>>)> = Vec::new();
    let mut multis: Vec<(String, Signal<Vec<String>>)> = Vec::new();
    let mut required: Vec<(String, QuestionFieldKind)> = Vec::new();

    for field in request.fields {
        let QuestionField {
            id,
            title,
            description,
            required: is_required,
            kind,
        } = field;
        if is_required {
            required.push((id.clone(), kind.clone()));
        }
        let placeholder = title.clone();
        let mut column = view().w_full().gap(px(2.0)).child(
            text(title)
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::foreground),
        );
        if let Some(description) = description {
            column = column.child(
                text(description)
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::text_muted),
            );
        }
        match kind {
            QuestionFieldKind::Text { default } => {
                let value = signal(default.unwrap_or_default());
                texts.push((id.clone(), value));
                column = column.child(field::text_field(value, move || placeholder.clone()));
            }
            QuestionFieldKind::Choice { options, default } => {
                let selected = signal(default);
                choices.push((id.clone(), selected));
                let rows = options
                    .into_iter()
                    .map(move |option| choice_button(option.label, option.value, selected));
                column = column.child(view().flex_col().gap(px(theme::SPACING_XS)).children(rows));
            }
            QuestionFieldKind::Boolean { default } => {
                let selected = signal(default);
                booleans.push((id.clone(), selected));
                let yes = boolean_button(copy::text("Yes", "是", "是").to_owned(), true, selected);
                let no = boolean_button(copy::text("No", "否", "否").to_owned(), false, selected);
                column = column.child(
                    view()
                        .flex_row()
                        .gap(px(theme::SPACING_SM))
                        .child(yes)
                        .child(no),
                );
            }
            QuestionFieldKind::Multi { options, default } => {
                let chosen = signal(default);
                multis.push((id.clone(), chosen));
                let rows = options
                    .into_iter()
                    .map(move |option| multi_button(option.label, option.value, chosen));
                column = column.child(view().flex_col().gap(px(theme::SPACING_XS)).children(rows));
            }
        }
        elements.push(column.into_element());
    }

    let error = signal(None::<&'static str>);
    let texts_for_submit = texts.clone();
    let choices_for_submit = choices.clone();
    let booleans_for_submit = booleans.clone();
    let multis_for_submit = multis.clone();
    let required_for_submit = required.clone();
    let submit_request = request_id.clone();
    let submit = {
        let on_question = on_question.clone();
        move || {
            let mut answers = Vec::new();
            for (id, value) in &texts_for_submit {
                let text = value.get_untracked();
                if required_for_submit
                    .iter()
                    .any(|(required_id, _)| required_id == id)
                    && text.trim().is_empty()
                {
                    error.set(Some("Fill in the required fields."));
                    return;
                }
                answers.push((id.clone(), QuestionAnswer::Text(text)));
            }
            for (id, value) in &choices_for_submit {
                let Some(value) = value.get_untracked() else {
                    if required_for_submit
                        .iter()
                        .any(|(required_id, _)| required_id == id)
                    {
                        error.set(Some("Choose an option for the required fields."));
                        return;
                    }
                    continue;
                };
                answers.push((id.clone(), QuestionAnswer::Text(value)));
            }
            for (id, value) in &multis_for_submit {
                let values = value.get_untracked();
                if required_for_submit
                    .iter()
                    .any(|(required_id, _)| required_id == id)
                    && values.is_empty()
                {
                    error.set(Some("Choose at least one option for the required fields."));
                    return;
                }
                if !values.is_empty() {
                    answers.push((id.clone(), QuestionAnswer::Texts(values)));
                }
            }
            for (id, value) in &booleans_for_submit {
                let Some(value) = value.get_untracked() else {
                    if required_for_submit
                        .iter()
                        .any(|(required_id, _)| required_id == id)
                    {
                        error.set(Some("Answer the required fields."));
                        return;
                    }
                    continue;
                };
                answers.push((id.clone(), QuestionAnswer::Boolean(value)));
            }
            error.set(None);
            on_question(&submit_request, QuestionResponse::Submit(answers));
        }
    };
    let decline = {
        let on_question = on_question.clone();
        let request_id = request_id.clone();
        move || on_question(&request_id, QuestionResponse::Decline)
    };

    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_MD))
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            text(request.title)
                .text_size(px(theme::font_body()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
        .child(
            text(request.message)
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
        // A question can carry more fields than a phone's card can show at
        // once; the fields scroll while the title and the submit stay put.
        .child(
            scroll().max_h(px(280.0)).child(
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(theme::SPACING_SM))
                    .children(elements),
            ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::accent_red)
            },
        ))
        .child(
            view()
                .flex_row()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .pt(px(theme::SPACING_XS))
                .child(
                    action::primary(
                        copy::text("Submit", "提交", "提交"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        submit,
                    )
                    .flex_1()
                    .disabled(move || resolving.get())
                    .disabled_style(|style| style.opacity(0.5)),
                )
                .child(
                    action::secondary(
                        copy::text("Decline", "拒绝", "拒絕"),
                        icon(include_str!("../../assets/icons/x.svg")),
                        false,
                        decline,
                    )
                    .flex_1()
                    .disabled(move || resolving.get())
                    .disabled_style(|style| style.opacity(0.5)),
                ),
        )
}

/// One selectable row of a choice field.
fn choice_button(label: String, value: String, selected: Signal<Option<String>>) -> ButtonElement {
    let pressed = value.clone();
    let border_value = value.clone();
    let fill_value = value.clone();
    let text_value = value;
    button()
        .on_press(move || selected.set(Some(pressed.clone())))
        .w_full()
        .min_h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(move || {
            if selected.with(|selected| selected.as_deref() == Some(border_value.as_str())) {
                theme::foreground()
            } else {
                theme::border_subtle()
            }
        })
        .bg(move || {
            if selected.with(|selected| selected.as_deref() == Some(fill_value.as_str())) {
                theme::background_card()
            } else {
                theme::background_dim()
            }
        })
        .items_center()
        .justify_center()
        .child(
            text(label)
                .text_size(px(theme::font_detail()))
                .text_color(move || {
                    if selected.with(|selected| selected.as_deref() == Some(text_value.as_str())) {
                        theme::foreground()
                    } else {
                        theme::text_secondary()
                    }
                }),
        )
}

/// One toggle row of a multi-select field.
fn multi_button(label: String, value: String, chosen: Signal<Vec<String>>) -> ButtonElement {
    let pressed = value.clone();
    let border_value = value.clone();
    let fill_value = value.clone();
    let text_value = value;
    button()
        .on_press(move || {
            chosen.update(|chosen| {
                if let Some(index) = chosen.iter().position(|item| item == &pressed) {
                    chosen.remove(index);
                } else {
                    chosen.push(pressed.clone());
                }
            });
        })
        .w_full()
        .min_h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(move || {
            if chosen.with(|chosen| chosen.contains(&border_value)) {
                theme::foreground()
            } else {
                theme::border_subtle()
            }
        })
        .bg(move || {
            if chosen.with(|chosen| chosen.contains(&fill_value)) {
                theme::background_card()
            } else {
                theme::background_dim()
            }
        })
        .items_center()
        .justify_center()
        .child(
            text(label)
                .text_size(px(theme::font_detail()))
                .text_color(move || {
                    if chosen.with(|chosen| chosen.contains(&text_value)) {
                        theme::foreground()
                    } else {
                        theme::text_secondary()
                    }
                }),
        )
}

/// One side of a yes/no field.
fn boolean_button(label: String, value: bool, selected: Signal<Option<bool>>) -> ButtonElement {
    button()
        .on_press(move || selected.set(Some(value)))
        .flex_1()
        .min_h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(move || {
            if selected.with(|selected| *selected == Some(value)) {
                theme::foreground()
            } else {
                theme::border_subtle()
            }
        })
        .bg(move || {
            if selected.with(|selected| *selected == Some(value)) {
                theme::background_card()
            } else {
                theme::background_dim()
            }
        })
        .items_center()
        .justify_center()
        .child(
            text(label)
                .text_size(px(theme::font_detail()))
                .text_color(move || {
                    if selected.with(|selected| *selected == Some(value)) {
                        theme::foreground()
                    } else {
                        theme::text_secondary()
                    }
                }),
        )
}

/// The composer, kept above the software keyboard.
fn composer(host: SessionHost) -> impl IntoElement {
    let draft = host.draft;
    let sending = host.sending;
    let send = host.on_send.clone();
    let commands = host.commands;
    let mentions = host.mentions;
    let on_query = host.on_command_query.clone();
    let on_mention = host.on_mention_query.clone();
    let on_interrupt = host.on_interrupt.clone();
    let sessions = host.sessions;
    let active = host.active;
    let attachments = host.attachments;
    let on_remove_attachment = host.on_remove_attachment.clone();
    let running = computed(move || {
        let Some(active) = active.get() else {
            return false;
        };
        sessions.with(|sessions| {
            sessions
                .iter()
                .find(|session| session.id == active)
                .is_some_and(|session| session.state == SessionState::Running)
        })
    });
    let submit: Rc<dyn Fn()> = {
        let send = send.clone();
        Rc::new(move || {
            if sending.get_untracked() {
                return;
            }
            let text = draft.get_untracked();
            let text = text.trim();
            // A picture or a file on its own is a prompt, the same way it is
            // on the GPUI client; the send action is enabled for it.
            if text.is_empty() && attachments.with(|items| items.is_empty()) {
                return;
            }
            // The service clears the field once the runtime accepted the
            // message, so a failed send leaves the draft where it was.
            send(text);
        })
    };
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_SM))
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .bg(theme::background)
        .pb_keyboard()
        .child(when(
            move || !attachments.with(|attachments| attachments.is_empty()),
            move || attachment_chips(attachments, on_remove_attachment.clone()),
        ))
        .child(
            view()
                .w_full()
                .flex_row()
                .items_end()
                .gap(px(theme::SPACING_SM))
                .child(
                    field::text_field_with(
                        draft,
                        copy::text("Message the Agent…", "给 Agent 发消息…", "傳訊息給 Agent…"),
                        move |text: &str| {
                            match text.strip_prefix('/') {
                                Some(query) => on_query(query),
                                None => commands.set(Vec::new()),
                            }
                            match mention_query(text) {
                                Some(query) => on_mention(&query),
                                None => mentions.set(Vec::new()),
                            }
                        },
                    )
                    .multiline(true)
                    .min_h(px(theme::TOUCH_TARGET))
                    .max_h(px(120.0))
                    .flex_1(),
                )
                .child({
                    let on_attach_image = host.on_attach_image.clone();
                    action::icon_button(
                        icon(include_str!("../../assets/icons/image.svg")),
                        copy::text("Attach a photo", "附加照片", "附加照片"),
                        move || on_attach_image(),
                    )
                })
                .child({
                    let on_attach_file = host.on_attach_file.clone();
                    action::icon_button(
                        icon(include_str!("../../assets/icons/paperclip.svg")),
                        copy::text("Attach a file", "附加文件", "附加檔案"),
                        move || on_attach_file(),
                    )
                })
                .child(when(move || running.get(), {
                    let on_interrupt = on_interrupt.clone();
                    move || {
                        let on_interrupt = on_interrupt.clone();
                        action::icon_button(
                            icon(include_str!("../../assets/icons/square.svg")),
                            copy::text("Stop", "停止", "停止"),
                            move || on_interrupt(),
                        )
                    }
                }))
                .child(when(move || !running.get(), {
                    move || {
                        let submit = submit.clone();
                        action::primary(
                            copy::text("Send", "发送", "傳送"),
                            icon(include_str!("../../assets/icons/send.svg")),
                            move || submit(),
                        )
                        .disabled(move || {
                            sending.get()
                                || (draft.with(|draft| draft.trim().is_empty())
                                    && attachments.with(|attachments| attachments.is_empty()))
                        })
                        .disabled_style(|style| style.opacity(0.4))
                    }
                })),
        )
        // `pb_keyboard` covers the keyboard while it is up; with it down the
        // composer still has to clear the home indicator.
        .child(when(
            move || keyboard_height().0 == 0.0,
            || view().h(safe_area().bottom),
        ))
}

/// The pending attachments, each removable.
fn attachment_chips(
    attachments: Signal<Vec<AttachmentItem>>,
    on_remove: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .flex_wrap()
        .gap(px(theme::SPACING_XS))
        .child(each(
            move || attachments.get(),
            |item| item.id.clone(),
            move |item, _| attachment_chip(item, on_remove.clone()),
        ))
}

fn attachment_chip(
    item: ReadSignal<AttachmentItem>,
    on_remove: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    button()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .min_h(px(28.0))
        .px(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_dim)
        .on_press(move || {
            let id = item.get_untracked().id;
            on_remove(&id);
        })
        .child(
            text(move || item.with(|item| item.label.clone()))
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
        .child(
            text("×")
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
}

/// Rename, archive or delete one session.
fn session_actions_sheet(
    actions: Signal<Option<String>>,
    rename: Signal<String>,
    busy: Signal<bool>,
    on_close: Rc<dyn Fn()>,
    on_rename: Rc<dyn Fn()>,
    on_archive: Rc<dyn Fn()>,
    on_delete: Rc<dyn Fn()>,
) -> impl IntoElement {
    let open = computed(move || actions.get().is_some());
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event
                && !next
            {
                on_close();
            }
        })
        .child(
            view()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Session", "会话", "工作階段"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(
                    view()
                        .w_full()
                        .flex_row()
                        .items_end()
                        .gap(px(theme::SPACING_SM))
                        .child(field::text_field(
                            rename,
                            copy::text("Session title", "会话标题", "工作階段標題"),
                        ))
                        .child({
                            let on_rename = on_rename.clone();
                            action::primary(
                                copy::text("Rename", "重命名", "重新命名"),
                                icon(include_str!("../../assets/icons/check.svg")),
                                move || on_rename(),
                            )
                            .disabled(move || {
                                busy.get() || rename.with(|title| title.trim().is_empty())
                            })
                            .disabled_style(|style| style.opacity(0.4))
                        }),
                )
                .child({
                    let on_archive = on_archive.clone();
                    action::secondary(
                        copy::text("Archive", "归档", "封存"),
                        icon(include_str!("../../assets/icons/folder.svg")),
                        false,
                        move || on_archive(),
                    )
                    .w_full()
                    .disabled(move || busy.get())
                    .disabled_style(|style| style.opacity(0.4))
                })
                .child({
                    let on_delete = on_delete.clone();
                    action::secondary(
                        copy::text("Delete", "删除", "刪除"),
                        icon(include_str!("../../assets/icons/x.svg")),
                        false,
                        move || on_delete(),
                    )
                    .w_full()
                    .disabled(move || busy.get())
                    .disabled_style(|style| style.opacity(0.4))
                }),
        )
}

/// The session drawer, from the left edge.
fn drawer(host: SessionHost, open: Signal<bool>) -> impl IntoElement {
    let sessions = host.sessions;
    let active = host.active;
    let runtime = host.runtime;
    let on_select = host.on_select.clone();
    let on_disconnect = host.on_disconnect.clone();
    let on_new_session = host.on_new_session.clone();
    let on_switch_runtime = host.on_switch_runtime.clone();
    let on_open_terminal = host.on_open_terminal.clone();
    let on_open_files = host.on_open_files.clone();
    let on_open_git = host.on_open_git.clone();
    let on_session_menu = host.on_session_menu.clone();
    let on_open_settings = host.on_open_settings.clone();
    sheet()
        .side(Placement::Left)
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .child(
            view()
                .w(px(DRAWER_WIDTH))
                .h_full()
                .flex_col()
                .bg(theme::background_card)
                .pt_safe_area_plus(px(theme::SPACING_LG))
                .pb_safe_area_plus(px(theme::SPACING_MD))
                .child(
                    view()
                        .px(px(theme::SPACING_LG))
                        .pb(px(theme::SPACING_MD))
                        .child(brand::wordmark()),
                )
                .child(
                    view().flex_1().min_h(px(0.0)).child(
                        scroll().flex_1().child(
                            view()
                                .gap(px(theme::SPACING_XS))
                                .px(px(theme::SPACING_SM))
                                .child(each(
                                    move || sessions.get(),
                                    |session| session.id.clone(),
                                    {
                                        let on_select = on_select.clone();
                                        move |session, _| {
                                            session_row(
                                                session,
                                                active,
                                                on_select.clone(),
                                                on_session_menu.clone(),
                                                open,
                                            )
                                        }
                                    },
                                )),
                        ),
                    ),
                )
                .child(
                    view()
                        .border_t_1()
                        .border_t_color(theme::border_subtle)
                        .px(px(theme::SPACING_LG))
                        .pt(px(theme::SPACING_MD))
                        .gap(px(theme::SPACING_XS))
                        .child(
                            text(move || {
                                runtime.get().unwrap_or_else(|| {
                                    copy::text("No runtime", "未连接运行时", "未連線執行環境")
                                        .into()
                                })
                            })
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::text_muted),
                        )
                        .child(
                            action::secondary(
                                copy::text("Changes", "变更", "變更"),
                                icon(include_str!("../../assets/icons/history.svg")),
                                false,
                                {
                                    let on_open_git = on_open_git.clone();
                                    move || on_open_git()
                                },
                            )
                            .w_full(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Files", "文件", "檔案"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                {
                                    let on_open_files = on_open_files.clone();
                                    move || on_open_files()
                                },
                            )
                            .w_full(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Terminal", "终端", "終端"),
                                icon(include_str!("../../assets/icons/terminal.svg")),
                                false,
                                {
                                    let on_open_terminal = on_open_terminal.clone();
                                    move || on_open_terminal()
                                },
                            )
                            .w_full(),
                        )
                        .child(
                            action::primary(
                                copy::text("New session", "新建会话", "新增工作階段"),
                                icon(include_str!("../../assets/icons/circle-plus.svg")),
                                {
                                    let on_new_session = on_new_session.clone();
                                    move || on_new_session()
                                },
                            )
                            .w_full(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Switch runtime", "切换运行时", "切換執行環境"),
                                icon(include_str!("../../assets/icons/refresh.svg")),
                                false,
                                {
                                    let on_switch = on_switch_runtime.clone();
                                    move || on_switch()
                                },
                            )
                            .w_full(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Settings", "设置", "設定"),
                                icon(include_str!("../../assets/icons/settings.svg")),
                                false,
                                move || on_open_settings(),
                            )
                            .w_full(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Disconnect", "断开连接", "中斷連線"),
                                icon(include_str!("../../assets/icons/plug-zap.svg")),
                                false,
                                move || on_disconnect(),
                            )
                            .w_full(),
                        ),
                ),
        )
}

fn session_row(
    session: ReadSignal<SessionSummary>,
    active: Signal<Option<String>>,
    on_select: Rc<dyn Fn(&str)>,
    on_menu: SessionMenu,
    open: Signal<bool>,
) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .flex_row()
        .items_center()
        .child(
            button()
                .flex_1()
                .px(px(theme::SPACING_MD))
                .py(px(theme::SPACING_SM))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .active(|style| style.opacity(0.85))
                .on_press(move || {
                    on_select(&session.get_untracked().id);
                    open.set(false);
                })
                .child(
                    view()
                        .w(px(8.0))
                        .h(px(8.0))
                        .flex_shrink_0()
                        .rounded(px(4.0))
                        .bg(move || session.get().state.dot()),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(2.0))
                        .child(
                            text(move || session.get().title.clone())
                                .truncate()
                                .text_size(px(theme::font_body()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || {
                                    let id = session.get().id;
                                    if active.with(|active| active.as_deref() == Some(id.as_str()))
                                    {
                                        theme::foreground()
                                    } else {
                                        theme::text_secondary()
                                    }
                                }),
                        )
                        .child(
                            text(move || {
                                let session = session.get();
                                format!("{} · {}", session.agent, session.meta)
                            })
                            .truncate()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                        ),
                ),
        )
        .child({
            button()
                .min_h(px(theme::TOUCH_TARGET))
                .min_w(px(theme::TOUCH_TARGET))
                .items_center()
                .justify_center()
                .active(|style| style.opacity(0.7))
                .accessibility_label(copy::text("Session actions", "会话操作", "工作階段操作"))
                .on_press(move || {
                    let session = session.get_untracked();
                    on_menu(&session.id, &session.title);
                })
                .child(
                    text("⋯")
                        .text_size(px(theme::font_heading()))
                        .text_color(theme::text_muted),
                )
        })
}

/// What was typed after `@` when it is still one reference, if any.
fn mention_query(text: &str) -> Option<String> {
    let at = text.rfind('@')?;
    let after = &text[at + 1..];
    if after.chars().any(char::is_whitespace) {
        return None;
    }
    if at > 0
        && !text[..at]
            .chars()
            .next_back()
            .is_some_and(|previous| previous.is_whitespace() || previous == '(')
    {
        return None;
    }
    Some(after.to_owned())
}

/// Replaces the `@…` token with the reference the person picked.
fn insert_mention(text: &str, insertion: &str) -> String {
    match text.rfind('@') {
        Some(at) => format!("{}{insertion}", &text[..at]),
        None => insertion.to_owned(),
    }
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

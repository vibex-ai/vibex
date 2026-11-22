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
    ui::{action, field},
};

/// The height of the top bar, above the safe area.
const HEADER_HEIGHT: f32 = 48.0;

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
    /// The project the session belongs to, for the drawer's grouping.
    pub project_id: String,
    pub project_name: String,
    /// The checkout or worktree it runs in, for the row's second line.
    pub workspace_id: String,
    pub workspace_label: String,
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
    /// The turn's changed files, summarized.
    Changes,
    /// A permission request or its resolution.
    Permission,
    /// A question request or its resolution.
    Question,
    /// The fold above one turn's working rows.
    ProcessHeader,
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
    /// The turn this row belongs to; empty when the row stands alone.
    pub turn_id: String,
    /// The fold's row count, on a process header.
    pub process_count: usize,
    /// Whether the fold or the row's detail is open.
    pub expanded: bool,
    /// Whether a row has more detail to read.
    pub expandable: bool,
    /// What the user message carried.
    pub attachments: Vec<AttachmentItem>,
    /// The row's newest sequence, for a fork cut.
    pub last_sequence: i64,
    /// When the turn started, for its working header.
    pub started_at_ms: i64,
    /// When the turn ended, when it did.
    pub ended_at_ms: i64,
    /// Whether the turn is finished.
    pub complete: bool,
}

/// What the open session is running on, for the composer's chips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSummaryView {
    pub agent: String,
    pub model: String,
    pub reasoning: String,
    pub mode: String,
    /// Whether the session has been moved off the Agent's own defaults.
    pub options_set: bool,
    pub available: bool,
}

/// One Agent + auth source + model the runtime can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeOptionView {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub detail: String,
    pub selected: bool,
    pub available: bool,
}

/// One value of a reasoning, mode or session option row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeChoiceView {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// One of the Agent's own session options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFeatureView {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub toggle: bool,
    pub selected: bool,
    pub values: Vec<RuntimeChoiceView>,
}

/// What the runtime last reported about the session's token use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenUsageView {
    /// Context used, as a percentage, when the runtime reports both numbers.
    pub percent: Option<u8>,
    pub used: u64,
    pub size: u64,
    pub input: u64,
    pub output: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSectionKind {
    Folder,
    Group,
}

/// One drawer section the authority arranged: a folder or a session group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarSectionView {
    pub kind: SidebarSectionKind,
    pub pinned: bool,
    pub id: String,
    /// The project a folder or group belongs to; `None` only for a legacy
    /// runtime-wide section.
    pub project_id: Option<String>,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub collapsed: bool,
    pub name: String,
    pub session_ids: Vec<String>,
}

/// A prompt waiting for the running turn to settle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessageView {
    pub id: u64,
    pub text: String,
    pub attachment_count: usize,
}

/// One prompt already sent, for the composer's history sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerHistoryEntry {
    pub id: String,
    pub text: String,
}

/// What one step of the Agent's plan is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStepState {
    Pending,
    Running,
    Completed,
    Failed,
}

/// One step of the Agent's current plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStepView {
    pub title: String,
    pub state: PlanStepState,
}

/// The Agent's current plan, for the bar above the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanView {
    pub title: String,
    pub steps: Vec<PlanStepView>,
    pub completed: usize,
    pub current: usize,
}

/// The phase of the session's goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalPhaseView {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
    Unknown,
}

/// The session's standing goal, for the bar above the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalView {
    pub objective: String,
    pub phase: GoalPhaseView,
    pub progress: Option<String>,
    pub can_pause: bool,
    pub can_resume: bool,
    pub can_clear: bool,
}

/// What the reader asked the goal to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalControl {
    Pause,
    Resume,
    Clear,
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
type OpenDiff = CollapseAction;

/// Opens the actions sheet for a session, with its id and title.
type SessionMenu = FolderNameAction;

use crate::screens::new_session::{
    NewSessionHost, RuntimeChoice, WorkspaceChoice, new_session as new_session_sheet,
    project_browser_sheet as project_browser,
};

/// A callback with no arguments.
type Action = Rc<dyn Fn()>;
/// Chooses a value for one of the Agent's session options.
type ChooseFeature = FolderNameAction;
/// Opens one user message for replacement.
type EditMessage = Rc<dyn Fn(i64, &str)>;
type OpenAttachment = Rc<dyn Fn(&AttachmentItem)>;
type BatchAction = Rc<dyn Fn(&[String])>;
type FolderNameAction = Rc<dyn Fn(&str, &str)>;
type CreateGroupAction = Rc<dyn Fn(&str, &[String])>;
type CollapseAction = SessionToggle;
type SessionToggle = Rc<dyn Fn(&str, bool)>;
type MoveSessionAction = Rc<dyn Fn(&str, Option<&str>)>;
type MoveRelative = Rc<dyn Fn(&str, isize)>;
pub type BrowseDirs = Rc<dyn Fn(Option<&str>)>;
/// Opens one row's full detail.
type ToggleDetail = Rc<dyn Fn(&str, i64)>;

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
    Skill,
}

/// What the shell may do, and the state it shows.
#[derive(Clone)]
pub struct SessionHost {
    pub sessions: Signal<Vec<SessionSummary>>,
    pub active: Signal<Option<String>>,
    pub pinned: Signal<Vec<String>>,
    pub unread: Signal<Vec<String>>,
    pub auto_continue_sessions: Signal<Vec<String>>,
    pub sidebar_project_order: Signal<Vec<String>>,
    pub sidebar_workspace_order: Signal<Vec<(String, Vec<String>)>>,
    pub sidebar_session_order: Signal<Vec<String>>,
    pub collapsed_projects: Signal<Vec<String>>,
    pub collapsed_workspaces: Signal<Vec<String>>,
    pub on_set_project_collapsed: SessionToggle,
    pub on_set_workspace_collapsed: SessionToggle,
    pub on_set_session_auto_continue: SessionToggle,
    pub sidebar_sections: Signal<Vec<SidebarSectionView>>,
    pub folder_open: Signal<bool>,
    pub folder_name: Signal<String>,
    pub on_open_subfolder: Rc<dyn Fn(&str)>,
    pub on_open_folder: Action,
    pub on_close_folder: Action,
    pub on_create_folder: Action,
    pub on_rename_folder: FolderNameAction,
    pub on_delete_folder: Rc<dyn Fn(&str)>,
    pub on_rename_group: FolderNameAction,
    pub on_delete_group: Rc<dyn Fn(&str)>,
    pub on_set_group_members: CreateGroupAction,
    pub on_set_group_pinned: SessionToggle,
    pub on_toggle_folder_collapsed: CollapseAction,
    pub on_toggle_group_collapsed: CollapseAction,
    pub on_create_group: CreateGroupAction,
    pub move_open: Signal<Option<String>>,
    pub on_open_move: Action,
    pub on_close_move: Action,
    pub on_move_session: MoveSessionAction,
    pub on_move_session_relative: MoveRelative,
    pub on_toggle_pin: Rc<dyn Fn(&str)>,

    pub timeline: Signal<Vec<TimelineEntry>>,
    pub draft: Signal<String>,
    pub sending: Signal<bool>,
    pub error: Signal<Option<SharedString>>,
    /// Whether the transcript has older history, and a read of it is running.
    pub has_older: Signal<bool>,
    pub loading_older: Signal<bool>,
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
    /// The `$` skills the session offers.
    pub skills: Signal<Vec<CommandEntry>>,
    pub on_skill_query: MentionQuery,
    /// Remembers the suggestion the reader picked, for its command or reference.
    pub on_arm_entry: Rc<dyn Fn(&str)>,
    /// Files and photos waiting to be sent with the message.
    pub attachments: Signal<Vec<AttachmentItem>>,
    pub on_attach_image: Action,
    pub on_attach_camera: Action,
    pub on_attach_file: Action,
    pub on_remove_attachment: Rc<dyn Fn(&str)>,
    /// Starts a new session in the open session's workspace.
    pub on_new_session: Action,
    /// Opens the settings sheet over the shell.
    pub on_open_settings: Action,
    /// The runtime the open session runs on, and the choices the runtime offers.
    pub runtime_summary: Signal<Option<RuntimeSummaryView>>,
    pub runtime_options: Signal<Vec<RuntimeOptionView>>,
    pub runtime_reasoning: Signal<Vec<RuntimeChoiceView>>,
    pub runtime_modes: Signal<Vec<RuntimeChoiceView>>,
    pub runtime_features: Signal<Vec<RuntimeFeatureView>>,
    /// The session's token use, when the runtime reports it.
    pub token_usage: Signal<Option<TokenUsageView>>,
    /// The Agent's current plan and the session's goal.
    pub plan: Signal<Option<PlanView>>,
    pub goal: Signal<Option<GoalView>>,
    /// The prompts already sent in this session.
    pub history: Signal<Vec<ComposerHistoryEntry>>,
    pub history_open: Signal<bool>,
    /// Prompts written ahead of a running turn.
    pub queued: Signal<Vec<QueuedMessageView>>,
    pub queue_paused: Signal<bool>,
    pub queue_manual: Signal<bool>,
    pub on_toggle_queue_manual: Action,
    /// The sequence of the latest user message that can be edited, and whether
    /// the composer is replacing it.
    pub editable_sequence: Signal<i64>,
    pub editing: Signal<bool>,
    pub on_edit_message: EditMessage,
    pub on_cancel_edit: Action,
    pub on_replace_message: Rc<dyn Fn(&str)>,

    pub on_send_queued: Rc<dyn Fn(u64)>,
    pub on_remove_queued: Rc<dyn Fn(u64)>,
    pub on_move_queued: Rc<dyn Fn(u64, isize)>,
    /// The scheduled-send menu and the prompt waiting on it.
    pub schedule_open: Signal<bool>,
    pub scheduled: Signal<bool>,
    pub on_open_schedule: Action,
    pub on_close_schedule: Action,
    pub on_schedule: Rc<dyn Fn(u64)>,
    pub on_cancel_schedule: Action,
    pub on_clear_queue: Action,
    pub on_toggle_queue_paused: Action,
    pub on_open_history: Action,
    pub on_close_history: Action,
    pub on_recall_history: Rc<dyn Fn(&str)>,

    pub on_goal_control: Rc<dyn Fn(GoalControl)>,
    pub runtime_open: Signal<bool>,
    pub runtime_busy: Signal<bool>,
    pub on_open_runtime: Action,
    pub on_close_runtime: Action,
    pub on_choose_runtime: Rc<dyn Fn(&str)>,
    pub on_choose_reasoning: Rc<dyn Fn(&str)>,
    pub on_choose_mode: Rc<dyn Fn(&str)>,
    pub on_choose_feature: ChooseFeature,
    /// Opens or closes one turn's working rows.
    pub on_toggle_turn: Rc<dyn Fn(&str)>,
    /// Opens a row's full detail, by row id and sequence.
    pub on_toggle_detail: ToggleDetail,
    /// Whether the conversation find bar is open.
    pub find_open: Signal<bool>,
    pub on_toggle_find: Action,
    /// Whether the last turn needs an explicit continue, and the action for it.
    pub needs_continue: Signal<bool>,
    pub on_continue_turn: Action,
    /// Whether the optional conversation chrome is shown.
    pub show_token_usage: Signal<bool>,
    pub show_plan_goal: Signal<bool>,
    pub show_generation_status: Signal<bool>,
    pub show_reasoning: Signal<bool>,
    /// Whether a paused turn continues on its own.
    pub auto_continue: Signal<bool>,
    pub on_toggle_auto_continue: Action,
    pub team_open: Signal<bool>,
    pub team_tasks: Signal<Vec<crate::screens::team::TeamTaskView>>,
    pub team_busy: Signal<bool>,
    pub team_error: Signal<Option<SharedString>>,
    pub on_open_team: Action,
    pub on_close_team: Action,
    pub on_refresh_team: Action,
    pub on_accept_team: Rc<dyn Fn(&str)>,
    pub on_cancel_team: Rc<dyn Fn(&str)>,
    pub on_interrupt_team: Rc<dyn Fn(&str)>,
    /// Reads the page above the transcript.
    pub on_load_older: Action,
    /// Forks the conversation through a row's sequence.
    pub on_fork: Rc<dyn Fn(i64)>,
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
    pub files_search: Signal<Vec<crate::screens::files::FileSearchRow>>,
    pub on_search_files: Rc<dyn Fn(&str)>,
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
    pub new_base_ref: Signal<String>,
    pub new_title: Signal<String>,
    pub new_prompt: Signal<String>,
    pub new_attachments: Signal<Vec<AttachmentItem>>,
    pub on_create_temporary_session: Action,
    pub new_project_path: Signal<String>,
    pub project_dirs: Signal<Vec<crate::screens::new_session::DirectoryEntry>>,
    pub project_browser_open: Signal<bool>,
    pub project_dir_path: Signal<Option<String>>,
    pub project_dir_parent: Signal<Option<String>>,
    pub on_open_project: Action,
    pub on_browse_project_dirs: BrowseDirs,
    pub on_attach_new_image: Action,
    pub on_attach_new_file: Action,
    pub on_remove_new_attachment: Rc<dyn Fn(&str)>,
    /// The message attachment whose preview is open.
    pub attachment_preview: Signal<Option<AttachmentItem>>,
    pub on_open_attachment: OpenAttachment,
    pub on_close_attachment: Action,
    pub on_create_worktree_session: Action,
    pub on_close_new_session: Action,
    /// The id of the session whose actions are open, if any.
    pub session_actions: Signal<Option<String>>,
    /// The title being edited in that sheet.
    pub session_rename: Signal<String>,
    pub session_busy: Signal<bool>,
    pub on_session_menu: SessionMenu,
    pub on_archive_sessions: BatchAction,
    pub on_delete_sessions: BatchAction,
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
    pub git_amend: Signal<bool>,
    pub git_push_after: Signal<bool>,
    pub git_remote_busy: Signal<bool>,
    pub git_branches: Signal<Vec<crate::screens::git::BranchRow>>,
    pub git_branch_menu: Signal<bool>,
    pub on_open_git_branches: Action,
    pub on_close_git_branches: Action,
    pub on_select_git_branch: Rc<dyn Fn(&str)>,
    pub worktrees: Signal<Vec<crate::screens::git::WorktreeRow>>,
    pub worktree_menu: Signal<bool>,
    pub worktree_operations: Signal<Vec<crate::screens::git::WorktreeOperationRow>>,
    pub worktree_busy: Signal<bool>,
    pub worktree_error: Signal<Option<SharedString>>,
    pub on_open_worktrees: Action,
    pub on_close_worktrees: Action,
    pub on_archive_worktree: Rc<dyn Fn(&str)>,
    pub on_merge_worktree: Rc<dyn Fn(&str)>,
    pub on_discard_worktree: Rc<dyn Fn(&str)>,
    pub on_continue_worktree: Rc<dyn Fn(&str)>,
    pub on_abort_worktree: Rc<dyn Fn(&str)>,
    pub on_resolve_worktree: crate::screens::git::ResolveWorktree,
    pub on_restore_worktree: Rc<dyn Fn(&str)>,
    pub on_toggle_git_amend: Action,
    pub on_toggle_git_push_after: Action,
    pub on_git_push: Action,
    pub on_git_pull: Action,
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
    view().flex_1().bg(theme::block_page).child(
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
        search: host.files_search,
        on_search: host.on_search_files.clone(),
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
        amend: host.git_amend,
        push_after: host.git_push_after,
        remote_busy: host.git_remote_busy,
        branches: host.git_branches,
        branch_menu: host.git_branch_menu,
        on_open_branches: host.on_open_git_branches.clone(),
        on_close_branches: host.on_close_git_branches.clone(),
        on_select_branch: host.on_select_git_branch.clone(),
        worktrees: host.worktrees,
        worktree_menu: host.worktree_menu,
        worktree_operations: host.worktree_operations,
        worktree_busy: host.worktree_busy,
        worktree_error: host.worktree_error,
        on_open_worktrees: host.on_open_worktrees.clone(),
        on_close_worktrees: host.on_close_worktrees.clone(),
        on_archive_worktree: host.on_archive_worktree.clone(),
        on_merge_worktree: host.on_merge_worktree.clone(),
        on_discard_worktree: host.on_discard_worktree.clone(),
        on_continue_worktree: host.on_continue_worktree.clone(),
        on_abort_worktree: host.on_abort_worktree.clone(),
        on_resolve_worktree: host.on_resolve_worktree.clone(),
        on_restore_worktree: host.on_restore_worktree.clone(),
        on_toggle_amend: host.on_toggle_git_amend.clone(),
        on_toggle_push_after: host.on_toggle_git_push_after.clone(),
        on_push: host.on_git_push.clone(),
        on_pull: host.on_git_pull.clone(),
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
        .bg(theme::block_page)
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

thread_local! {
    /// The mounted shell's drawer signal, for the platform back gesture.
    static DRAWER_OPEN: std::cell::Cell<Option<Signal<bool>>> = const {
        std::cell::Cell::new(None)
    };
}

/// Whether the mounted session drawer is open.
pub fn drawer_is_open() -> bool {
    DRAWER_OPEN.with(|cell| cell.get().is_some_and(|open| open.get_untracked()))
}

/// Opens the mounted session drawer, if a shell is on screen.
pub fn open_drawer() {
    DRAWER_OPEN.with(|cell| {
        if let Some(open) = cell.get() {
            open.set(true);
        }
    });
}

/// Closes the mounted session drawer, if a shell is on screen.
pub fn close_drawer() {
    DRAWER_OPEN.with(|cell| {
        if let Some(open) = cell.get() {
            open.set(false);
        }
    });
}

/// The shell proper: header, transcript, cards, composer and drawer.
fn shell(host: SessionHost) -> impl IntoElement {
    let drawer_open = signal(false);
    let tools_open = signal(false);
    DRAWER_OPEN.with(|cell| cell.set(Some(drawer_open)));
    let edge_drag = nsh_core::element::animated_value(0.0);
    let open_drag = nsh_core::element::animated_value(0.0);
    let connection = host.connection;
    let connection_error = host.connection_error;
    let on_retry = host.on_retry.clone();

    view()
        .flex_1()
        .relative()
        .bg(theme::block_page)
        .pan_values(Some(&open_drag), None)
        .pan_enabled(move || !drawer_open.get())
        .on_pan_start(|_: &PanEvent| {})
        .on_pan_end(move |event: &PanEvent| {
            if !event.cancelled && event.translation.x.0 > 50.0 {
                drawer_open.set(true);
            }
        })
        .flex_col()
        .child(header(host.clone(), drawer_open, tools_open))
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
                let on_arm = host.on_arm_entry.clone();
                move || command_palette(commands, draft, PaletteKind::Slash, on_arm.clone())
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
                let on_arm = host.on_arm_entry.clone();
                move || command_palette(mentions, draft, PaletteKind::Mention, on_arm.clone())
            },
        ))
        .child(when(
            move || {
                skill_query(&host.draft.get()).is_some()
                    && !host.skills.with(|skills| skills.is_empty())
            },
            {
                let skills = host.skills;
                let draft = host.draft;
                let on_arm = host.on_arm_entry.clone();
                move || command_palette(skills, draft, PaletteKind::Skill, on_arm.clone())
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
        .child(runtime_sheet(host.clone()))
        .child(history_sheet(host.clone()))
        .child(tools_sheet(
            tools_open,
            host.on_open_files.clone(),
            host.on_open_git.clone(),
            host.on_open_terminal.clone(),
            host.on_open_team.clone(),
        ))
        .child(drawer(host.clone(), drawer_open))
        .child(
            view()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .bottom(px(0.0))
                .w(px(24.0))
                .pan_values(Some(&edge_drag), None)
                .pan_enabled(true)
                .on_pan_start(|_: &PanEvent| {})
                .on_pan_end(move |event: &PanEvent| {
                    if !event.cancelled && event.translation.x.0 > 40.0 {
                        drawer_open.set(true);
                    }
                }),
        )
        .child(session_actions_sheet(
            host.session_actions,
            host.session_rename,
            host.session_busy,
            host.on_close_session_actions.clone(),
            host.on_rename_session.clone(),
            host.on_open_move.clone(),
            host.auto_continue_sessions,
            host.on_set_session_auto_continue.clone(),
            host.pinned,
            host.on_toggle_pin.clone(),
            host.on_archive_session.clone(),
            host.on_delete_session.clone(),
            host.on_open_team.clone(),
        ))
        .child(attachment_preview_sheet(
            host.attachment_preview,
            host.on_close_attachment.clone(),
        ))
        .child(move_session_sheet(
            host.move_open,
            host.sidebar_sections,
            host.on_move_session.clone(),
            host.on_close_move.clone(),
        ))
        .child(crate::screens::team::team(crate::screens::team::TeamHost {
            open: host.team_open,
            tasks: host.team_tasks,
            busy: host.team_busy,
            error: host.team_error,
            on_refresh: host.on_refresh_team.clone(),
            on_accept: host.on_accept_team.clone(),
            on_cancel: host.on_cancel_team.clone(),
            on_interrupt: host.on_interrupt_team.clone(),
            on_close: host.on_close_team.clone(),
        }))
        .child(project_browser(
            host.project_browser_open,
            host.project_dirs,
            host.project_dir_path,
            host.project_dir_parent,
            host.on_browse_project_dirs.clone(),
            host.new_project_path,
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
            on_create_temporary: host.on_create_temporary_session.clone(),
            project_path: host.new_project_path,
            on_open_project: host.on_open_project.clone(),
            project_dirs: host.project_dirs,
            browser_open: host.project_browser_open,
            project_dir_path: host.project_dir_path,
            project_dir_parent: host.project_dir_parent,
            on_browse_project_dirs: host.on_browse_project_dirs.clone(),
            title: host.new_title,
            prompt: host.new_prompt,
            attachments: host.new_attachments,
            on_attach_image: host.on_attach_new_image.clone(),
            on_attach_file: host.on_attach_new_file.clone(),
            on_remove_attachment: host.on_remove_new_attachment.clone(),
            worktree_branch: host.new_worktree_branch,
            base_ref: host.new_base_ref,
            on_create_worktree: host.on_create_worktree_session.clone(),
        }))
}

/// The top bar: the drawer, the title, a refresh.
fn header(
    host: SessionHost,
    drawer_open: Signal<bool>,
    tools_open: Signal<bool>,
) -> impl IntoElement {
    let sessions = host.sessions;
    let active = host.active;
    let refresh = host.on_refresh.clone();
    let on_open_history = host.on_open_history.clone();
    let on_toggle_find = host.on_toggle_find.clone();
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
                .child(action::icon_button(
                    icon(include_str!("../../assets/icons/history.svg")),
                    copy::text("Earlier messages", "历史消息", "歷史訊息"),
                    move || on_open_history(),
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
                    icon(include_str!("../../assets/icons/wrench.svg")),
                    copy::text("Tools", "工具", "工具"),
                    move || tools_open.set(true),
                ))
                .child({
                    let on_toggle_find = on_toggle_find.clone();
                    action::icon_button(
                        icon(include_str!("../../assets/icons/search.svg")),
                        copy::text("Find in conversation", "在会话中查找", "在工作階段中尋找"),
                        move || on_toggle_find(),
                    )
                })
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
    let on_toggle_turn = host.on_toggle_turn.clone();
    let on_toggle_detail = host.on_toggle_detail.clone();
    let on_fork = host.on_fork.clone();
    let editable_sequence = host.editable_sequence;
    let on_edit_message = host.on_edit_message.clone();
    let on_open_attachment = host.on_open_attachment.clone();
    let find_open = host.find_open;
    let find_query = signal(String::new());
    let find_cursor = signal(0usize);
    let find_target = signal(0usize);
    let find_matches = computed(move || {
        let query = find_query.with(|query| query.trim().to_lowercase());
        if query.is_empty() {
            return Vec::new();
        }
        timeline.with(|entries| {
            entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry.title.to_lowercase().contains(&query)
                        || entry.body.to_lowercase().contains(&query)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        })
    });
    let has_older = host.has_older;
    let loading_older = host.loading_older;
    let on_load_older = host.on_load_older.clone();
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
                view()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex_col()
                    .child(when(move || has_older.get(), {
                        let on_load_older = on_load_older.clone();
                        move || {
                            let on_load_older = on_load_older.clone();
                            button()
                                .on_press(move || {
                                    if !loading_older.get_untracked() {
                                        on_load_older();
                                    }
                                })
                                .w_full()
                                .min_h(px(36.0))
                                .items_center()
                                .justify_center()
                                .child(
                                    text(move || {
                                        if loading_older.get() {
                                            copy::text(
                                                "Loading earlier...",
                                                "正在加载更早的消息…",
                                                "正在載入更早的訊息…",
                                            )
                                        } else {
                                            copy::text(
                                                "Load earlier messages",
                                                "加载更早的消息",
                                                "載入更早的訊息",
                                            )
                                        }
                                    })
                                    .text_size(px(theme::font_caption()))
                                    .text_color(theme::text_muted),
                                )
                        }
                    }))
                    .child(when(move || find_open.get(), {
                        let close = find_open;
                        move || find_bar(find_query, find_matches, find_cursor, find_target, close)
                    }))
                    .child(dynamic({
                        let on_toggle_turn = on_toggle_turn.clone();
                        let on_toggle_detail = on_toggle_detail.clone();
                        let on_fork = on_fork.clone();
                        let on_edit_message = on_edit_message.clone();
                        let on_open_attachment = on_open_attachment.clone();
                        move || {
                            let target = find_target.get();
                            let list =
                                virtual_list(move || timeline.with(|entries| entries.len()), {
                                    let on_toggle_turn = on_toggle_turn.clone();
                                    let on_toggle_detail = on_toggle_detail.clone();
                                    let on_fork = on_fork.clone();
                                    let editable_sequence = editable_sequence;
                                    let on_edit_message = on_edit_message.clone();
                                    let on_open_attachment = on_open_attachment.clone();
                                    move |at: Position| {
                                        entry_row(
                                            at,
                                            timeline,
                                            on_toggle_turn.clone(),
                                            on_toggle_detail.clone(),
                                            on_fork.clone(),
                                            editable_sequence,
                                            on_edit_message.clone(),
                                            on_open_attachment.clone(),
                                        )
                                    }
                                })
                                .keys(move || {
                                    timeline.with(|entries| {
                                        entries
                                            .iter()
                                            .map(|entry| entry.id.clone())
                                            .collect::<Vec<_>>()
                                    })
                                })
                                .estimated_item_size(px(72.0));
                            if target > 0 {
                                list.initial_scroll_index(target.saturating_sub(1)).flex_1()
                            } else {
                                // A chat opens at its newest message and stays
                                // there while a reply streams in, unless the
                                // reader scrolled up.
                                list.alignment(ListAlignment::Bottom)
                                    .follow_end(px(120.0))
                                    .flex_1()
                            }
                        }
                    }))
            },
        ))
}

fn find_bar(
    query: Signal<String>,
    matches: Memo<Vec<usize>>,
    cursor: Signal<usize>,
    target: Signal<usize>,
    close: Signal<bool>,
) -> impl IntoElement {
    let count = computed(move || {
        let total = matches.with(|matches| matches.len());
        if total == 0 {
            "0/0".to_owned()
        } else {
            format!(
                "{}/{}",
                cursor.with(|cursor| (*cursor).min(total - 1)) + 1,
                total
            )
        }
    });
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_XS))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            field::text_field_with(
                query,
                copy::text("Find in conversation", "在会话中查找", "在工作階段中尋找"),
                move |_| {
                    cursor.set(0);
                    target.set(0);
                },
            )
            .flex_1(),
        )
        .child(
            text(move || count.get())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
        .child(find_step("‹", -1, matches, cursor, target))
        .child(find_step("›", 1, matches, cursor, target))
        .child(small_action(
            copy::text("Close", "关闭", "關閉"),
            move || {
                close.set(false);
            },
        ))
}

fn find_step(
    glyph: &'static str,
    direction: isize,
    matches: Memo<Vec<usize>>,
    cursor: Signal<usize>,
    target: Signal<usize>,
) -> impl IntoElement {
    button()
        .on_press(move || {
            let total = matches.with(|matches| matches.len());
            if total == 0 {
                return;
            }
            let next = if direction < 0 {
                (cursor.get_untracked() + total - 1) % total
            } else {
                (cursor.get_untracked() + 1) % total
            };
            cursor.set(next);
            target.set(matches.with(|matches| matches[next]) + 1);
        })
        .min_h(px(theme::TOUCH_TARGET))
        .min_w(px(theme::TOUCH_TARGET))
        .items_center()
        .justify_center()
        .child(
            text(glyph)
                .text_size(px(theme::font_heading()))
                .text_color(theme::text_secondary),
        )
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
#[allow(clippy::too_many_arguments)]
fn entry_row(
    at: Position,
    entries: Signal<Vec<TimelineEntry>>,
    on_toggle_turn: Rc<dyn Fn(&str)>,
    on_toggle_detail: ToggleDetail,
    on_fork: Rc<dyn Fn(i64)>,
    editable_sequence: Signal<i64>,
    on_edit_message: EditMessage,
    on_open_attachment: OpenAttachment,
) -> impl IntoElement {
    let entry = computed(move || entries.with(|entries| entries.get(at.index()).cloned()));
    view()
        .w_full()
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .child(entry_element(
            entry,
            on_toggle_turn,
            on_toggle_detail,
            on_fork,
            editable_sequence,
            on_edit_message,
            on_open_attachment,
        ))
}

fn entry_element(
    entry: Memo<Option<TimelineEntry>>,
    on_toggle_turn: Rc<dyn Fn(&str)>,
    on_toggle_detail: ToggleDetail,
    on_fork: Rc<dyn Fn(i64)>,
    editable_sequence: Signal<i64>,
    on_edit_message: EditMessage,
    on_open_attachment: OpenAttachment,
) -> impl IntoElement {
    let kind = computed(move || entry.with(|e| e.as_ref().map(|e| e.kind)));
    let body =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.body.clone()).unwrap_or_default()));
    let title =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.title.clone()).unwrap_or_default()));
    let meta =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.meta.clone()).unwrap_or_default()));
    let streaming = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.streaming)));
    let attachments = computed(move || {
        entry.with(|e| {
            e.as_ref()
                .map(|e| e.attachments.clone())
                .unwrap_or_default()
        })
    });
    let turn_id =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.turn_id.clone()).unwrap_or_default()));
    let process_count = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.process_count)));
    let expanded = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.expanded)));
    let sequence = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.last_sequence)));
    let started_at_ms = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.started_at_ms)));
    let ended_at_ms = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.ended_at_ms)));
    let complete = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.complete)));
    let expandable = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.expandable)));
    let row_expanded = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.expanded)));

    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::User)),
            move || {
                user_bubble(
                    body,
                    attachments,
                    sequence,
                    editable_sequence,
                    on_edit_message.clone(),
                    on_open_attachment.clone(),
                )
            },
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::ProcessHeader)),
            {
                let on_toggle_turn = on_toggle_turn.clone();
                move || {
                    process_header_row(
                        started_at_ms,
                        ended_at_ms,
                        complete,
                        process_count,
                        expanded,
                        turn_id,
                        on_toggle_turn.clone(),
                    )
                }
            },
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Agent)),
            {
                let on_fork = on_fork.clone();
                move || agent_prose(title, body, streaming, sequence, on_fork.clone())
            },
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Reasoning)),
            move || muted_prose(copy::text("Thought", "思考", "思考"), body),
        ))
        .child(when(
            move || kind.with(|kind| matches!(kind, Some(EntryKind::Tool | EntryKind::Notice))),
            {
                let on_toggle_detail = on_toggle_detail.clone();
                move || {
                    let on_toggle_detail = on_toggle_detail.clone();
                    let toggle = Rc::new(move || {
                        on_toggle_detail(&turn_id.get_untracked(), sequence.get_untracked())
                    });
                    tool_card(title, body, meta, expandable, row_expanded, toggle)
                }
            },
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Error)),
            move || error_card(body),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Changes)),
            move || changes_card(title, body),
        ))
        .child(when(
            move || {
                kind.with(|kind| matches!(kind, Some(EntryKind::Permission | EntryKind::Question)))
            },
            move || decision_card(title, body, meta),
        ))
}

fn user_bubble(
    body: Memo<String>,
    attachments: Memo<Vec<AttachmentItem>>,
    sequence: Memo<i64>,
    editable_sequence: Signal<i64>,
    on_edit: EditMessage,
    on_open_attachment: OpenAttachment,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .items_end()
        .gap(px(theme::SPACING_XS))
        .child(
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
        .child(when(
            move || !attachments.with(|items| items.is_empty()),
            move || message_attachments(attachments, on_open_attachment.clone()),
        ))
        .child(when(
            move || sequence.get() > 0 && editable_sequence.get() == sequence.get(),
            {
                let on_edit = on_edit.clone();
                move || {
                    let on_edit = on_edit.clone();
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .child(small_action(
                            copy::text("Edit", "编辑", "編輯"),
                            move || {
                                let seq = sequence.get_untracked();
                                let text = body.get_untracked();
                                on_edit(seq, &text);
                            },
                        ))
                }
            },
        ))
}

fn message_attachments(
    attachments: Memo<Vec<AttachmentItem>>,
    on_open: OpenAttachment,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .flex_wrap()
        .justify_end()
        .gap(px(theme::SPACING_XS))
        .child(each(
            move || attachments.get(),
            |item| item.id.clone(),
            move |item, _| {
                let on_open = on_open.clone();
                button()
                    .on_press(move || {
                        let item = item.get_untracked();
                        on_open(&item);
                    })
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    .min_h(px(28.0))
                    .px(px(theme::SPACING_SM))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::border_subtle)
                    .bg(theme::background_dim)
                    .child(
                        text(move || item.with(|item| item.label.clone()))
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_secondary),
                    )
            },
        ))
}

fn process_header_row(
    started_at_ms: Memo<i64>,
    ended_at_ms: Memo<i64>,
    complete: Memo<bool>,
    process_count: Memo<usize>,
    expanded: Memo<bool>,
    turn_id: Memo<String>,
    on_toggle: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(36.0))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .active(|style| style.opacity(0.8))
        .on_press(move || on_toggle(&turn_id.get_untracked()))
        .child(
            text(move || {
                let label = if complete.get() {
                    copy::text("Worked for", "工作了", "工作了")
                } else {
                    copy::text("Working for", "工作了", "工作了")
                };
                format!(
                    "{label} {}",
                    work_duration(started_at_ms.get(), ended_at_ms.get(), complete.get())
                )
            })
            .flex_1()
            .truncate()
            .text_size(px(theme::font_detail()))
            .text_color(theme::text_secondary),
        )
        .child(
            text(move || process_count.get().to_string())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
        .child(
            text(move || if expanded.get() { "▾" } else { "▸" })
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
}

fn work_duration(started_at_ms: i64, ended_at_ms: i64, complete: bool) -> String {
    let end = if complete && ended_at_ms > 0 {
        ended_at_ms
    } else {
        nsh_core::time::unix_ms() as i64
    };
    let seconds = ((end - started_at_ms).max(0) / 1_000) as u64;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

fn agent_prose(
    title: Memo<String>,
    body: Memo<String>,
    streaming: Memo<bool>,
    sequence: Memo<i64>,
    on_fork: Rc<dyn Fn(i64)>,
) -> impl IntoElement {
    let images = computed(move || markdown_images(&body.get()));
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
            text_view()
                .markdown(move || Some(body.get().into()))
                .options(move || MarkdownOptions {
                    streaming: streaming.get(),
                    ..MarkdownOptions::default()
                })
                .selectable(true),
        )
        .child(when(
            move || images.with(|images| !images.is_empty()),
            move || {
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(theme::SPACING_XS))
                    .child(each(
                        move || images.get(),
                        |source| source.clone(),
                        move |source, _| {
                            image(move || ImageSrc::from(source.get_untracked()))
                                .w_full()
                                .max_h(px(320.0))
                                .object_fit(nsh_core::ResizeMode::Contain)
                        },
                    ))
            },
        ))
        .child(message_actions(body, sequence, on_fork))
}

fn markdown_images(text: &str) -> Vec<String> {
    let mut images = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        rest = &rest[start + 2..];
        let Some(close) = rest.find(']') else {
            break;
        };
        let after = &rest[close + 1..];
        let Some(after) = after.strip_prefix('(') else {
            continue;
        };
        let Some(end) = after.find(')') else {
            break;
        };
        let source = after[..end].trim();
        if source.starts_with("http") || source.starts_with("data:image/") {
            images.push(source.to_owned());
            if images.len() >= 4 {
                break;
            }
        }
        rest = &after[end + 1..];
    }
    images
}

fn message_actions(
    body: Memo<String>,
    sequence: Memo<i64>,
    on_fork: Rc<dyn Fn(i64)>,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .child(small_action(
            copy::text("Copy", "复制", "複製"),
            move || set_clipboard(body.get_untracked()),
        ))
        .child(small_action(
            copy::text("Share", "分享", "分享"),
            move || {
                let text = body.get_untracked();
                spawn(async move {
                    let _ = share(nsh_core::share::ShareRequest::new().text(text)).await;
                });
            },
        ))
        .child(small_action(copy::text("Fork", "分叉", "分叉"), {
            let on_fork = on_fork.clone();
            move || on_fork(sequence.get_untracked())
        }))
}

fn small_action_reactive(
    label: impl Fn() -> &'static str + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_press)
        .min_h(px(28.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .active(|style| style.opacity(0.7))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
}

fn small_action(label: &'static str, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .on_press(on_press)
        .min_h(px(28.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .active(|style| style.opacity(0.7))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
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

fn tool_card(
    title: Memo<String>,
    body: Memo<String>,
    meta: Memo<String>,
    expandable: Memo<bool>,
    expanded: Memo<bool>,
    on_toggle: Rc<dyn Fn()>,
) -> impl IntoElement {
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
                )
                .child(when(
                    move || expandable.get(),
                    move || {
                        let on_toggle = on_toggle.clone();
                        button()
                            .on_press(move || on_toggle())
                            .min_h(px(24.0))
                            .px(px(theme::SPACING_XS))
                            .items_center()
                            .child(
                                text(move || {
                                    if expanded.get() {
                                        copy::text("Hide", "收起", "收起")
                                    } else {
                                        copy::text("Detail", "详情", "詳情")
                                    }
                                })
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                            )
                    },
                )),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::font_caption()))
                .line_height(theme::line_height_for(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
}

fn changes_card(title: Memo<String>, body: Memo<String>) -> impl IntoElement {
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
                .gap(px(theme::SPACING_XS))
                .child(
                    svg(icon(include_str!("../../assets/icons/file-text.svg")))
                        .w(px(theme::ICON_SM))
                        .h(px(theme::ICON_SM))
                        .text_color(theme::text_muted),
                )
                .child(
                    text(move || title.get())
                        .flex_1()
                        .text_size(px(theme::font_detail()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::foreground),
                ),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::font_micro()))
                .line_height(theme::line_height_for(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
}

fn decision_card(title: Memo<String>, body: Memo<String>, meta: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default)
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
    on_arm: Rc<dyn Fn(&str)>,
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
                    {
                        let on_arm = on_arm.clone();
                        move |command, _| command_row(command, draft, kind, on_arm.clone())
                    },
                ))),
        )
}

fn command_row(
    command: ReadSignal<CommandEntry>,
    draft: Signal<String>,
    kind: PaletteKind,
    on_arm: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    button()
        .on_press(move || {
            let command = command.get_untracked();
            on_arm(&command.id);
            let insertion = command.insertion;
            draft.set(match kind {
                PaletteKind::Slash => insertion,
                PaletteKind::Mention => insert_mention(&draft.get_untracked(), &insertion),
                PaletteKind::Skill => insert_skill(&draft.get_untracked(), &insertion),
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
    let skills = host.skills;
    let on_query = host.on_command_query.clone();
    let on_mention = host.on_mention_query.clone();
    let on_skill = host.on_skill_query.clone();
    let on_interrupt = host.on_interrupt.clone();
    let sessions = host.sessions;
    let active = host.active;
    let attachments = host.attachments;
    let on_remove_attachment = host.on_remove_attachment.clone();
    let runtime_summary = host.runtime_summary;
    let runtime_reasoning = host.runtime_reasoning;
    let runtime_modes = host.runtime_modes;
    let runtime_features = host.runtime_features;
    let token_usage = host.token_usage;
    let show_token_usage = host.show_token_usage;
    let show_plan_goal = host.show_plan_goal;
    let plan = host.plan;
    let goal = host.goal;
    let queued = host.queued;
    let queue_paused = host.queue_paused;
    let queue_manual = host.queue_manual;
    let on_toggle_queue_manual = host.on_toggle_queue_manual.clone();
    let on_send_queued = host.on_send_queued.clone();
    let on_remove_queued = host.on_remove_queued.clone();
    let on_move_queued = host.on_move_queued.clone();
    let schedule_open = host.schedule_open;
    let scheduled = host.scheduled;
    let on_open_schedule = host.on_open_schedule.clone();
    let on_close_schedule = host.on_close_schedule.clone();
    let on_schedule = host.on_schedule.clone();
    let on_cancel_schedule = host.on_cancel_schedule.clone();
    let on_clear_queue = host.on_clear_queue.clone();
    let on_toggle_queue_paused = host.on_toggle_queue_paused.clone();
    let editing = host.editing;
    let on_cancel_edit = host.on_cancel_edit.clone();
    let on_replace_message = host.on_replace_message.clone();
    let on_goal_control = host.on_goal_control.clone();
    let needs_continue = host.needs_continue;
    let on_continue_turn = host.on_continue_turn.clone();
    let plan_open = signal(false);
    let on_open_runtime = host.on_open_runtime.clone();
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
            if editing.get_untracked() {
                on_replace_message(text);
            } else {
                send(text);
            }
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
        .bg(theme::block_surface)
        .pb_keyboard()
        .child(when(
            move || !attachments.with(|attachments| attachments.is_empty()),
            move || attachment_chips(attachments, on_remove_attachment.clone()),
        ))
        .child(when(move || needs_continue.get(), {
            let on_continue_turn = on_continue_turn.clone();
            move || {
                view()
                    .w_full()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .px(px(theme::SPACING_SM))
                    .py(px(theme::SPACING_XS))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .bg(theme::background_dim)
                    .child(
                        text(copy::text(
                            "The last turn was paused.",
                            "上一回合已暂停。",
                            "上一回合已暫停。",
                        ))
                        .flex_1()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_secondary),
                    )
                    .child(action::secondary(
                        copy::text("Continue", "继续", "繼續"),
                        icon(include_str!("../../assets/icons/send.svg")),
                        false,
                        {
                            let on_continue_turn = on_continue_turn.clone();
                            move || on_continue_turn()
                        },
                    ))
            }
        }))
        .child(when(move || editing.get(), {
            let on_cancel_edit = on_cancel_edit.clone();
            move || {
                view()
                    .w_full()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .px(px(theme::SPACING_SM))
                    .py(px(theme::SPACING_XS))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .bg(theme::background_dim)
                    .child(
                        text(copy::text(
                            "Replacing the last message",
                            "正在替换上一条消息",
                            "正在取代上一則訊息",
                        ))
                        .flex_1()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_secondary),
                    )
                    .child(small_action(copy::text("Cancel", "取消", "取消"), {
                        let on_cancel_edit = on_cancel_edit.clone();
                        move || on_cancel_edit()
                    }))
            }
        }))
        .child(when(move || schedule_open.get(), {
            let on_close_schedule = on_close_schedule.clone();
            let on_schedule = on_schedule.clone();
            move || schedule_picker(on_close_schedule.clone(), on_schedule.clone())
        }))
        .child(when(move || scheduled.get(), {
            let on_cancel_schedule = on_cancel_schedule.clone();
            move || {
                view()
                    .w_full()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .px(px(theme::SPACING_SM))
                    .py(px(theme::SPACING_XS))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .bg(theme::background_dim)
                    .child(
                        text(copy::text("Message scheduled", "消息已定时", "訊息已定時"))
                            .flex_1()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_secondary),
                    )
                    .child(small_action(copy::text("Cancel", "取消", "取消"), {
                        let on_cancel_schedule = on_cancel_schedule.clone();
                        move || on_cancel_schedule()
                    }))
            }
        }))
        .child(when(move || !queued.with(|queued| queued.is_empty()), {
            move || {
                queue_card(
                    queued,
                    queue_paused,
                    queue_manual,
                    on_toggle_queue_manual.clone(),
                    on_send_queued.clone(),
                    on_remove_queued.clone(),
                    on_move_queued.clone(),
                    on_clear_queue.clone(),
                    on_toggle_queue_paused.clone(),
                )
            }
        }))
        .child(when(
            move || show_plan_goal.get() && goal.get().is_some(),
            {
                let on_goal_control = on_goal_control.clone();
                move || goal_bar(goal, on_goal_control.clone())
            },
        ))
        .child(when(
            move || show_plan_goal.get() && plan.get().is_some(),
            move || plan_bar(plan, plan_open),
        ))
        .child(when(move || runtime_summary.get().is_some(), {
            let on_open_runtime = on_open_runtime.clone();
            move || {
                runtime_chip_row(
                    runtime_summary,
                    runtime_reasoning,
                    runtime_modes,
                    runtime_features,
                    token_usage,
                    show_token_usage,
                    on_open_runtime.clone(),
                )
            }
        }))
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
                            match skill_query(text) {
                                Some(query) => on_skill(&query),
                                None => skills.set(Vec::new()),
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
                    let on_attach_camera = host.on_attach_camera.clone();
                    action::icon_button(
                        icon(include_str!("../../assets/icons/circle-plus.svg")),
                        copy::text("Take a photo", "拍照", "拍照"),
                        move || on_attach_camera(),
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
                .child({
                    let on_open_schedule = on_open_schedule.clone();
                    action::icon_button(
                        icon(include_str!("../../assets/icons/history.svg")),
                        copy::text("Schedule message", "定时发送", "定時傳送"),
                        move || on_open_schedule(),
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
                            move || {
                                if editing.get() {
                                    copy::text("Replace", "替换", "取代")
                                } else {
                                    copy::text("Send", "发送", "傳送")
                                }
                            },
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

/// The prompts written ahead of the running turn.
fn schedule_picker(on_close: Action, on_schedule: Rc<dyn Fn(u64)>) -> impl IntoElement {
    let choices = [
        (
            copy::text("In 5 minutes", "5 分钟后", "5 分鐘後"),
            5 * 60u64,
        ),
        (
            copy::text("In 30 minutes", "30 分钟后", "30 分鐘後"),
            30 * 60u64,
        ),
        (copy::text("In 1 hour", "1 小时后", "1 小時後"), 60 * 60u64),
    ];
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_SM))
        .py(px(theme::SPACING_XS))
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(theme::background_dim)
        .child(
            text(copy::text("Send later", "稍后发送", "稍後傳送"))
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
        .child(each(
            move || {
                choices
                    .iter()
                    .map(|(label, seconds)| (*label, *seconds))
                    .collect::<Vec<_>>()
            },
            |(_, seconds)| *seconds,
            move |choice, _| {
                let (label, seconds) = choice.get_untracked();
                let on_schedule = on_schedule.clone();
                small_action(label, move || on_schedule(seconds))
            },
        ))
        .child(small_action(
            copy::text("Close", "关闭", "關閉"),
            move || on_close(),
        ))
}

#[allow(clippy::too_many_arguments)]
fn queue_card(
    queued: Signal<Vec<QueuedMessageView>>,
    paused: Signal<bool>,
    manual: Signal<bool>,
    on_toggle_manual: Action,
    on_send: Rc<dyn Fn(u64)>,
    on_remove: Rc<dyn Fn(u64)>,
    on_move: Rc<dyn Fn(u64, isize)>,
    on_clear: Action,
    on_toggle_paused: Action,
) -> impl IntoElement {
    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .p(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || {
                        format!(
                            "{} · {}",
                            copy::text("Queued", "队列", "佇列"),
                            queued.with(|queued| queued.len())
                        )
                    })
                    .flex_1()
                    .text_size(px(theme::font_caption()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_secondary),
                )
                .child(small_action_reactive(
                    move || {
                        if manual.get_untracked() {
                            copy::text("Auto", "自动", "自動")
                        } else {
                            copy::text("Manual", "手动", "手動")
                        }
                    },
                    move || on_toggle_manual(),
                ))
                .child(small_action_reactive(
                    move || {
                        if paused.get_untracked() {
                            copy::text("Resume", "继续", "繼續")
                        } else {
                            copy::text("Pause", "暂停", "暫停")
                        }
                    },
                    move || on_toggle_paused(),
                ))
                .child(small_action(
                    copy::text("Clear", "清空", "清空"),
                    move || on_clear(),
                )),
        )
        .child(each(
            move || queued.get(),
            |message| message.id,
            move |message, _| {
                let id = message.with(|message| message.id);
                let send = on_send.clone();
                let remove = on_remove.clone();
                let dragging = signal(false);
                view()
                    .w_full()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    .on_long_press(move || dragging.set(true))
                    .on_pan_end({
                        let on_move = on_move.clone();
                        move |event: &PanEvent| {
                            if !dragging.get_untracked() {
                                return;
                            }
                            dragging.set(false);
                            if event.translation.y.0 < -28.0 {
                                on_move(id, -1);
                            } else if event.translation.y.0 > 28.0 {
                                on_move(id, 1);
                            }
                        }
                    })
                    .child(
                        text(move || {
                            message.with(|message| {
                                if message.attachment_count > 0 {
                                    format!("{} (+{})", message.text, message.attachment_count)
                                } else {
                                    message.text.clone()
                                }
                            })
                        })
                        .flex_1()
                        .truncate()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_secondary),
                    )
                    .child({
                        let on_move = on_move.clone();
                        move || small_action("↑", move || on_move(id, -1))
                    }())
                    .child({
                        let on_move = on_move.clone();
                        move || small_action("↓", move || on_move(id, 1))
                    }())
                    .child(small_action(
                        copy::text("Send", "发送", "傳送"),
                        move || send(id),
                    ))
                    .child(small_action(
                        copy::text("Remove", "移除", "移除"),
                        move || remove(id),
                    ))
            },
        ))
}

/// The session's standing objective, above the plan it is worked through.
fn goal_bar(
    goal: Signal<Option<GoalView>>,
    on_control: Rc<dyn Fn(GoalControl)>,
) -> impl IntoElement {
    let pause = on_control.clone();
    let resume = on_control.clone();
    let clear = on_control;
    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .p(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || {
                        goal.with(|goal| {
                            goal.as_ref()
                                .map(|goal| goal.objective.clone())
                                .unwrap_or_default()
                        })
                    })
                    .flex_1()
                    .truncate()
                    .text_size(px(theme::font_detail()))
                    .text_color(theme::foreground),
                )
                .child(
                    text(move || {
                        goal.with(|goal| goal.as_ref().map(goal_phase_label).unwrap_or_default())
                    })
                    .text_size(px(theme::font_micro()))
                    .text_color(move || {
                        goal.with(|goal| {
                            goal.as_ref()
                                .map(goal_phase_color)
                                .unwrap_or_else(theme::text_muted)
                        })
                    }),
                )
                .child(small_action(
                    copy::text("Clear", "清除", "清除"),
                    move || clear(GoalControl::Clear),
                )),
        )
        .child(when(
            move || goal.with(|goal| goal.as_ref().is_some_and(|goal| goal.progress.is_some())),
            move || {
                text(move || {
                    goal.with(|goal| {
                        goal.as_ref()
                            .and_then(|goal| goal.progress.clone())
                            .unwrap_or_default()
                    })
                })
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted)
            },
        ))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .child(when(
                    move || goal.with(|goal| goal.as_ref().is_some_and(|goal| goal.can_pause)),
                    {
                        let pause = pause.clone();
                        move || {
                            let pause = pause.clone();
                            small_action(copy::text("Pause", "暂停", "暫停"), move || {
                                pause(GoalControl::Pause)
                            })
                        }
                    },
                ))
                .child(when(
                    move || goal.with(|goal| goal.as_ref().is_some_and(|goal| goal.can_resume)),
                    {
                        let resume = resume.clone();
                        move || {
                            let resume = resume.clone();
                            small_action(copy::text("Resume", "继续", "繼續"), move || {
                                resume(GoalControl::Resume)
                            })
                        }
                    },
                )),
        )
}

fn goal_phase_label(goal: &GoalView) -> String {
    match goal.phase {
        GoalPhaseView::Active => copy::text("Active", "进行中", "進行中"),
        GoalPhaseView::Paused => copy::text("Paused", "已暂停", "已暫停"),
        GoalPhaseView::Blocked => copy::text("Blocked", "受阻", "受阻"),
        GoalPhaseView::UsageLimited => copy::text("Limit reached", "已达限额", "已達限額"),
        GoalPhaseView::BudgetLimited => copy::text("Budget reached", "已达预算", "已達預算"),
        GoalPhaseView::Complete => copy::text("Complete", "已完成", "已完成"),
        GoalPhaseView::Unknown => copy::text("Unknown", "未知", "未知"),
    }
    .to_owned()
}

fn goal_phase_color(goal: &GoalView) -> Hsla {
    match goal.phase {
        GoalPhaseView::Active => theme::color_token("chart-1"),
        GoalPhaseView::Paused | GoalPhaseView::Blocked => theme::color_token("warning"),
        GoalPhaseView::UsageLimited | GoalPhaseView::BudgetLimited => theme::accent_red(),
        GoalPhaseView::Complete => theme::color_token("chart-2"),
        GoalPhaseView::Unknown => theme::text_muted(),
    }
}

/// The Agent's current plan, with its steps one tap away.
fn plan_bar(plan: Signal<Option<PlanView>>, open: Signal<bool>) -> impl IntoElement {
    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .p(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || {
                        plan.with(|plan| {
                            plan.as_ref()
                                .map(|plan| plan.title.clone())
                                .filter(|title| !title.trim().is_empty())
                                .unwrap_or_else(|| {
                                    copy::text("Current plan", "当前计划", "目前計劃").to_owned()
                                })
                        })
                    })
                    .flex_1()
                    .truncate()
                    .text_size(px(theme::font_detail()))
                    .text_color(theme::foreground),
                )
                .child(
                    text(move || {
                        plan.with(|plan| {
                            plan.as_ref()
                                .map(|plan| format!("{}/{}", plan.completed, plan.steps.len()))
                                .unwrap_or_default()
                        })
                    })
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
                )
                .child(small_action(
                    copy::text("Steps", "步骤", "步驟"),
                    move || open.update(|open| *open = !*open),
                )),
        )
        .child(
            text(move || {
                plan.with(|plan| {
                    plan.as_ref()
                        .and_then(|plan| plan.steps.get(plan.current))
                        .map(|step| step.title.clone())
                        .unwrap_or_default()
                })
            })
            .truncate()
            .text_size(px(theme::font_micro()))
            .text_color(theme::text_muted),
        )
        .child(when(
            move || open.get(),
            move || {
                view().w_full().flex_col().gap(px(2.0)).child(each(
                    move || {
                        plan.with(|plan| {
                            plan.as_ref()
                                .map(|plan| plan.steps.clone())
                                .unwrap_or_default()
                        })
                    },
                    |step| step.title.clone(),
                    move |step, _| {
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .child(
                                text(move || match step.with(|step| step.state) {
                                    PlanStepState::Completed => "✓",
                                    PlanStepState::Running => "◐",
                                    PlanStepState::Failed => "×",
                                    PlanStepState::Pending => "·",
                                })
                                .text_size(px(theme::font_micro()))
                                .text_color(move || {
                                    match step.with(|step| step.state) {
                                        PlanStepState::Completed => theme::color_token("chart-1"),
                                        PlanStepState::Running => theme::color_token("warning"),
                                        PlanStepState::Failed => theme::accent_red(),
                                        PlanStepState::Pending => theme::text_muted(),
                                    }
                                }),
                            )
                            .child(
                                text(move || step.with(|step| step.title.clone()))
                                    .flex_1()
                                    .truncate()
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_secondary),
                            )
                    },
                ))
            },
        ))
}

/// The composer's runtime chips: the model, and the Agent's own options.
fn runtime_chip_row(
    summary: Signal<Option<RuntimeSummaryView>>,
    reasoning: Signal<Vec<RuntimeChoiceView>>,
    modes: Signal<Vec<RuntimeChoiceView>>,
    features: Signal<Vec<RuntimeFeatureView>>,
    token_usage: Signal<Option<TokenUsageView>>,
    show_token: Signal<bool>,
    on_open: Action,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .child({
            let on_open = on_open.clone();
            button()
                .on_press(move || on_open())
                .flex_1()
                .min_w(px(0.0))
                .min_h(px(28.0))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .rounded(px(theme::RADIUS_CONTROL))
                .border_1()
                .border_color(theme::border_subtle)
                .bg(theme::background_dim)
                .px(px(theme::SPACING_SM))
                .active(|style| style.opacity(0.8))
                .child(
                    text(move || {
                        summary.with(|summary| {
                            summary
                                .as_ref()
                                .map(|summary| summary.model.clone())
                                .unwrap_or_else(|| copy::text("Model", "模型", "模型").to_owned())
                        })
                    })
                    .flex_1()
                    .truncate()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_secondary),
                )
        })
        .child(chip_button(
            move || {
                reasoning.with(|choices| {
                    choices
                        .iter()
                        .find(|choice| choice.selected)
                        .map(|choice| choice.label.clone())
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| copy::text("Reasoning", "推理", "推理").to_owned())
                })
            },
            on_open.clone(),
        ))
        .child(chip_button(
            move || {
                modes.with(|choices| {
                    choices
                        .iter()
                        .find(|choice| choice.selected)
                        .map(|choice| choice.label.clone())
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| copy::text("Mode", "模式", "模式").to_owned())
                })
            },
            on_open.clone(),
        ))
        .child(when(
            move || features.with(|features| !features.is_empty()),
            {
                let on_open = on_open.clone();
                move || {
                    chip_button(
                        move || copy::text("Options", "选项", "選項").to_owned(),
                        on_open.clone(),
                    )
                }
            },
        ))
        .child(when(
            move || show_token.get() && token_usage.get().is_some(),
            move || token_chip(token_usage),
        ))
}

fn token_chip(usage: Signal<Option<TokenUsageView>>) -> impl IntoElement {
    button()
        .on_press(move || {
            let usage = usage.get_untracked();
            spawn(async move {
                let body = token_usage_detail(usage.as_ref());
                let _ = nsh_base::alert_dialog()
                    .title(copy::text(
                        "Token usage",
                        "Token 使用情况",
                        "Token 使用情況",
                    ))
                    .description(body)
                    .show()
                    .await;
            });
        })
        .min_h(px(28.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_dim)
        .active(|style| style.opacity(0.8))
        .child(
            text(move || match usage.get() {
                Some(usage) => usage
                    .percent
                    .map(|percent| format!("{percent}%"))
                    .unwrap_or_else(|| compact_tokens(usage.input + usage.output)),
                None => "–".to_owned(),
            })
            .text_size(px(theme::font_micro()))
            .text_color(move || match usage.get() {
                Some(usage) => match usage.percent {
                    Some(percent) if percent > 90 => theme::accent_red(),
                    Some(percent) if percent >= 70 => theme::color_token("warning"),
                    _ => theme::text_muted(),
                },
                None => theme::text_muted(),
            }),
        )
}

fn token_usage_detail(usage: Option<&TokenUsageView>) -> String {
    let Some(usage) = usage else {
        return copy::text(
            "The runtime has not reported token usage yet.",
            "运行时尚未上报 token 使用情况。",
            "執行環境尚未回報 Token 使用情況。",
        )
        .to_owned();
    };
    if let Some(percent) = usage.percent.filter(|_| usage.size > 0) {
        format!(
            "{} {percent}% · {} / {}",
            copy::text("Context used", "上下文已用", "上下文已用"),
            compact_tokens(usage.used),
            compact_tokens(usage.size)
        )
    } else {
        format!(
            "{} {} · {} {}",
            copy::text("Input", "输入", "輸入"),
            compact_tokens(usage.input),
            copy::text("Output", "输出", "輸出"),
            compact_tokens(usage.output)
        )
    }
}

fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn chip_button(label: impl Fn() -> String + 'static, on_press: Action) -> ButtonElement {
    button()
        .on_press(move || on_press())
        .min_h(px(28.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_dim)
        .active(|style| style.opacity(0.8))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
}

/// The prompts already sent, one tap from the composer.
fn history_sheet(host: SessionHost) -> impl IntoElement {
    let open = host.history_open;
    let history = host.history;
    let on_close = host.on_close_history.clone();
    let on_close_sheet = on_close.clone();
    let on_recall = host.on_recall_history.clone();
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event
                && !next
            {
                on_close_sheet();
            }
        })
        .child(
            scroll().max_h(px(520.0)).child(
                view()
                    .w_full()
                    .gap(px(theme::SPACING_SM))
                    .p(px(theme::SPACING_MD))
                    .pb_safe_area_plus(px(theme::SPACING_LG))
                    .bg(theme::background_card)
                    .rounded_t(px(16.0))
                    .child(
                        text(copy::text("Earlier messages", "历史消息", "歷史訊息"))
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    )
                    .child(when(
                        move || history.with(|history| history.is_empty()),
                        || {
                            text(copy::text(
                                "No earlier prompts in this session.",
                                "这个会话还没有更早的消息。",
                                "這個工作階段還沒有更早的訊息。",
                            ))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::text_muted)
                        },
                    ))
                    .child(each(
                        move || history.get(),
                        |entry| entry.id.clone(),
                        move |entry, _| {
                            let recall = entry.with(|entry| entry.text.clone());
                            let on_recall = on_recall.clone();
                            button()
                                .on_press(move || on_recall(&recall))
                                .w_full()
                                .min_h(px(44.0))
                                .px(px(theme::SPACING_MD))
                                .py(px(theme::SPACING_XS))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .active(|style| style.bg(theme::background_dim()))
                                .items_start()
                                .child(
                                    text(move || entry.with(|entry| entry.text.clone()))
                                        .max_h(px(64.0))
                                        .truncate()
                                        .text_size(px(theme::font_body()))
                                        .text_color(theme::text_secondary),
                                )
                        },
                    ))
                    .child({
                        let on_close = on_close.clone();
                        action::secondary(
                            copy::text("Close", "关闭", "關閉"),
                            icon(include_str!("../../assets/icons/x.svg")),
                            false,
                            move || on_close(),
                        )
                        .w_full()
                    }),
            ),
        )
}

/// The runtime sheet: the model list and the Agent's own option groups.
fn runtime_sheet(host: SessionHost) -> impl IntoElement {
    let open = host.runtime_open;
    let on_close = host.on_close_runtime.clone();
    let on_close_sheet = on_close.clone();
    let summary = host.runtime_summary;
    let options = host.runtime_options;
    let reasoning = host.runtime_reasoning;
    let modes = host.runtime_modes;
    let features = host.runtime_features;
    let busy = host.runtime_busy;
    let on_choose_runtime = host.on_choose_runtime.clone();
    let on_choose_reasoning = host.on_choose_reasoning.clone();
    let on_choose_mode = host.on_choose_mode.clone();
    let on_choose_feature = host.on_choose_feature.clone();
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event
                && !next
            {
                on_close_sheet();
            }
        })
        .child(
            scroll().max_h(px(600.0)).child(
                view()
                    .w_full()
                    .gap(px(theme::SPACING_MD))
                    .p(px(theme::SPACING_MD))
                    .pb_safe_area_plus(px(theme::SPACING_LG))
                    .bg(theme::background_card)
                    .rounded_t(px(16.0))
                    .child(
                        text(move || {
                            summary.with(|summary| {
                                summary
                                    .as_ref()
                                    .map(|summary| format!("{} · {}", summary.agent, summary.model))
                                    .unwrap_or_else(|| {
                                        copy::text("Runtime", "运行时", "執行環境").to_owned()
                                    })
                            })
                        })
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                    )
                    .child(section_label(copy::text(
                        "Agent and model",
                        "Agent 与模型",
                        "Agent 與模型",
                    )))
                    .child(
                        view()
                            .w_full()
                            .flex_col()
                            .gap(px(theme::SPACING_XS))
                            .child(each(
                                move || options.get(),
                                |option| option.id.clone(),
                                move |option, _| {
                                    let id = option.with(|option| option.id.clone());
                                    let on_choose = on_choose_runtime.clone();
                                    button()
                                        .on_press(move || on_choose(&id))
                                        .w_full()
                                        .min_h(px(48.0))
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .border_1()
                                        .border_color(move || {
                                            if option.with(|option| option.selected) {
                                                theme::foreground()
                                            } else {
                                                theme::border_subtle()
                                            }
                                        })
                                        .bg(move || {
                                            if option.with(|option| option.selected) {
                                                theme::background_dim()
                                            } else {
                                                theme::background_card()
                                            }
                                        })
                                        .px(px(theme::SPACING_MD))
                                        .py(px(theme::SPACING_XS))
                                        .flex_col()
                                        .items_start()
                                        .disabled(move || {
                                            busy.get() || !option.with(|option| option.available)
                                        })
                                        .disabled_style(|style| style.opacity(0.45))
                                        .child(
                                            text(move || {
                                                option.with(|option| {
                                                    format!("{} · {}", option.agent, option.model)
                                                })
                                            })
                                            .truncate()
                                            .text_size(px(theme::font_detail()))
                                            .text_color(theme::foreground),
                                        )
                                        .child(
                                            text(move || {
                                                option.with(|option| option.detail.clone())
                                            })
                                            .truncate()
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_muted),
                                        )
                                },
                            )),
                    )
                    .child(choice_section(
                        copy::text("Reasoning", "推理", "推理"),
                        reasoning,
                        on_choose_reasoning,
                    ))
                    .child(choice_section(
                        copy::text("Mode", "模式", "模式"),
                        modes,
                        on_choose_mode,
                    ))
                    .child(feature_section(features, on_choose_feature))
                    .child({
                        let on_close = on_close.clone();
                        action::secondary(
                            copy::text("Close", "关闭", "關閉"),
                            icon(include_str!("../../assets/icons/x.svg")),
                            false,
                            move || on_close(),
                        )
                        .w_full()
                    }),
            ),
        )
}

fn section_label(label: &'static str) -> impl IntoElement {
    text(label)
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted)
}

fn choice_section(
    label: &'static str,
    choices: Signal<Vec<RuntimeChoiceView>>,
    on_choose: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(when(move || !choices.with(|choices| choices.is_empty()), {
            move || {
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(theme::SPACING_XS))
                    .child(section_label(label))
            }
        }))
        .child(each(
            move || choices.get(),
            |choice| choice.value.clone(),
            move |choice, _| {
                let value = choice.with(|choice| choice.value.clone());
                let on_choose = on_choose.clone();
                button()
                    .on_press(move || on_choose(&value))
                    .w_full()
                    .min_h(px(40.0))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(move || {
                        if choice.with(|choice| choice.selected) {
                            theme::foreground()
                        } else {
                            theme::border_subtle()
                        }
                    })
                    .px(px(theme::SPACING_MD))
                    .items_center()
                    .child(
                        text(move || choice.with(|choice| choice.label.clone()))
                            .text_size(px(theme::font_detail()))
                            .text_color(theme::foreground),
                    )
            },
        ))
}

fn feature_section(
    features: Signal<Vec<RuntimeFeatureView>>,
    on_choose: ChooseFeature,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(when(
            move || !features.with(|features| features.is_empty()),
            || section_label(copy::text("Session options", "会话选项", "工作階段選項")),
        ))
        .child(each(
            move || features.get(),
            |feature| feature.id.clone(),
            move |feature, _| {
                let id = feature.with(|feature| feature.id.clone());
                let toggle = feature.with(|feature| feature.toggle);
                let on_choose = on_choose.clone();
                if toggle {
                    let next = if feature.with(|feature| feature.selected) {
                        "false"
                    } else {
                        "true"
                    };
                    button()
                        .on_press(move || on_choose(&id, next))
                        .w_full()
                        .min_h(px(40.0))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme::border_subtle)
                        .px(px(theme::SPACING_MD))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .child(
                            text(move || feature.with(|feature| feature.label.clone()))
                                .flex_1()
                                .truncate()
                                .text_size(px(theme::font_detail()))
                                .text_color(theme::foreground),
                        )
                        .child(
                            text(move || {
                                if feature.with(|feature| feature.selected) {
                                    copy::text("On", "开", "開")
                                } else {
                                    copy::text("Off", "关", "關")
                                }
                            })
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                        )
                        .into_element()
                } else {
                    let values = feature.with(|feature| feature.values.clone());
                    view()
                        .w_full()
                        .flex_col()
                        .gap(px(theme::SPACING_XS))
                        .child(
                            text(move || feature.with(|feature| feature.label.clone()))
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                        )
                        .children(values.into_iter().map({
                            let on_choose = on_choose.clone();
                            move |value| {
                                let id = id.clone();
                                let choice = value.value.clone();
                                let on_choose = on_choose.clone();
                                button()
                                    .on_press(move || on_choose(&id, &choice))
                                    .w_full()
                                    .min_h(px(36.0))
                                    .px(px(theme::SPACING_MD))
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .bg(theme::background_dim)
                                    .items_center()
                                    .child(
                                        text(value.label)
                                            .text_size(px(theme::font_detail()))
                                            .text_color(theme::text_secondary),
                                    )
                            }
                        }))
                        .into_element()
                }
            },
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
#[allow(clippy::too_many_arguments)]
fn tools_sheet(
    open: Signal<bool>,
    on_files: Action,
    on_changes: Action,
    on_terminal: Action,
    on_team: Action,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                open.set(false);
            }
        })
        .child(
            view()
                .w_full()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(theme::RADIUS_SHEET))
                .child(sheet_grabber())
                .child(
                    view().w_full().pt(px(8.0)).pb(px(4.0)).child(
                        text(copy::text("Tools", "工具", "工具"))
                            .text_size(px(theme::font_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    ),
                )
                .child(
                    view()
                        .w_full()
                        .pt(px(4.0))
                        .child({
                            let action = on_files.clone();
                            sheet_action(
                                copy::text("Files", "文件", "檔案"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                None,
                                move || {
                                    open.set(false);
                                    action();
                                },
                            )
                        })
                        .child({
                            let action = on_changes.clone();
                            sheet_action(
                                copy::text("Changes", "变更", "變更"),
                                icon(include_str!("../../assets/icons/code-xml.svg")),
                                false,
                                None,
                                move || {
                                    open.set(false);
                                    action();
                                },
                            )
                        })
                        .child({
                            let action = on_terminal.clone();
                            sheet_action(
                                copy::text("Terminal", "终端", "終端"),
                                icon(include_str!("../../assets/icons/terminal.svg")),
                                false,
                                None,
                                move || {
                                    open.set(false);
                                    action();
                                },
                            )
                        })
                        .child({
                            let action = on_team.clone();
                            sheet_action(
                                copy::text("Team", "团队", "團隊"),
                                icon(include_str!("../../assets/icons/bot.svg")),
                                false,
                                None,
                                move || {
                                    open.set(false);
                                    action();
                                },
                            )
                        }),
                ),
        )
}

fn drawer_root_menu(
    open: Signal<bool>,
    on_new_session: Action,
    on_new_folder: Action,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                open.set(false);
            }
        })
        .child(
            view()
                .w_full()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(theme::RADIUS_SHEET))
                .child(sheet_grabber())
                .child(
                    view().w_full().pt(px(8.0)).pb(px(4.0)).child(
                        text(copy::text("Projects", "项目", "專案"))
                            .text_size(px(theme::font_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    ),
                )
                .child(
                    view()
                        .w_full()
                        .pt(px(4.0))
                        .child({
                            let on_new_session = on_new_session.clone();
                            sheet_action(
                                copy::text("New project", "新建项目", "新增專案"),
                                icon(include_str!("../../assets/icons/plus.svg")),
                                false,
                                None,
                                move || {
                                    open.set(false);
                                    on_new_session();
                                },
                            )
                        })
                        .child({
                            let on_new_folder = on_new_folder.clone();
                            sheet_action(
                                copy::text("New folder", "新建文件夹", "新增資料夾"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                None,
                                move || {
                                    open.set(false);
                                    on_new_folder();
                                },
                            )
                        }),
                ),
        )
}

fn sheet_grabber() -> impl IntoElement {
    view()
        .w(px(36.0))
        .h(px(4.0))
        .rounded(px(2.0))
        .bg(theme::muted_foreground().opacity(0.4))
}

fn sheet_action(
    label: impl IntoReactive<SharedString>,
    icon: ImageSrc,
    danger: bool,
    disabled: Option<Signal<bool>>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let ink: fn() -> Hsla = if danger {
        theme::accent_red
    } else {
        theme::foreground
    };
    button()
        .w_full()
        .h(px(48.0))
        .px(px(10.0))
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .rounded(px(10.0))
        .active(|style| style.bg(theme::background_dim()))
        .disabled(move || disabled.is_some_and(|busy| busy.get()))
        .disabled_style(|style| style.opacity(0.4))
        .on_press(on_press)
        .child(
            svg(icon)
                .w(px(18.0))
                .h(px(18.0))
                .flex_shrink_0()
                .text_color(ink),
        )
        .child(
            text(label)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_detail()))
                .text_color(ink),
        )
}

fn session_actions_sheet(
    actions: Signal<Option<String>>,
    rename: Signal<String>,
    busy: Signal<bool>,
    on_close: Rc<dyn Fn()>,
    on_rename: Rc<dyn Fn()>,
    on_move: Action,
    auto_continue: Signal<Vec<String>>,
    on_toggle_auto_continue: SessionToggle,
    pinned: Signal<Vec<String>>,
    on_toggle_pin: Rc<dyn Fn(&str)>,
    on_archive: Rc<dyn Fn()>,
    on_delete: Rc<dyn Fn()>,
    on_open_team: Action,
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
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(theme::RADIUS_SHEET))
                .child(sheet_grabber())
                .child(
                    view().w_full().pt(px(8.0)).pb(px(4.0)).child(
                        text(copy::text("Session", "会话", "工作階段"))
                            .text_size(px(theme::font_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    ),
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
                .child(
                    view()
                        .w_full()
                        .pt(px(4.0))
                        .child({
                            let on_move = on_move.clone();
                            sheet_action(
                                copy::text("Move to folder", "移动到文件夹", "移動到資料夾"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                None,
                                move || on_move(),
                            )
                        })
                        .child({
                            let on_open_team = on_open_team.clone();
                            sheet_action(
                                copy::text("Team", "团队", "團隊"),
                                icon(include_str!("../../assets/icons/bot.svg")),
                                false,
                                None,
                                move || on_open_team(),
                            )
                        })
                        .child({
                            let on_toggle_auto_continue = on_toggle_auto_continue.clone();
                            sheet_action(
                                move || {
                                    let enabled = actions.with(|id| {
                                        id.as_ref().is_some_and(|id| {
                                            auto_continue.with(|sessions| {
                                                sessions.iter().any(|candidate| candidate == id)
                                            })
                                        })
                                    });
                                    if enabled {
                                        copy::text(
                                            "Auto-continue on",
                                            "自动继续已开",
                                            "自動繼續已開",
                                        )
                                    } else {
                                        copy::text(
                                            "Auto-continue off",
                                            "自动继续已关",
                                            "自動繼續已關",
                                        )
                                    }
                                },
                                icon(include_str!("../../assets/icons/send.svg")),
                                false,
                                None,
                                move || {
                                    if let Some(id) = actions.get_untracked() {
                                        let enabled = auto_continue.with(|sessions| {
                                            sessions.iter().any(|candidate| candidate == &id)
                                        });
                                        on_toggle_auto_continue(&id, !enabled);
                                    }
                                },
                            )
                        })
                        .child({
                            let on_toggle_pin = on_toggle_pin.clone();
                            sheet_action(
                                move || {
                                    let pinned = actions.with(|id| {
                                        id.as_ref().is_some_and(|id| {
                                            pinned.with(|pinned| {
                                                pinned.iter().any(|candidate| candidate == id)
                                            })
                                        })
                                    });
                                    if pinned {
                                        copy::text("Unpin", "取消置顶", "取消置頂")
                                    } else {
                                        copy::text("Pin", "置顶", "置頂")
                                    }
                                },
                                icon(include_str!("../../assets/icons/check.svg")),
                                false,
                                None,
                                move || {
                                    if let Some(id) = actions.get_untracked() {
                                        on_toggle_pin(&id);
                                    }
                                },
                            )
                        })
                        .child({
                            let on_archive = on_archive.clone();
                            sheet_action(
                                copy::text("Archive", "归档", "封存"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                Some(busy),
                                move || on_archive(),
                            )
                        })
                        .child({
                            let on_delete = on_delete.clone();
                            sheet_action(
                                copy::text("Delete", "删除", "刪除"),
                                icon(include_str!("../../assets/icons/x.svg")),
                                true,
                                Some(busy),
                                move || on_delete(),
                            )
                        }),
                ),
        )
}

/// The session drawer, from the left edge.
fn move_session_sheet(
    open: Signal<Option<String>>,
    sections: Signal<Vec<SidebarSectionView>>,
    on_move: MoveSessionAction,
    on_close: Action,
) -> impl IntoElement {
    let folders = computed(move || {
        sections.with(|sections| {
            sections
                .iter()
                .filter(|section| section.kind == SidebarSectionKind::Folder)
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    sheet()
        .open(move || Some(open.get().is_some()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                on_close();
            }
        })
        .child(
            view()
                .w_full()
                .gap(px(theme::SPACING_XS))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Move to folder", "移动到文件夹", "移動到資料夾"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child({
                    let on_move = on_move.clone();
                    button()
                        .on_press(move || {
                            if let Some(id) = open.get_untracked() {
                                on_move(&id, None);
                            }
                        })
                        .w_full()
                        .min_h(px(44.0))
                        .px(px(theme::SPACING_SM))
                        .items_center()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .bg(theme::background_dim)
                        .child(
                            text(copy::text("No folder", "不放入文件夹", "不放入資料夾"))
                                .text_size(px(theme::font_body()))
                                .text_color(theme::foreground),
                        )
                })
                .child(each(
                    move || folders.get(),
                    |folder| folder.id.clone(),
                    move |folder, _| {
                        let on_move = on_move.clone();
                        button()
                            .on_press(move || {
                                if let Some(id) = open.get_untracked() {
                                    let folder = folder.get_untracked();
                                    on_move(&id, Some(&folder.id));
                                }
                            })
                            .w_full()
                            .min_h(px(44.0))
                            .px(px(theme::SPACING_SM))
                            .items_center()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::background_dim)
                            .child(
                                text(move || folder.get().name)
                                    .text_size(px(theme::font_body()))
                                    .text_color(theme::foreground),
                            )
                    },
                )),
        )
}

fn attachment_preview_sheet(
    preview: Signal<Option<AttachmentItem>>,
    close: Action,
) -> impl IntoElement {
    sheet()
        .side(Placement::Bottom)
        .open(move || Some(preview.with(Option::is_some)))
        .close_on_swipe(true)
        .on_open_change({
            let close = close.clone();
            move |event: &SheetEvent| {
                if let SheetEvent::OpenChange(false, _) = event {
                    close();
                }
            }
        })
        .child(
            view()
                .w_full()
                .max_h(px(560.0))
                .flex_col()
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .child(
                    view()
                        .w_full()
                        .h(px(48.0))
                        .flex_row()
                        .items_center()
                        .px(px(theme::SPACING_MD))
                        .gap(px(theme::SPACING_SM))
                        .border_b_1()
                        .border_b_color(theme::border_subtle)
                        .child(
                            text(copy::text("Attachment", "附件", "附件"))
                                .flex_1()
                                .text_size(px(theme::font_heading()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::foreground),
                        )
                        .child(action::icon_button(
                            icon(include_str!("../../assets/icons/x.svg")),
                            copy::text("Close", "关闭", "關閉"),
                            {
                                let close = close.clone();
                                move || close()
                            },
                        )),
                )
                .child(when(
                    move || {
                        preview.with(|preview| {
                            preview
                                .as_ref()
                                .is_some_and(|preview| preview.mime_type.starts_with("image/"))
                        })
                    },
                    move || {
                        view().p(px(theme::SPACING_MD)).items_center().child(
                            image(move || {
                                ImageSrc::from(preview.with(|preview| {
                                    preview
                                        .as_ref()
                                        .map(|preview| preview.uri.clone())
                                        .unwrap_or_default()
                                }))
                            })
                            .w_full()
                            .object_fit(nsh_core::ResizeMode::Contain),
                        )
                    },
                ))
                .child(when(
                    move || {
                        preview.with(|preview| {
                            preview
                                .as_ref()
                                .is_some_and(|preview| !preview.mime_type.starts_with("image/"))
                        })
                    },
                    move || {
                        text(move || {
                            preview.with(|preview| {
                                preview
                                    .as_ref()
                                    .map(|preview| preview.label.clone())
                                    .unwrap_or_default()
                            })
                        })
                        .px(px(theme::SPACING_MD))
                        .py(px(theme::SPACING_LG))
                        .text_size(px(theme::font_detail()))
                        .text_color(theme::text_secondary)
                    },
                )),
        )
}

/// The mark a session's Agent wears in the drawer.
fn agent_icon(agent: &str) -> &'static str {
    let agent = agent.to_ascii_lowercase();
    if agent.contains("claude") {
        include_str!("../../assets/icons/claude.svg")
    } else if agent.contains("codex") || agent.contains("openai") || agent.contains("gpt") {
        include_str!("../../assets/icons/openai.svg")
    } else if agent.contains("gemini") {
        include_str!("../../assets/icons/gemini.svg")
    } else if agent.contains("deepseek") {
        include_str!("../../assets/icons/deepseek.svg")
    } else {
        include_str!("../../assets/icons/bot.svg")
    }
}

fn drawer_view_tool(project_view: Signal<bool>, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .accessibility_label(copy::text("Switch view", "切换视图", "切換檢視"))
        .on_press(on_press)
        .size(px(32.0))
        .rounded(px(theme::RADIUS_CONTROL))
        .items_center()
        .justify_center()
        .bg(move || {
            if project_view.get() {
                theme::background_dim()
            } else {
                theme::background_card().opacity(0.0)
            }
        })
        .active(|style| style.opacity(0.65))
        .child(when(
            move || project_view.get(),
            || {
                svg(icon(include_str!("../../assets/icons/message-square.svg")))
                    .w(px(16.0))
                    .h(px(16.0))
                    .text_color(theme::text_secondary)
            },
        ))
        .child(when(
            move || !project_view.get(),
            || {
                svg(icon(include_str!("../../assets/icons/folder.svg")))
                    .w(px(16.0))
                    .h(px(16.0))
                    .text_color(theme::text_secondary)
            },
        ))
}

fn drawer_header_button(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(36.0))
        .rounded(px(theme::RADIUS_CONTROL))
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.6))
        .child(
            svg(icon)
                .w(px(18.0))
                .h(px(18.0))
                .text_color(theme::foreground),
        )
}

fn drawer_tool(
    icon: ImageSrc,
    label: &'static str,
    active: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(32.0))
        .rounded(px(theme::RADIUS_CONTROL))
        .items_center()
        .justify_center()
        .bg(if active {
            theme::background_dim()
        } else {
            theme::background_card().opacity(0.0)
        })
        .active(|style| style.opacity(0.65))
        .child(
            svg(icon)
                .w(px(16.0))
                .h(px(16.0))
                .text_color(theme::text_secondary),
        )
}

fn drawer(host: SessionHost, open: Signal<bool>) -> impl IntoElement {
    let close_drag = nsh_core::element::animated_value(0.0);
    let sessions = host.sessions;
    let active = host.active;
    let activity_phase = signal(0u32);
    if !crate::motion::reduce_motion() {
        spawn(async move {
            let mut frame = 0;
            loop {
                nsh_core::element::timer(std::time::Duration::from_millis(100)).await;
                frame = (frame + 1) % 8;
                activity_phase.set(frame);
            }
        });
    }
    let collapsed_projects = host.collapsed_projects;
    let on_set_project_collapsed = host.on_set_project_collapsed.clone();
    let collapsed_workspaces = host.collapsed_workspaces;
    let on_set_workspace_collapsed = host.on_set_workspace_collapsed.clone();
    let search_open = signal(false);
    let query = signal(String::new());
    let project_view = signal(false);
    let root_menu = signal(false);
    let select_mode = signal(false);
    let selected = signal(Vec::<String>::new());
    let pinned = host.pinned;
    let sidebar_sections = host.sidebar_sections;
    let unread = host.unread;
    let project_order = host.sidebar_project_order;
    let workspace_order = host.sidebar_workspace_order;
    let session_order = host.sidebar_session_order;
    let folder_open = host.folder_open;
    let folder_name = host.folder_name;
    let on_open_subfolder = host.on_open_subfolder.clone();
    let on_close_folder = host.on_close_folder.clone();
    let on_create_folder = host.on_create_folder.clone();
    let on_rename_folder = host.on_rename_folder.clone();
    let on_delete_folder = host.on_delete_folder.clone();
    let folder_actions = signal(None::<String>);
    let folder_rename = signal(String::new());
    let group_actions = signal(None::<String>);
    let group_rename = signal(String::new());
    let members_group = signal(None::<String>);
    let members_selected = signal(Vec::<String>::new());
    let on_set_group_members = host.on_set_group_members.clone();
    let on_set_group_pinned = host.on_set_group_pinned.clone();
    let on_move_session_relative = host.on_move_session_relative.clone();
    let on_rename_group = host.on_rename_group.clone();
    let on_delete_group = host.on_delete_group.clone();
    let on_toggle_folder_collapsed = host.on_toggle_folder_collapsed.clone();
    let on_toggle_group_collapsed = host.on_toggle_group_collapsed.clone();

    let on_collapse_all = {
        let sessions = sessions;
        let sections = sidebar_sections;
        let collapsed = collapsed_projects;
        let on_project = on_set_project_collapsed.clone();
        let on_folder = on_toggle_folder_collapsed.clone();
        let on_group = on_toggle_group_collapsed.clone();
        move || {
            let project_ids = sessions.with(|sessions| {
                sessions
                    .iter()
                    .map(|session| session.project_id.clone())
                    .collect::<std::collections::BTreeSet<_>>()
            });
            let rows = sections.get_untracked();
            let any_open = project_ids
                .iter()
                .any(|id| !collapsed.with(|collapsed| collapsed.contains(id)))
                || rows.iter().any(|section| !section.collapsed);
            for id in project_ids {
                on_project(&id, any_open);
            }
            for section in rows {
                if section.kind == SidebarSectionKind::Folder {
                    on_folder(&section.id, any_open);
                } else {
                    on_group(&section.id, any_open);
                }
            }
        }
    };
    let on_locate = {
        let sessions = sessions;
        let active = active;
        let on_project = on_set_project_collapsed.clone();
        move || {
            let Some(active) = active.get_untracked() else {
                return;
            };
            let project = sessions.with(|sessions| {
                sessions
                    .iter()
                    .find(|session| session.id == active)
                    .map(|session| session.project_id.clone())
            });
            if let Some(project) = project {
                on_project(&project, false);
            }
        }
    };
    let group_open = signal(false);
    let group_name = signal(String::new());
    let on_create_group = host.on_create_group.clone();
    let hidden_sessions = computed(move || {
        sidebar_sections.with(|sections| {
            sections
                .iter()
                .flat_map(|section| section.session_ids.iter().cloned())
                .collect::<Vec<_>>()
        })
    });
    let runtime = host.runtime;
    let on_select = host.on_select.clone();
    let on_new_session = host.on_new_session.clone();
    let on_switch_runtime = host.on_switch_runtime.clone();
    let on_session_menu = host.on_session_menu.clone();
    let on_open_settings = host.on_open_settings.clone();
    let section_rows = {
        let sections = sidebar_sections;
        let sessions = sessions;
        let active = active;
        let activity_phase = activity_phase;
        let project_view = project_view;
        let on_select = on_select.clone();
        let on_session_menu = on_session_menu.clone();
        let on_move_session_relative = on_move_session_relative.clone();
        let on_toggle_folder_collapsed = on_toggle_folder_collapsed.clone();
        let on_toggle_group_collapsed = on_toggle_group_collapsed.clone();
        let folder_actions = folder_actions;
        let folder_rename = folder_rename;
        let group_actions = group_actions;
        let group_rename = group_rename;
        Rc::new(move |project_id: &str| -> AnyElement {
            let on_select = on_select.clone();
            let on_session_menu = on_session_menu.clone();
            let on_move_session_relative = on_move_session_relative.clone();
            let on_toggle_folder_collapsed = on_toggle_folder_collapsed.clone();
            let on_toggle_group_collapsed = on_toggle_group_collapsed.clone();
            let project_id = project_id.to_owned();
            each(
                move || {
                    sections.with(|sections| {
                        sections
                            .iter()
                            .filter(|section| {
                                section.project_id.as_deref() == Some(project_id.as_str())
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                },
                |section| section.id.clone(),
                move |section, _| {
                    sidebar_section(
                        section,
                        sessions,
                        active,
                        activity_phase,
                        project_view,
                        on_select.clone(),
                        on_session_menu.clone(),
                        {
                            let folder_actions = folder_actions;
                            let folder_rename = folder_rename;
                            Rc::new(move |id: &str, name: &str| {
                                folder_rename.set(name.to_owned());
                                folder_actions.set(Some(id.to_owned()));
                            })
                        },
                        {
                            let callback = on_toggle_folder_collapsed.clone();
                            Rc::new(move |id: &str, collapsed: bool| callback(id, collapsed))
                        },
                        {
                            let callback = on_toggle_group_collapsed.clone();
                            Rc::new(move |id: &str, collapsed: bool| callback(id, collapsed))
                        },
                        {
                            let group_actions = group_actions;
                            let group_rename = group_rename;
                            Rc::new(move |id: &str, name: &str| {
                                group_rename.set(name.to_owned());
                                group_actions.set(Some(id.to_owned()));
                            })
                        },
                        open,
                        select_mode,
                        selected,
                        pinned,
                        unread,
                        on_move_session_relative.clone(),
                    )
                },
            )
        })
    };
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
                .w_full()
                .h_full()
                .relative()
                .flex_col()
                .bg(theme::background_card)
                .pan_values(Some(&close_drag), None)
                .pan_enabled(move || open.get())
                .on_pan_start(|_: &PanEvent| {})
                .on_pan_end(move |event: &PanEvent| {
                    if !event.cancelled && event.translation.x.0 < -50.0 {
                        open.set(false);
                    }
                })
                .pt_safe_area()
                .pb_safe_area_plus(px(theme::SPACING_MD))
                .child(
                    view()
                        .h(px(56.0))
                        .px(px(theme::SPACING_LG))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex_row()
                                .items_center()
                                .gap(px(1.0))
                                .child({
                                    let mark = theme::wordmark_mark(theme::font_title());
                                    svg(icon(include_str!("../../assets/icons/vibex-mark.svg")))
                                        .w(px(mark.width))
                                        .h(px(mark.height))
                                        .mt(px(mark.margin_top))
                                        .relative()
                                        .top(px(mark.top_offset))
                                        .flex_shrink_0()
                                        .text_color(theme::foreground)
                                })
                                .child(
                                    text("ibex")
                                        .text_size(px(theme::font_title()))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::foreground),
                                ),
                        )
                        .child({
                            let on_new_session = on_new_session.clone();
                            drawer_header_button(
                                icon(include_str!("../../assets/icons/plus.svg")),
                                copy::text("New session", "新建会话", "新增工作階段"),
                                move || on_new_session(),
                            )
                        })
                        .child(drawer_header_button(
                            icon(include_str!("../../assets/icons/chevron-right.svg")),
                            copy::text("Back to session", "返回会话", "返回工作階段"),
                            move || open.set(false),
                        )),
                )
                .child(
                    view()
                        .h(px(40.0))
                        .flex_shrink_0()
                        .px(px(theme::SPACING_LG))
                        .border_t_1()
                        .border_t_color(theme::border_default())
                        .border_b_1()
                        .border_b_color(theme::border_default())
                        .flex_row()
                        .items_center()
                        .gap(px(2.0))
                        .child(
                            text(copy::text("Projects", "项目", "專案"))
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(theme::font_caption()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text_muted),
                        )
                        .child(drawer_view_tool(project_view, move || {
                            project_view.update(|view| *view = !*view);
                        }))
                        .child(drawer_tool(
                            icon(include_str!("../../assets/icons/list-checks.svg")),
                            copy::text("Select sessions", "选择会话", "選取工作階段"),
                            false,
                            move || {
                                let next = !select_mode.get_untracked();
                                select_mode.set(next);
                                if !next {
                                    selected.set(Vec::new());
                                }
                            },
                        ))
                        .child(drawer_tool(
                            icon(include_str!("../../assets/icons/chevrons-right-left.svg")),
                            copy::text(
                                "Collapse or expand projects",
                                "折叠或展开项目",
                                "摺疊或展開專案",
                            ),
                            false,
                            on_collapse_all,
                        ))
                        .child(drawer_tool(
                            icon(include_str!("../../assets/icons/crosshair.svg")),
                            copy::text(
                                "Locate current session",
                                "定位当前会话",
                                "定位目前工作階段",
                            ),
                            false,
                            on_locate,
                        ))
                        .child(drawer_tool(
                            icon(include_str!("../../assets/icons/plus.svg")),
                            copy::text("New project", "新建项目", "新增專案"),
                            false,
                            move || root_menu.set(true),
                        ))
                        .child(drawer_tool(
                            icon(include_str!("../../assets/icons/search.svg")),
                            copy::text("Search sessions", "搜索会话", "搜尋工作階段"),
                            false,
                            move || {
                                search_open.update(|open| *open = !*open);
                                if !search_open.get_untracked() {
                                    query.set(String::new());
                                }
                            },
                        )),
                )
                .child(when(move || select_mode.get(), {
                    let on_archive_sessions = host.on_archive_sessions.clone();
                    let on_delete_sessions = host.on_delete_sessions.clone();
                    move || {
                        let on_archive_sessions = on_archive_sessions.clone();
                        let on_delete_sessions = on_delete_sessions.clone();
                        view()
                            .w_full()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .px(px(theme::SPACING_LG))
                            .pb(px(theme::SPACING_XS))
                            .child(
                                text(move || {
                                    format!(
                                        "{} {}",
                                        selected.with(|selected| selected.len()),
                                        copy::text("selected", "已选择", "已選取")
                                    )
                                })
                                .flex_1()
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                            )
                            .child(small_action(copy::text("Group", "分组", "分組"), {
                                move || {
                                    if !selected.with(|selected| selected.is_empty()) {
                                        group_open.set(true);
                                    }
                                }
                            }))
                            .child(small_action(copy::text("Archive", "归档", "封存"), {
                                let on_archive_sessions = on_archive_sessions.clone();
                                move || {
                                    let ids = selected.get_untracked();
                                    if !ids.is_empty() {
                                        on_archive_sessions(&ids);
                                    }
                                    selected.set(Vec::new());
                                }
                            }))
                            .child(small_action(copy::text("Delete", "删除", "刪除"), {
                                let on_delete_sessions = on_delete_sessions.clone();
                                move || {
                                    let ids = selected.get_untracked();
                                    if !ids.is_empty() {
                                        on_delete_sessions(&ids);
                                    }
                                    selected.set(Vec::new());
                                }
                            }))
                            .child(small_action(copy::text("Done", "完成", "完成"), {
                                move || {
                                    select_mode.set(false);
                                    selected.set(Vec::new());
                                }
                            }))
                    }
                }))
                .child(group_actions_sheet(
                    group_actions,
                    group_rename,
                    on_rename_group.clone(),
                    on_delete_group.clone(),
                    {
                        let on_set_group_pinned = on_set_group_pinned.clone();
                        let sections = sidebar_sections;
                        Rc::new(move || {
                            if let Some(id) = group_actions.get_untracked() {
                                let pinned = sections.with(|sections| {
                                    sections
                                        .iter()
                                        .find(|section| section.id == id)
                                        .is_some_and(|section| section.pinned)
                                });
                                on_set_group_pinned(&id, !pinned);
                                group_actions.set(None);
                            }
                        })
                    },
                    {
                        let sections = sidebar_sections;
                        Rc::new(move || {
                            if let Some(id) = group_actions.get_untracked() {
                                let members = sections.with(|sections| {
                                    sections
                                        .iter()
                                        .find(|section| section.id == id)
                                        .map(|section| section.session_ids.clone())
                                        .unwrap_or_default()
                                });
                                members_selected.set(members);
                                members_group.set(Some(id));
                                group_actions.set(None);
                            }
                        })
                    },
                ))
                .child(group_members_sheet(
                    members_group,
                    members_selected,
                    sessions,
                    on_set_group_members.clone(),
                ))
                .child(group_sheet(
                    group_open,
                    group_name,
                    on_create_group.clone(),
                    selected,
                ))
                .child(folder_actions_sheet(
                    folder_actions,
                    folder_rename,
                    on_rename_folder.clone(),
                    on_delete_folder.clone(),
                    {
                        let on_open_subfolder = on_open_subfolder.clone();
                        Rc::new(move || {
                            if let Some(id) = folder_actions.get_untracked() {
                                folder_actions.set(None);
                                on_open_subfolder(&id);
                            }
                        })
                    },
                ))
                .child(folder_sheet(
                    folder_open,
                    folder_name,
                    on_close_folder.clone(),
                    on_create_folder.clone(),
                ))
                .child(
                    view().flex_1().min_h(px(0.0)).child(
                        scroll().flex_1().child(
                            view()
                                .gap(px(theme::SPACING_XS))
                                .px(px(theme::SPACING_SM))
                                .child(when(move || search_open.get(), {
                                    move || {
                                        view()
                                            .px(px(theme::SPACING_XS))
                                            .pb(px(theme::SPACING_XS))
                                            .child(field::text_field(
                                                query,
                                                copy::text(
                                                    "Search sessions",
                                                    "搜索会话",
                                                    "搜尋工作階段",
                                                ),
                                            ))
                                    }
                                }))
                                .child({
                                    let group_move = on_move_session_relative.clone();
                                    each(
                                        move || {
                                            drawer_groups(
                                                sessions,
                                                sidebar_sections,
                                                query,
                                                host.pinned,
                                                hidden_sessions,
                                                project_order,
                                                workspace_order,
                                                session_order,
                                                collapsed_workspaces,
                                            )
                                        },
                                        |group| group.id.clone(),
                                        move |group, _| {
                                            let collapsed = collapsed_projects;
                                            let on_select = on_select.clone();
                                            let on_menu = on_session_menu.clone();
                                            drawer_group(
                                                group,
                                                active,
                                                activity_phase,
                                                project_view,
                                                section_rows.clone(),
                                                collapsed,
                                                on_set_project_collapsed.clone(),
                                                on_new_session.clone(),
                                                on_select.clone(),
                                                on_menu.clone(),
                                                open,
                                                select_mode,
                                                selected,
                                                pinned,
                                                unread,
                                                group_move.clone(),
                                                collapsed_workspaces,
                                                on_set_workspace_collapsed.clone(),
                                            )
                                        },
                                    )
                                }),
                        ),
                    ),
                )
                .child(
                    view()
                        .border_t_1()
                        .border_t_color(theme::border_subtle)
                        .px(px(theme::SPACING_LG))
                        .py(px(10.0))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .child({
                            let on_switch = on_switch_runtime.clone();
                            button()
                                .flex_1()
                                .min_h(px(40.0))
                                .min_w(px(0.0))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_SM))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .active(|style| style.opacity(0.7))
                                .accessibility_label(copy::text(
                                    "Switch runtime",
                                    "切换运行时",
                                    "切換執行環境",
                                ))
                                .on_press(move || {
                                    open.set(false);
                                    on_switch()
                                })
                                .child(
                                    view()
                                        .w(px(8.0))
                                        .h(px(8.0))
                                        .flex_shrink_0()
                                        .rounded(px(4.0))
                                        .bg(move || {
                                            if runtime.get().is_some() {
                                                theme::color_token("chart-1")
                                            } else {
                                                theme::text_muted()
                                            }
                                        }),
                                )
                                .child(
                                    text(move || {
                                        runtime.get().unwrap_or_else(|| {
                                            copy::text(
                                                "No runtime",
                                                "未连接运行时",
                                                "未連線執行環境",
                                            )
                                            .into()
                                        })
                                    })
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme::font_detail()))
                                    .text_color(theme::text_secondary),
                                )
                        })
                        .child({
                            let on_open_settings = on_open_settings.clone();
                            button()
                                .size(px(40.0))
                                .flex_shrink_0()
                                .items_center()
                                .justify_center()
                                .rounded(px(20.0))
                                .active(|style| style.opacity(0.6))
                                .accessibility_label(copy::text("Settings", "设置", "設定"))
                                .on_press(move || {
                                    open.set(false);
                                    on_open_settings()
                                })
                                .child(
                                    svg(icon(include_str!("../../assets/icons/settings.svg")))
                                        .w(px(20.0))
                                        .h(px(20.0))
                                        .text_color(theme::text_secondary),
                                )
                        }),
                )
                .child(drawer_root_menu(
                    root_menu,
                    host.on_new_session.clone(),
                    host.on_open_folder.clone(),
                ))
                .child(
                    view()
                        .absolute()
                        .right(px(0.0))
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .w(px(24.0))
                        .pan_values(Some(&close_drag), None)
                        .pan_enabled(move || open.get())
                        .on_pan_start(|_: &PanEvent| {})
                        .on_pan_end(move |event: &PanEvent| {
                            if !event.cancelled && event.translation.x.0 < -40.0 {
                                open.set(false);
                            }
                        }),
                ),
        )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DrawerGroup {
    id: String,
    name: String,
    count: usize,
    rows: Vec<DrawerRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DrawerRow {
    Header {
        id: String,
        name: String,
        workspace: Option<String>,
    },
    Session(SessionSummary),
}

impl DrawerRow {
    fn key(&self) -> String {
        match self {
            DrawerRow::Header { id, .. } => format!("header:{id}"),
            DrawerRow::Session(session) => session.id.clone(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drawer_groups(
    sessions: Signal<Vec<SessionSummary>>,
    sections: Signal<Vec<SidebarSectionView>>,
    query: Signal<String>,
    pinned: Signal<Vec<String>>,
    hidden: Memo<Vec<String>>,
    project_order: Signal<Vec<String>>,
    workspace_order: Signal<Vec<(String, Vec<String>)>>,
    session_order: Signal<Vec<String>>,
    collapsed_workspaces: Signal<Vec<String>>,
) -> Vec<DrawerGroup> {
    sessions.with(|sessions| {
        let pinned_ids = pinned.get_untracked();
        let hidden_ids = hidden.get_untracked();
        let project_rank = project_order.get_untracked();
        let workspace_rank = workspace_order.get_untracked();
        let session_rank = session_order.get_untracked();
        let collapsed_workspaces = collapsed_workspaces.get_untracked();
        let query = query.with(|query| query.trim().to_lowercase());
        let _ = &query;
        let project_totals = sessions.iter().fold(
            std::collections::BTreeMap::<String, usize>::new(),
            |mut totals, session| {
                *totals.entry(session.project_id.clone()).or_default() += 1;
                totals
            },
        );
        let mut groups: Vec<DrawerGroup> = Vec::new();
        for session in sessions {
            if hidden_ids.iter().any(|id| id == &session.id) {
                continue;
            }
            if !query.is_empty()
                && !session.title.to_lowercase().contains(&query)
                && !session.agent.to_lowercase().contains(&query)
                && !session.project_name.to_lowercase().contains(&query)
                && !session.workspace_label.to_lowercase().contains(&query)
            {
                continue;
            }
            match groups
                .iter_mut()
                .find(|group| group.id == session.project_id)
            {
                Some(group) => group.rows.push(DrawerRow::Session(session.clone())),
                None => groups.push(DrawerGroup {
                    id: session.project_id.clone(),
                    name: if session.project_name.trim().is_empty() {
                        session.project_id.clone()
                    } else {
                        session.project_name.clone()
                    },
                    count: 0,
                    rows: vec![DrawerRow::Session(session.clone())],
                }),
            }
        }
        let project_names = sessions
            .iter()
            .map(|session| (session.project_id.clone(), session.project_name.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        for section in sections.get_untracked() {
            let Some(project_id) = section.project_id else {
                continue;
            };
            if groups.iter().any(|group| group.id == project_id) {
                continue;
            }
            groups.push(DrawerGroup {
                id: project_id.clone(),
                name: project_names
                    .get(&project_id)
                    .cloned()
                    .unwrap_or(project_id),
                count: 0,
                rows: Vec::new(),
            });
        }
        for group in &mut groups {
            let workspace_ranks = workspace_rank
                .iter()
                .find(|(project, _)| project == &group.id)
                .map(|(_, workspaces)| workspaces.clone())
                .unwrap_or_default();
            let mut sessions = group
                .rows
                .drain(..)
                .filter_map(|row| match row {
                    DrawerRow::Session(session) => Some(session),
                    DrawerRow::Header { .. } => None,
                })
                .collect::<Vec<_>>();
            sessions.sort_by_key(|session| {
                let pinned = !pinned_ids.iter().any(|id| id == &session.id);
                let workspace_rank = workspace_ranks
                    .iter()
                    .position(|id| id == &session.workspace_id)
                    .unwrap_or(usize::MAX);
                let rank = session_rank
                    .iter()
                    .position(|id| id == &session.id)
                    .unwrap_or(usize::MAX);
                (pinned, workspace_rank, rank)
            });
            group.count = project_totals
                .get(&group.id)
                .copied()
                .unwrap_or(sessions.len());
            let mut rows = Vec::new();
            let mut current: Option<String> = None;
            for session in sessions {
                let key = if session.workspace_id.trim().is_empty() {
                    session.workspace_label.clone()
                } else {
                    session.workspace_id.clone()
                };
                let label = if session.workspace_label.trim().is_empty() {
                    copy::text("Workspace", "工作区", "工作區").to_owned()
                } else {
                    session.workspace_label.clone()
                };
                if current.as_ref() != Some(&key) {
                    rows.push(DrawerRow::Header {
                        id: format!("{}:{key}", group.id),
                        name: label,
                        workspace: Some(key.clone()),
                    });
                    current = Some(key.clone());
                }
                if !collapsed_workspaces.iter().any(|id| id == &key) {
                    rows.push(DrawerRow::Session(session));
                }
            }
            group.rows = rows;
        }
        groups.sort_by_key(|group| {
            project_rank
                .iter()
                .position(|id| id == &group.id)
                .unwrap_or(usize::MAX)
        });
        groups
    })
}

fn group_actions_sheet(
    action: Signal<Option<String>>,
    name: Signal<String>,
    on_rename: FolderNameAction,
    on_delete: Rc<dyn Fn(&str)>,
    on_pin: Action,
    on_members: Action,
) -> impl IntoElement {
    let open = computed(move || action.get().is_some());
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                action.set(None);
            }
        })
        .child(
            view()
                .w_full()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(theme::RADIUS_SHEET))
                .child(sheet_grabber())
                .child(
                    view().w_full().pt(px(8.0)).pb(px(4.0)).child(
                        text(copy::text("Session group", "会话分组", "工作階段分組"))
                            .text_size(px(theme::font_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    ),
                )
                .child(field::text_field(
                    name,
                    copy::text("Group name", "分组名称", "分組名稱"),
                ))
                .child({
                    let on_rename = on_rename.clone();
                    action::primary(
                        copy::text("Rename", "重命名", "重新命名"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        move || {
                            if let Some(id) = action.get_untracked()
                                && !name.with(|name| name.trim().is_empty())
                            {
                                on_rename(&id, &name.get_untracked());
                                action.set(None);
                            }
                        },
                    )
                    .w_full()
                })
                .child(
                    view()
                        .w_full()
                        .pt(px(4.0))
                        .child({
                            let on_pin = on_pin.clone();
                            sheet_action(
                                copy::text("Pin group", "置顶分组", "置頂分組"),
                                icon(include_str!("../../assets/icons/check.svg")),
                                false,
                                None,
                                move || on_pin(),
                            )
                        })
                        .child({
                            let on_members = on_members.clone();
                            sheet_action(
                                copy::text("Members", "成员", "成員"),
                                icon(include_str!("../../assets/icons/bot.svg")),
                                false,
                                None,
                                move || on_members(),
                            )
                        })
                        .child({
                            let on_delete = on_delete.clone();
                            sheet_action(
                                copy::text("Delete", "删除", "刪除"),
                                icon(include_str!("../../assets/icons/x.svg")),
                                true,
                                None,
                                move || {
                                    if let Some(id) = action.get_untracked() {
                                        on_delete(&id);
                                        action.set(None);
                                    }
                                },
                            )
                        }),
                ),
        )
}

fn group_members_sheet(
    group: Signal<Option<String>>,
    members: Signal<Vec<String>>,
    sessions: Signal<Vec<SessionSummary>>,
    on_save: CreateGroupAction,
) -> impl IntoElement {
    sheet()
        .open(move || Some(group.get().is_some()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                group.set(None);
            }
        })
        .child(
            view()
                .w_full()
                .max_h(px(520.0))
                .gap(px(theme::SPACING_XS))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Group members", "分组成员", "分組成員"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(scroll().max_h(px(360.0)).child(
                    view().w_full().flex_col().gap(px(2.0)).child(each(
                        move || sessions.get(),
                        |session| session.id.clone(),
                        move |session, _| {
                            let id = session.get_untracked().id;
                            let checked = members.with(|members| members.contains(&id));
                            button()
                                .on_press(move || {
                                    members.update(|members| {
                                        if let Some(index) =
                                            members.iter().position(|candidate| candidate == &id)
                                        {
                                            members.remove(index);
                                        } else {
                                            members.push(id.clone());
                                        }
                                    });
                                })
                                .w_full()
                                .min_h(px(40.0))
                                .px(px(theme::SPACING_SM))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_SM))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .bg(theme::background_dim)
                                .child(
                                    text(if checked { "✓" } else { "·" })
                                        .text_size(px(theme::font_caption()))
                                        .text_color(theme::text_secondary),
                                )
                                .child(
                                    text(move || session.get().title)
                                        .flex_1()
                                        .truncate()
                                        .text_size(px(theme::font_body()))
                                        .text_color(theme::foreground),
                                )
                        },
                    )),
                ))
                .child({
                    let on_save = on_save.clone();
                    action::primary(
                        copy::text("Save", "保存", "儲存"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        move || {
                            if let Some(group_id) = group.get_untracked() {
                                let ids = members.get_untracked();
                                on_save(&group_id, &ids);
                                group.set(None);
                            }
                        },
                    )
                    .w_full()
                }),
        )
}

fn group_sheet(
    open: Signal<bool>,
    name: Signal<String>,
    on_create: CreateGroupAction,
    members: Signal<Vec<String>>,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                open.set(false);
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
                    text(copy::text("New group", "新建分组", "新增分組"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(field::text_field(
                    name,
                    copy::text("Group name", "分组名称", "分組名稱"),
                ))
                .child({
                    let on_create = on_create.clone();
                    action::primary(
                        copy::text("Create", "创建", "建立"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        move || {
                            let ids = members.get_untracked();
                            let group = name.get_untracked();
                            if !ids.is_empty() && !group.trim().is_empty() {
                                on_create(&group, &ids);
                                name.set(String::new());
                                open.set(false);
                            }
                        },
                    )
                    .w_full()
                }),
        )
}

fn folder_actions_sheet(
    action: Signal<Option<String>>,
    name: Signal<String>,
    on_rename: FolderNameAction,
    on_delete: Rc<dyn Fn(&str)>,
    on_new_subfolder: Action,
) -> impl IntoElement {
    let open = computed(move || action.get().is_some());
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                action.set(None);
            }
        })
        .child(
            view()
                .w_full()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(theme::RADIUS_SHEET))
                .child(sheet_grabber())
                .child(
                    view().w_full().pt(px(8.0)).pb(px(4.0)).child(
                        text(copy::text("Folder", "文件夹", "資料夾"))
                            .text_size(px(theme::font_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    ),
                )
                .child(field::text_field(
                    name,
                    copy::text("Folder name", "文件夹名称", "資料夾名稱"),
                ))
                .child({
                    let on_rename = on_rename.clone();
                    action::primary(
                        copy::text("Rename", "重命名", "重新命名"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        move || {
                            if let Some(id) = action.get_untracked()
                                && !name.with(|name| name.trim().is_empty())
                            {
                                on_rename(&id, &name.get_untracked());
                                action.set(None);
                            }
                        },
                    )
                    .w_full()
                })
                .child(
                    view()
                        .w_full()
                        .pt(px(4.0))
                        .child({
                            let on_new_subfolder = on_new_subfolder.clone();
                            sheet_action(
                                copy::text("New subfolder", "新建子文件夹", "新增子資料夾"),
                                icon(include_str!("../../assets/icons/folder.svg")),
                                false,
                                None,
                                move || on_new_subfolder(),
                            )
                        })
                        .child({
                            let on_delete = on_delete.clone();
                            sheet_action(
                                copy::text("Delete", "删除", "刪除"),
                                icon(include_str!("../../assets/icons/x.svg")),
                                true,
                                None,
                                move || {
                                    if let Some(id) = action.get_untracked() {
                                        on_delete(&id);
                                        action.set(None);
                                    }
                                },
                            )
                        }),
                ),
        )
}

fn folder_sheet(
    open: Signal<bool>,
    name: Signal<String>,
    on_close: Action,
    on_create: Action,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
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
                    text(copy::text("New folder", "新建文件夹", "新增資料夾"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(field::text_field(
                    name,
                    copy::text("Folder name", "文件夹名称", "資料夾名稱"),
                ))
                .child({
                    let on_create = on_create.clone();
                    action::primary(
                        copy::text("Create", "创建", "建立"),
                        icon(include_str!("../../assets/icons/check.svg")),
                        move || on_create(),
                    )
                    .w_full()
                    .disabled(move || name.with(|name| name.trim().is_empty()))
                    .disabled_style(|style| style.opacity(0.4))
                }),
        )
}

#[allow(clippy::too_many_arguments)]
fn sidebar_section(
    section: ReadSignal<SidebarSectionView>,
    sessions: Signal<Vec<SessionSummary>>,
    active: Signal<Option<String>>,
    activity_phase: Signal<u32>,
    project_only: Signal<bool>,
    on_select: Rc<dyn Fn(&str)>,
    on_menu: SessionMenu,
    on_folder_menu: FolderNameAction,
    on_folder_collapse: CollapseAction,
    on_group_collapse: CollapseAction,
    on_group_menu: FolderNameAction,
    open: Signal<bool>,
    select_mode: Signal<bool>,
    selected: Signal<Vec<String>>,
    pinned: Signal<Vec<String>>,
    unread: Signal<Vec<String>>,
    on_move_relative: MoveRelative,
) -> impl IntoElement {
    let rows = computed(move || {
        let ids = section.with(|section| section.session_ids.clone());
        sessions.with(|sessions| {
            ids.iter()
                .filter_map(|id| sessions.iter().find(|session| &session.id == id).cloned())
                .collect::<Vec<_>>()
        })
    });
    let indent =
        move || 24.0 + section.with(|section| section.depth.saturating_sub(1) as f32) * 18.0;
    view()
        .w_full()
        .flex_col()
        .gap(px(2.0))
        .child(
            view()
                .w_full()
                .h(px(42.0))
                .pl(px(
                    theme::SPACING_XS + section.with(|section| section.depth as f32) * 10.0
                ))
                .pr(px(theme::SPACING_XS))
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .child({
                    let on_folder_collapse = on_folder_collapse.clone();
                    let on_group_collapse = on_group_collapse.clone();
                    button()
                        .flex_1()
                        .min_w(px(0.0))
                        .h(px(42.0))
                        .px(px(8.0))
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .active(|style| style.opacity(0.8))
                        .on_press(move || {
                            let section = section.get_untracked();
                            let next = !section.collapsed;
                            if section.kind == SidebarSectionKind::Folder {
                                on_folder_collapse(&section.id, next);
                            } else {
                                on_group_collapse(&section.id, next);
                            }
                        })
                        .child(
                            svg(icon(include_str!("../../assets/icons/folder.svg")))
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex_shrink_0()
                                .text_color(theme::text_muted),
                        )
                        .child(when(
                            move || section.with(|section| section.pinned),
                            || {
                                text("★")
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted)
                            },
                        ))
                        .child(
                            view()
                                .flex_shrink(1.0)
                                .min_w(px(0.0))
                                .flex_row()
                                .items_center()
                                .gap(px(4.0))
                                .child(
                                    text(move || section.with(|section| section.name.clone()))
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(theme::font_body()))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::foreground),
                                )
                                .child(when(
                                    move || section.with(|section| section.collapsed),
                                    || {
                                        svg(icon(include_str!(
                                            "../../assets/icons/chevron-right.svg"
                                        )))
                                        .w(px(12.0))
                                        .h(px(12.0))
                                        .flex_shrink_0()
                                        .text_color(theme::text_muted)
                                    },
                                ))
                                .child(when(
                                    move || !section.with(|section| section.collapsed),
                                    || {
                                        svg(icon(include_str!(
                                            "../../assets/icons/chevron-down.svg"
                                        )))
                                        .w(px(12.0))
                                        .h(px(12.0))
                                        .flex_shrink_0()
                                        .text_color(theme::text_muted)
                                    },
                                )),
                        )
                })
                .child({
                    let on_folder_menu = on_folder_menu.clone();
                    let on_group_menu = on_group_menu.clone();
                    button()
                        .on_press(move || {
                            let section = section.get_untracked();
                            if section.kind == SidebarSectionKind::Folder {
                                on_folder_menu(&section.id, &section.name);
                            } else {
                                on_group_menu(&section.id, &section.name);
                            }
                        })
                        .accessibility_label(copy::text(
                            "Folder actions",
                            "文件夹操作",
                            "資料夾操作",
                        ))
                        .size(px(28.0))
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .active(|style| style.opacity(0.65))
                        .child(
                            svg(icon(include_str!(
                                "../../assets/icons/ellipsis-vertical.svg"
                            )))
                            .w(px(16.0))
                            .h(px(16.0))
                            .text_color(theme::text_muted),
                        )
                }),
        )
        .child(when(
            move || !section.with(|section| section.collapsed) && !project_only.get(),
            move || {
                let on_select = on_select.clone();
                let on_menu = on_menu.clone();
                let on_move_relative = on_move_relative.clone();
                view()
                    .w_full()
                    .relative()
                    .child(
                        view()
                            .absolute()
                            .left(move || px(indent()))
                            .top(px(0.0))
                            .bottom(px(0.0))
                            .w(px(1.0))
                            .bg(theme::border_subtle()),
                    )
                    .child(
                        view()
                            .w_full()
                            .pl(move || px(indent() + 10.0))
                            .flex_col()
                            .gap(px(2.0))
                            .child(each(
                                move || rows.get(),
                                |session| session.id.clone(),
                                move |session, _| {
                                    let session_signal = signal(session.get_untracked());
                                    let on_move_relative = on_move_relative.clone();
                                    session_row(
                                        session_signal,
                                        activity_phase,
                                        active,
                                        on_select.clone(),
                                        on_menu.clone(),
                                        open,
                                        select_mode,
                                        selected,
                                        pinned,
                                        unread,
                                        on_move_relative,
                                    )
                                },
                            )),
                    )
            },
        ))
}

#[allow(clippy::too_many_arguments)]
fn drawer_group(
    group: ReadSignal<DrawerGroup>,
    active: Signal<Option<String>>,
    activity_phase: Signal<u32>,
    project_only: Signal<bool>,
    section_rows: Rc<dyn Fn(&str) -> AnyElement>,
    collapsed: Signal<Vec<String>>,
    on_project_collapse: SessionToggle,
    on_new_session: Action,
    on_select: Rc<dyn Fn(&str)>,
    on_menu: SessionMenu,
    open: Signal<bool>,
    select_mode: Signal<bool>,
    selected: Signal<Vec<String>>,
    pinned: Signal<Vec<String>>,
    unread: Signal<Vec<String>>,
    on_move_relative: MoveRelative,
    collapsed_workspaces: Signal<Vec<String>>,
    on_workspace_collapse: SessionToggle,
) -> impl IntoElement {
    let closed = computed(move || {
        let id = group.with(|group| group.id.clone());
        collapsed.with(|collapsed| collapsed.contains(&id))
    });
    view()
        .w_full()
        .flex_col()
        .gap(px(2.0))
        .child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .child({
                    let on_project_collapse = on_project_collapse.clone();
                    button()
                        .on_press(move || {
                            let id = group.get_untracked().id;
                            let collapsed_now = collapsed.with(|collapsed| collapsed.contains(&id));
                            on_project_collapse(&id, !collapsed_now);
                        })
                        .flex_1()
                        .min_w(px(0.0))
                        .h(px(40.0))
                        .px(px(8.0))
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .active(|style| style.opacity(0.8))
                        .child(
                            svg(icon(include_str!("../../assets/icons/boxes.svg")))
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex_shrink_0()
                                .text_color(theme::color_token("chart-category-4")),
                        )
                        .child(
                            text(move || group.with(|group| group.name.clone()))
                                .flex_shrink(1.0)
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(theme::font_body()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::foreground),
                        )
                        .child(
                            view()
                                .flex_shrink_0()
                                .rounded(px(9.0))
                                .bg(theme::background_dim())
                                .px(px(6.0))
                                .py(px(1.0))
                                .child(
                                    text(move || group.with(|group| group.count.to_string()))
                                        .text_size(px(theme::font_detail()))
                                        .text_color(theme::text_muted),
                                ),
                        )
                })
                .child({
                    let on_new_session = on_new_session.clone();
                    button()
                        .size(px(28.0))
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .active(|style| style.opacity(0.65))
                        .accessibility_label(copy::text(
                            "New session here",
                            "在此新建会话",
                            "在此新增工作階段",
                        ))
                        .on_press(move || on_new_session())
                        .child(
                            svg(icon(include_str!("../../assets/icons/plus.svg")))
                                .w(px(16.0))
                                .h(px(16.0))
                                .text_color(theme::text_muted),
                        )
                }),
        )
        .child((section_rows)(&group.get_untracked().id))
        .child(when(move || !closed.get() && !project_only.get(), {
            move || {
                let on_select = on_select.clone();
                let on_menu = on_menu.clone();
                let on_workspace_collapse = on_workspace_collapse.clone();
                let on_move_relative = on_move_relative.clone();
                view().w_full().flex_col().gap(px(2.0)).child(each(
                    move || group.with(|group| group.rows.clone()),
                    |row| row.key(),
                    move |row, _| {
                        let on_select = on_select.clone();
                        let on_menu = on_menu.clone();
                        match row.get_untracked() {
                            DrawerRow::Header {
                                name, workspace, ..
                            } => {
                                let on_workspace_collapse = on_workspace_collapse.clone();
                                let collapsed_workspaces = collapsed_workspaces;
                                let collapsed = workspace.as_ref().is_some_and(|id| {
                                    collapsed_workspaces.with(|ids| ids.contains(id))
                                });
                                let toggle_workspace = workspace.clone();
                                button()
                                    .on_press(move || {
                                        if let Some(workspace) = toggle_workspace.clone() {
                                            on_workspace_collapse(&workspace, !collapsed);
                                        }
                                    })
                                    .w_full()
                                    .px(px(theme::SPACING_LG))
                                    .pt(px(4.0))
                                    .flex_row()
                                    .items_center()
                                    .gap(px(2.0))
                                    .child(
                                        text(if collapsed { "▸" } else { "▾" })
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_muted),
                                    )
                                    .child(
                                        text(name)
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_muted),
                                    )
                                    .into_element()
                            }
                            DrawerRow::Session(session) => {
                                let session = signal(session);
                                let on_move_relative = on_move_relative.clone();
                                view()
                                    .w_full()
                                    .relative()
                                    .pl(px(34.0))
                                    .child(
                                        view()
                                            .absolute()
                                            .left(px(24.0))
                                            .top(px(0.0))
                                            .bottom(px(0.0))
                                            .w(px(1.0))
                                            .bg(theme::border_subtle()),
                                    )
                                    .child(
                                        session_row(
                                            session,
                                            activity_phase,
                                            active,
                                            on_select,
                                            on_menu,
                                            open,
                                            select_mode,
                                            selected,
                                            pinned,
                                            unread,
                                            on_move_relative,
                                        )
                                        .into_element(),
                                    )
                                    .into_element()
                            }
                        }
                    },
                ))
            }
        }))
}

#[allow(clippy::too_many_arguments)]
fn session_row(
    session: Signal<SessionSummary>,
    activity_phase: Signal<u32>,
    active: Signal<Option<String>>,
    on_select: Rc<dyn Fn(&str)>,
    on_menu: SessionMenu,
    open: Signal<bool>,
    select_mode: Signal<bool>,
    selected: Signal<Vec<String>>,
    pinned: Signal<Vec<String>>,
    unread: Signal<Vec<String>>,
    on_move_relative: MoveRelative,
) -> impl IntoElement {
    let dragged = signal(false);
    let is_active = move || {
        let id = session.get().id;
        active.with(|active| active.as_deref() == Some(id.as_str()))
    };
    view()
        .w_full()
        .rounded(px(10.0))
        .bg(move || {
            if is_active() {
                theme::background_dim()
            } else {
                theme::background_card().opacity(0.0)
            }
        })
        .flex_row()
        .items_center()
        .on_long_press(move || dragged.set(true))
        .on_pan_end(move |event: &PanEvent| {
            if !dragged.get_untracked() {
                return;
            }
            dragged.set(false);
            let id = session.get_untracked().id;
            if event.translation.y.0 < -28.0 {
                on_move_relative(&id, -1);
            } else if event.translation.y.0 > 28.0 {
                on_move_relative(&id, 1);
            }
        })
        .child(
            button()
                .flex_1()
                .min_w(px(0.0))
                .h(px(48.0))
                .px(px(8.0))
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .active(|style| style.opacity(0.85))
                .on_press(move || {
                    let id = session.get_untracked().id;
                    if select_mode.get_untracked() {
                        selected.update(|selected| {
                            if let Some(index) =
                                selected.iter().position(|candidate| candidate == &id)
                            {
                                selected.remove(index);
                            } else {
                                selected.push(id);
                            }
                        });
                    } else {
                        on_select(&id);
                        open.set(false);
                    }
                })
                .child(when(
                    move || select_mode.get(),
                    move || {
                        let id = session.get_untracked().id;
                        let checked = selected.with(|selected| selected.contains(&id));
                        view()
                            .w(px(18.0))
                            .h(px(18.0))
                            .rounded(px(9.0))
                            .border_1()
                            .border_color(if checked {
                                theme::foreground()
                            } else {
                                theme::border_default()
                            })
                            .bg(if checked {
                                theme::foreground()
                            } else {
                                theme::background_dim()
                            })
                            .items_center()
                            .justify_center()
                            .child(
                                text(if checked { "✓" } else { "" })
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::background()),
                            )
                    },
                ))
                .child(
                    view()
                        .w(px(24.0))
                        .h(px(24.0))
                        .flex_shrink_0()
                        .relative()
                        .items_center()
                        .justify_center()
                        .child(
                            svg(icon(agent_icon(session.get_untracked().agent.as_str())))
                                .w(px(17.0))
                                .h(px(17.0))
                                .text_color(theme::text_secondary),
                        )
                        .child(when(
                            move || {
                                matches!(
                                    session.get().state,
                                    SessionState::Waiting | SessionState::Failed
                                )
                            },
                            move || {
                                view()
                                    .absolute()
                                    .right(px(1.0))
                                    .bottom(px(1.0))
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded(px(3.0))
                                    .bg(move || session.get().state.dot())
                            },
                        )),
                )
                .child(when(
                    move || {
                        let id = session.get().id;
                        pinned.with(|pinned| pinned.iter().any(|candidate| candidate == &id))
                    },
                    || {
                        text("★")
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted)
                    },
                ))
                .child(when(
                    move || {
                        let id = session.get().id;
                        unread.with(|unread| unread.iter().any(|candidate| candidate == &id))
                    },
                    || {
                        view()
                            .w(px(6.0))
                            .h(px(6.0))
                            .flex_shrink_0()
                            .rounded(px(3.0))
                            .bg(theme::accent_red())
                    },
                ))
                .child(
                    text(move || session.get().title.clone())
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(theme::font_body()))
                        .font_weight(if is_active() {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .text_color(move || {
                            if is_active() {
                                theme::foreground()
                            } else {
                                theme::text_secondary()
                            }
                        }),
                )
                .child(when(
                    move || session.get().state == SessionState::Running,
                    move || {
                        view()
                            .w(px(14.0))
                            .h(px(14.0))
                            .flex_shrink_0()
                            .relative()
                            .children((0..6).map(|index| {
                                let phase = activity_phase;
                                let column = (index % 2) as f32;
                                let row = (index / 2) as f32;
                                view()
                                    .absolute()
                                    .left(px(4.0 + column * 4.0))
                                    .top(px(2.0 + row * 4.0))
                                    .w(px(2.0))
                                    .h(px(2.0))
                                    .rounded(px(1.0))
                                    .bg(theme::color_token("chart-2"))
                                    .opacity(move || {
                                        let head = phase.get() as f32 / 8.0 * 6.0;
                                        let offset = (index as f32 - head).rem_euclid(6.0);
                                        let distance = offset.min(6.0 - offset);
                                        let intensity = 0.5
                                            + 0.5 * (distance / 6.0 * std::f32::consts::TAU).cos();
                                        0.28 + 0.72 * intensity
                                    })
                            }))
                    },
                ))
                .child(when(
                    move || session.get().state != SessionState::Running,
                    move || {
                        text(move || session.get().meta.clone())
                            .flex_shrink_0()
                            .text_size(px(theme::font_detail()))
                            .text_color(theme::text_muted)
                    },
                )),
        )
        .child(when(move || !select_mode.get(), {
            move || {
                let on_menu = on_menu.clone();
                button()
                    .size(px(36.0))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .active(|style| style.opacity(0.7))
                    .accessibility_label(copy::text("Session actions", "会话操作", "工作階段操作"))
                    .on_press(move || {
                        let session = session.get_untracked();
                        on_menu(&session.id, &session.title);
                    })
                    .child(
                        svg(icon(include_str!(
                            "../../assets/icons/ellipsis-vertical.svg"
                        )))
                        .w(px(16.0))
                        .h(px(16.0))
                        .text_color(theme::text_muted),
                    )
            }
        }))
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

/// What was typed after `$` when it is still one skill name, if any.
fn skill_query(text: &str) -> Option<String> {
    let at = text.rfind('$')?;
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

/// Replaces the `$…` token with the skill the person picked.
fn insert_skill(text: &str, insertion: &str) -> String {
    match text.rfind('$') {
        Some(at) => format!("{}{insertion}", &text[..at]),
        None => insertion.to_owned(),
    }
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

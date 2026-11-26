//! The session shell: the sessions page, the conversation and the workbench.
//!
//! Three pages side by side, drawn with `nsh`. The conversation is the one in
//! the middle: a header of three floating controls, the transcript as a
//! virtualized list of rows, and the prompt card pinned above the keyboard. A
//! session being written takes the transcript's place on that same page. The
//! sessions tree fills the page on its left and the workspace's files, changes
//! and terminal the page on its right. The app's session service owns the data
//! and the actions; this module binds them and decides how a row looks.
//!
//! The timeline renders desktop-projected turns and activity groups as a
//! virtual list with touch disclosures and native Markdown views. A turn's
//! working rows hang on one rail, so a run of steps reads as one run.
//!
//! A row's commands are behind a long press, in a menu raised beside the
//! finger ([`crate::ui::menu`]); what a command still has to ask is asked in
//! a sheet of cards ([`crate::ui::sheet`]).

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use nsh_base::{ButtonElement, button};
use nsh_core::{
    element::{ImageSrc, Painter, Position, canvas},
    prelude::*,
};

use crate::{
    copy,
    screens::connecting::{self as connecting_screen, ConnectionState},
    theme,
    ui::{
        action, compose, field,
        menu::{self, Menu, MenuEntry, MenuSide},
        surface,
    },
};

mod find;
mod pager;
mod tree;
mod workbench;

use pager::{Pager, ShellPage};
pub use tree::{SidebarCardSlice, SidebarRowKind, SidebarRowView, SidebarTree};
use workbench::WorkbenchTab;

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
    pub terminal: bool,
    /// The project the session belongs to, for the drawer's grouping.
    pub project_id: String,
    pub project_name: String,
    /// The checkout or worktree it runs in, for the row's second line.
    pub workspace_id: String,
    pub workspace_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConversationState {
    Loading,
    #[default]
    Ready,
    Failed(SharedString),
}

/// What kind of timeline row this is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    #[default]
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
    ActivityGroup,
    Generating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityGlyph {
    Command,
    Search,
    Directory,
    Read,
    Edit,
    Create,
    Delete,
    Plan,
    Agent,
    Image,
    Integration,
    Web,
    Retry,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityDetail {
    pub label: String,
    pub text: String,
    pub copy: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityView {
    pub glyph: ActivityGlyph,
    pub target: String,
    pub path: Option<String>,
    pub failed: bool,
    pub error: Option<String>,
    pub duration: Option<String>,
    /// Lines an edit added, when the row carries a patch to count them from.
    pub added_lines: Option<u32>,
    /// Lines an edit removed, when the row carries a patch to count them from.
    pub removed_lines: Option<u32>,
    pub details_pending: bool,
    pub details_loading: bool,
    pub details: Vec<ActivityDetail>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnFileChange {
    pub path: String,
    pub row_id: String,
    pub deleted: bool,
}

/// One row of the transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    pub parent_id: Option<String>,
    pub activity: Option<ActivityView>,
    pub changed_files: Vec<TurnFileChange>,
    /// The Agent that ran the turn, for the working header's mark. Empty when
    /// the row does not know, which draws the default one.
    pub agent: String,
    /// Whether this is the turn's answer, the one row that concludes it.
    ///
    /// A turn's process rows are the Agent's working notes; only the answer is
    /// what the reader came for, so only it carries the row's actions.
    pub conclusion: bool,
    /// The emoji the Agent set on this message, when it set one.
    ///
    /// It rides on the bubble rather than in the prose: the Agent answers a
    /// message with a reaction through its own tool, and the reader sees it as
    /// the mark they would have left themselves.
    pub reaction: Option<SharedString>,
    /// How the message reached the turn, when it was not an ordinary prompt.
    ///
    /// A steered or resent message wears a glyph on the bubble's lower edge,
    /// as the desktop draws it: it says what the message did without a line of
    /// status text above the bubble.
    pub delivery: Option<MessageDelivery>,
}

/// How a message reached the turn it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageDelivery {
    /// Injected into a turn that was already running.
    Steer,
    /// Interrupted the running turn and claimed a new one.
    Resend,
}

/// What the session is running on, for the composer's runtime trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSummaryView {
    pub agent_id: String,
    pub agent: String,
    pub model_id: String,
    pub model: String,
    pub reasoning: String,
    pub mode: String,
    /// Whether the session has been moved off the Agent's own defaults.
    pub options_set: bool,
    pub reasoning_set: bool,
    pub mode_set: bool,
    pub available: bool,
}

/// The state and callbacks shared by the Agent settings sheet in both an open
/// session and a session being created.
#[derive(Clone)]
pub struct RuntimePickerHost {
    pub summary: Signal<Option<RuntimeSummaryView>>,
    pub options: Signal<Vec<RuntimeOptionView>>,
    pub reasoning: Signal<Vec<RuntimeChoiceView>>,
    pub modes: Signal<Vec<RuntimeChoiceView>>,
    pub features: Signal<Vec<RuntimeFeatureView>>,
    pub open: Signal<bool>,
    pub busy: Signal<bool>,
    pub on_open: Action,
    pub on_close: Action,
    pub on_choose_runtime: Rc<dyn Fn(&str)>,
    pub on_choose_reasoning: Rc<dyn Fn(&str)>,
    pub on_choose_mode: Rc<dyn Fn(&str)>,
    pub on_choose_feature: ChooseFeature,
}

/// One Agent + auth source + model the runtime can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeOptionView {
    pub id: String,
    pub agent_id: String,
    pub agent: String,
    pub model: String,
    /// Optional secondary note shown under the model name; empty when the
    /// fold header already names the provider.
    pub detail: String,
    pub selected: bool,
    pub available: bool,
    /// The provider profile, account or route the option draws from, as a
    /// stable key to group by.
    pub source_key: String,
    /// The display name of the provider profile, account or route shown on the
    /// fold header.
    pub source_label: String,
    /// The model's own id, for its brand mark.
    pub model_id: String,
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
    pub explicit_value: Option<String>,
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
    /// The worktree a group is anchored to, for the session-row menus that
    /// offer to join a group in the same scope.
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub depth: usize,
    pub collapsed: bool,
    pub name: String,
    pub session_ids: Vec<String>,
    /// The idle days after which a folder archives its own sessions.
    pub auto_archive_after_days: Option<u8>,
    /// Whether automatic continuation is on for every member of a group.
    pub auto_continue: bool,
}

/// One message a session is holding for a later hour.
///
/// The row menu addresses the holder's queue entry, so the id travels with
/// the session it belongs to; a sheet that only knew the session could not
/// cancel or move the deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledSendView {
    pub session_id: String,
    pub message_id: u64,
    pub scheduled_at_ms: i64,
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
    pub sequence: i64,
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
    NewSessionHost, ProjectBrowserHost, WorkspaceChoice, new_session as new_session_page,
    project_browser_sheet as project_browser,
};

/// A callback with no arguments.
type Action = Rc<dyn Fn()>;
/// Chooses a value for one of the Agent's session options.
type ChooseFeature = Rc<dyn Fn(&str, Option<&str>)>;
/// Opens one user message for replacement.
type EditMessage = Rc<dyn Fn(i64, &str)>;
type OpenAttachment = Rc<dyn Fn(&AttachmentItem)>;
type BatchAction = Rc<dyn Fn(&[String])>;
type FolderNameAction = Rc<dyn Fn(&str, &str)>;
type CancelScheduledAction = Rc<dyn Fn(&str, u64)>;
type RescheduleScheduledAction = Rc<dyn Fn(&str, u64, u64)>;
type FolderArchiveAction = Rc<dyn Fn(&str, Option<u8>)>;
type CreateGroupAction = Rc<dyn Fn(&str, &[String])>;
type CollapseAction = SessionToggle;
type SessionToggle = Rc<dyn Fn(&str, bool)>;
type MoveSessionAction = Rc<dyn Fn(&str, Option<&str>)>;
pub type BrowseDirs = Rc<dyn Fn(Option<&str>)>;
/// Opens one row's full detail.
type ToggleDetail = Rc<dyn Fn(&str, i64)>;
type RevealRow = Rc<dyn Fn(&str, bool)>;

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
    pub auto_continue_sessions: Signal<Vec<String>>,
    pub sidebar: SidebarTree,
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
    pub on_set_group_auto_continue: SessionToggle,
    pub on_create_group: CreateGroupAction,
    pub move_open: Signal<Option<String>>,
    pub on_open_move: Action,
    pub on_close_move: Action,
    pub on_move_session: MoveSessionAction,
    pub on_toggle_pin: Rc<dyn Fn(&str)>,

    pub timeline: Signal<Vec<TimelineEntry>>,
    pub conversation: Signal<ConversationState>,
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
    pub on_new_project: Action,
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
    pub on_reveal_row: RevealRow,
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
    pub terminal_visible: Signal<bool>,
    pub terminal_title: Signal<Option<SharedString>>,
    pub terminal_error: Signal<Option<SharedString>>,
    pub terminal_ref: ComponentRef<crate::screens::terminal::Terminal>,
    pub on_open_terminal: Action,
    pub on_terminal_data: TerminalInput,
    pub on_terminal_resize: Rc<dyn Fn(u32, u32)>,
    pub on_terminal_title: Rc<dyn Fn(&str)>,
    pub terminal_choices: Signal<Vec<crate::screens::terminal::TerminalChoice>>,
    pub terminal_id: Signal<Option<String>>,
    pub terminal_busy: Signal<bool>,
    pub terminal_output_revision: Signal<u64>,
    pub on_retry_terminal: Action,
    pub on_kill_terminal: Rc<dyn Fn(&str)>,
    pub on_select_terminal: Rc<dyn Fn(&str)>,
    pub on_new_terminal: Action,
    /// Stops the active session's turn.
    pub on_interrupt: Action,
    /// Opens the runtime switcher over the live session.
    pub on_switch_runtime: Action,
    /// The files surface, when it is open over the shell.
    pub files_open: Signal<bool>,
    pub files_entries: Signal<Vec<crate::screens::files::FileEntry>>,
    pub files_search: Signal<Vec<crate::screens::files::FileSearchRow>>,
    pub files_searching: Signal<bool>,
    pub files_path: Signal<Option<String>>,
    pub on_search_files: crate::screens::files::FileSearch,
    pub files_preview: Signal<Option<crate::screens::files::FilePreview>>,
    pub files_error: Signal<Option<SharedString>>,
    pub on_open_files: Action,
    pub on_refresh_files: Action,
    pub on_collapse_files: Action,
    pub on_open_dir: OpenPath,
    pub on_open_file: crate::screens::files::OpenFile,
    pub on_files_back: Action,
    /// Whether a new session is being written on the conversation page, and
    /// what it can be started with.
    pub new_session_open: Signal<bool>,
    pub new_workspaces: Signal<Vec<WorkspaceChoice>>,
    pub new_workspace: Signal<Option<String>>,
    pub new_runtime: RuntimePickerHost,
    pub new_busy: Signal<bool>,
    pub new_error: Signal<Option<SharedString>>,
    pub on_create_session: Action,
    pub new_worktree_branch: Signal<String>,
    pub new_base_ref: Signal<String>,
    pub new_title: Signal<String>,
    pub new_prompt: Signal<String>,
    pub new_attachments: Signal<Vec<AttachmentItem>>,
    pub on_create_temporary_session: Action,
    pub project_browser: ProjectBrowserHost,
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
    /// Sessions whose automatic continuation the reader suspended. The row
    /// menu lifts a suspension; only the badge used to read it.
    pub auto_continue_paused: Signal<Vec<String>>,
    /// The messages the holder is waiting to send later, by session.
    pub scheduled_sends: Signal<Vec<ScheduledSendView>>,
    pub on_resume_auto_continue: Rc<dyn Fn(&str)>,
    pub on_cancel_scheduled_send: CancelScheduledAction,
    /// Moves a held send to `seconds` from now, the other half of the same
    /// deadline the schedule menu asks about.
    pub on_reschedule_scheduled_send: RescheduleScheduledAction,
    pub on_set_folder_auto_archive: FolderArchiveAction,
    pub on_toggle_project_new_session_location: Rc<dyn Fn(&str)>,
    pub on_remove_project: Rc<dyn Fn(&str)>,
    pub on_session_menu: SessionMenu,
    pub on_pin_sessions: BatchAction,
    pub on_fork_sessions: BatchAction,
    pub on_archive_sessions: BatchAction,
    pub on_delete_sessions: BatchAction,
    pub on_close_session_actions: Action,
    pub on_rename_session: Action,
    pub on_archive_session: Action,
    pub on_delete_session: Action,
    /// The Git surface, when it is open over the shell.
    pub git_open: Signal<bool>,
    pub git_branch: Signal<Option<SharedString>>,
    pub git_totals: Signal<(u32, u32)>,
    pub git_changes: Signal<Vec<crate::screens::git::GitChangeRow>>,
    pub git_diff: Signal<Option<crate::screens::git::GitDiffPreview>>,
    pub git_error: Signal<Option<SharedString>>,
    pub on_open_git: Action,
    pub on_open_git_diff: OpenDiff,
    pub on_stage_git: OpenPath,
    pub on_unstage_git: OpenPath,
    pub git_selected: Signal<Vec<String>>,
    pub on_toggle_git_selection: OpenPath,
    pub on_toggle_git_directory: BatchAction,
    pub on_toggle_git_directories: Action,
    pub on_revert_git_selection: BatchAction,
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
    pub on_git_fetch: Action,
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
    let established = signal(false);
    effect(move || {
        if connection.get() == ConnectionState::Online {
            established.set(true);
        }
    });
    view().flex_1().bg(theme::block_page_adaptive()).child(
        when(
            move || {
                !established.get()
                    && connection.get() != ConnectionState::Online
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

fn surfaces(host: SessionHost) -> impl IntoElement {
    let pager = Pager::new();
    let overlays = Overlays {
        menu: signal(None),
        rename: signal(false),
        reschedule: signal(None),
    };
    let initial_tab = if host.git_open.get_untracked() {
        Some(WorkbenchTab::Changes)
    } else if host.terminal_open.get_untracked() {
        Some(WorkbenchTab::Terminal)
    } else if host.files_open.get_untracked() {
        Some(WorkbenchTab::Files)
    } else {
        None
    };
    let tab = signal(initial_tab.unwrap_or_default());
    let terminal_find = signal(false);
    if initial_tab.is_some() {
        pager.show_initial(ShellPage::Workbench);
    }
    let rail_host = host.clone();
    let creating = host.new_session_open;
    // A raised menu is the first thing the back key puts away.
    pager.on_pre_back(move || menu::dismiss(overlays.menu));
    pager.on_back({
        let host = host.clone();
        move || match pager.page_untracked() {
            ShellPage::Workbench
                if tab.get_untracked() == WorkbenchTab::Terminal
                    && terminal_find.get_untracked() =>
            {
                terminal_find.set(false);
                dismiss_keyboard();
                true
            }
            ShellPage::Workbench => tab.get_untracked().back(&host),
            // Backing out of a session being written puts its Agent settings
            // sheet away first, then returns to the open session.
            ShellPage::Home if creating.get_untracked() => {
                dismiss_keyboard();
                if host.new_runtime.open.get_untracked() {
                    (host.new_runtime.on_close)();
                } else {
                    (host.on_close_new_session)();
                }
                true
            }
            ShellPage::Home if host.find_open.get_untracked() => {
                (host.on_toggle_find)();
                true
            }
            _ => false,
        }
    });
    // A session is written on the conversation page, whichever page asked
    // for it.
    effect(move || {
        if creating.get() && pager.page_untracked() != ShellPage::Home {
            pager.close();
        }
    });
    let active = host.active;
    let session = active_session(&host);
    let previous = stored(None::<(String, bool)>);
    effect(move || {
        let id = active.get();
        let terminal =
            session.with(|session| session.as_ref().is_some_and(|session| session.terminal));
        let current = id.map(|id| (id, terminal));
        let prior = previous.replace(current.clone());
        if prior != current {
            if terminal {
                tab.set(WorkbenchTab::Terminal);
                pager.open(ShellPage::Workbench);
            } else if prior.is_some() && pager.page_untracked() != ShellPage::Home {
                pager.close();
            }
        }
    });
    let drawer_host = host.clone();
    view()
        .flex_1()
        .min_h(px(0.0))
        .relative()
        .child(pager::pager(
            pager,
            shell(host.clone(), pager, tab, overlays),
            drawer(drawer_host, pager, overlays),
            move || {
                workbench::workbench(pager, tab, terminal_find, rail_host.clone()).into_element()
            },
        ))
        // The sheets a session's commands lead to belong to no one page: a
        // row on the sessions page and the conversation's own header raise
        // the same ones.
        .child(rename_session_sheet(&host, overlays.rename))
        .child(scheduled_send_sheet(
            overlays.reschedule,
            host.on_reschedule_scheduled_send.clone(),
        ))
        .child(move_session_sheet(
            host.move_open,
            host.sidebar_sections,
            host.on_move_session.clone(),
            host.on_close_move.clone(),
        ))
        .child(attachment_preview_sheet(
            host.attachment_preview,
            host.on_close_attachment.clone(),
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
        .child(project_browser(host.project_browser.clone()))
        .child(menu::menu_layer(overlays.menu))
}

fn workbench_surfaces(
    host: SessionHost,
    tab: Signal<WorkbenchTab>,
    terminal_find: Signal<bool>,
) -> impl IntoElement {
    let terminal = crate::screens::terminal::TerminalHost {
        session_terminal: active_session(&host),
        title: host.terminal_title,
        error: host.terminal_error,
        term_ref: host.terminal_ref,
        visible: host.terminal_visible,
        on_data: host.on_terminal_data.clone(),
        on_resize: host.on_terminal_resize.clone(),
        on_title: host.on_terminal_title.clone(),
        terminals: host.terminal_choices,
        active: host.terminal_id,
        busy: host.terminal_busy,
        output_revision: host.terminal_output_revision,
        find_open: terminal_find,
        on_retry: host.on_retry_terminal.clone(),
        on_kill: host.on_kill_terminal.clone(),
        on_select_terminal: host.on_select_terminal.clone(),
        on_new_terminal: host.on_new_terminal.clone(),
    };
    let files = crate::screens::files::FilesHost {
        entries: host.files_entries,
        search: host.files_search,
        searching: host.files_searching,
        path: host.files_path,
        on_search: host.on_search_files.clone(),
        on_refresh: host.on_refresh_files.clone(),
        on_collapse: host.on_collapse_files.clone(),
        file: host.files_preview,
        error: host.files_error,
        on_open_dir: host.on_open_dir.clone(),
        on_open_file: host.on_open_file.clone(),
        on_back: host.on_files_back.clone(),
    };
    let git = crate::screens::git::GitHost {
        active_session: host.active,
        branch: host.git_branch,
        totals: host.git_totals,
        changes: host.git_changes,
        diff: host.git_diff,
        error: host.git_error,
        on_open_diff: host.on_open_git_diff.clone(),
        on_stage: host.on_stage_git.clone(),
        on_unstage: host.on_unstage_git.clone(),
        selected: host.git_selected,
        on_toggle_selection: host.on_toggle_git_selection.clone(),
        on_toggle_directory: host.on_toggle_git_directory.clone(),
        on_toggle_directories: host.on_toggle_git_directories.clone(),
        on_revert_selection: host.on_revert_git_selection.clone(),
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
        on_fetch: host.on_git_fetch.clone(),
        history: host.git_history,
        show_history: host.git_show_history,
        commit_detail: host.git_commit_detail,
        on_open_commit: host.on_open_git_commit.clone(),
        on_revert: host.on_revert_git.clone(),
        on_commit: host.on_commit_git.clone(),
        on_toggle_history: host.on_toggle_git_history.clone(),
        on_back: host.on_git_back.clone(),
        on_refresh: host.on_refresh_git.clone(),
    };
    let git_open = host.git_open;
    let files_open = host.files_open;
    let terminal_open = host.terminal_open;
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(workbench::tab_panel(
            tab,
            WorkbenchTab::Files,
            move || files_open.get(),
            move || crate::screens::files::files(files.clone()).into_element(),
        ))
        .child(workbench::tab_panel(
            tab,
            WorkbenchTab::Changes,
            move || git_open.get(),
            move || crate::screens::git::git(git.clone()).into_element(),
        ))
        .child(workbench::tab_panel(
            tab,
            WorkbenchTab::Terminal,
            move || terminal_open.get(),
            move || crate::screens::terminal::terminal(terminal.clone()).into_element(),
        ))
}

pub fn handle_back() -> bool {
    pager::current().is_some_and(Pager::back)
}

/// The conversation page: header, transcript, cards and composer. A session
/// being written takes the transcript's place under the same header.
fn shell(
    host: SessionHost,
    pager: Pager,
    tab: Signal<WorkbenchTab>,
    overlays: Overlays,
) -> impl IntoElement {
    let connection = host.connection;
    let connection_error = host.connection_error;
    let on_retry = host.on_retry.clone();
    let terminal = active_session(&host);
    let composer_host = host.clone();
    let composer_input = element_ref();
    let jump_to_message = signal(None::<i64>);
    let runtime_panel = signal(RuntimePanel::Model);
    let creating = host.new_session_open;
    // Whether the group tabs take the bar's room above the transcript: the
    // connection strip below them uses the same answer, so the two never both
    // inset for the bar.
    let tabs_above_for_banner = group_tabs_visible(&host);
    let create_host = NewSessionHost {
        workspaces: host.new_workspaces,
        workspace: host.new_workspace,
        runtime: host.new_runtime.clone(),
        busy: host.new_busy,
        error: host.new_error,
        on_create: host.on_create_session.clone(),
        on_create_temporary: host.on_create_temporary_session.clone(),
        title: host.new_title,
        prompt: host.new_prompt,
        attachments: host.new_attachments,
        on_attach_image: host.on_attach_new_image.clone(),
        on_attach_file: host.on_attach_new_file.clone(),
        on_remove_attachment: host.on_remove_new_attachment.clone(),
        worktree_branch: host.new_worktree_branch,
        base_ref: host.new_base_ref,
        on_create_worktree: host.on_create_worktree_session.clone(),
    };

    view()
        .flex_1()
        .relative()
        // The page tone the cards sit on: where a palette makes the card and
        // the page the same white, this is the step between them, and the
        // floating bar's glass has something to be glass over.
        .bg(theme::block_page_adaptive())
        .flex_col()
        .test_id("conversation-page")
        .child(header(host.clone(), pager, overlays))
        .child(when(
            move || creating.get(),
            move || new_session_page(create_host.clone()),
        ))
        .child(
            // The conversation stays mounted under a session being written,
            // so backing out finds it where it was left.
            view()
                .flex_1()
                .min_h(px(0.0))
                .flex_col()
                .relative()
                // The glass the transcript passes under. It lives in here
                // rather than on the page so that it and the group tabs are
                // siblings: the tabs clear it by `z_index`, which only orders
                // siblings, and the band must not blur the controls on it.
                .child(top_glass())
                // The bar floats over the page and the transcript runs beneath
                // it: the list starts at the page's top edge, and the space the
                // bar takes is the list's own content padding (see
                // `timeline_view`), so a message scrolled up keeps showing
                // through the glass instead of stopping at a hard edge.
                .style_when(move || creating.get(), |style| style.hidden())
                .child(group_tabs(&host))
                // The connection strip belongs under the bar, not on the page's
                // top edge: the status bar and the system's own chrome live
                // there, and a strip in that band drew over them. In flow here,
                // it is pushed below the bar by the same inset the tabs use,
                // and it sits above the glass so the blur never touches it.
                .child(when(move || connection.get() != ConnectionState::Online, {
                    let tabs_above = tabs_above_for_banner;
                    move || {
                        view()
                            .z_index(2)
                            .pt(move || {
                                if tabs_above.get() {
                                    px(0.0)
                                } else {
                                    px(top_bar_inset())
                                }
                            })
                            .child(connection_banner(
                                connection,
                                connection_error,
                                on_retry.clone(),
                            ))
                    }
                }))
                .child(
                    view()
                        .relative()
                        .flex_1()
                        .min_h(px(0.0))
                        .child(timeline_view(host.clone(), jump_to_message, pager, tab))
                        // A failure floats here and leaves on its own. Drawn
                        // into the page it had no way out: it outlived the
                        // condition it described, and the reader had to leave
                        // the session to be rid of it.
                        .child({
                            let error = host.error;
                            crate::ui::toast::toast(
                                error,
                                top_bar_inset() + theme::SPACING_SM,
                                move || error.set(None),
                            )
                        }),
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
                        move || {
                            command_palette(mentions, draft, PaletteKind::Mention, on_arm.clone())
                        }
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
                .child(when(
                    move || {
                        terminal.with(|session| {
                            session.as_ref().is_some_and(|session| !session.terminal)
                        })
                    },
                    move || composer(composer_host.clone(), composer_input, runtime_panel),
                )),
        )
        .child(runtime_sheet(
            RuntimePickerHost {
                summary: host.runtime_summary,
                options: host.runtime_options,
                reasoning: host.runtime_reasoning,
                modes: host.runtime_modes,
                features: host.runtime_features,
                open: host.runtime_open,
                busy: host.runtime_busy,
                on_open: host.on_open_runtime.clone(),
                on_close: host.on_close_runtime.clone(),
                on_choose_runtime: host.on_choose_runtime.clone(),
                on_choose_reasoning: host.on_choose_reasoning.clone(),
                on_choose_mode: host.on_choose_mode.clone(),
                on_choose_feature: host.on_choose_feature.clone(),
            },
            runtime_panel,
        ))
        .child(history_sheet(host.clone(), jump_to_message, composer_input))
}

/// The conversation's top bar: three objects on the page rather than a strip
/// across it. The way to the sessions, the session's own title and menu, and
/// the two ways out of it: a new session, and the workbench.
fn header(host: SessionHost, pager: Pager, overlays: Overlays) -> impl IntoElement {
    let creating = host.new_session_open;
    let terminal_choices = host.terminal_choices;
    let terminal_running = move || {
        terminal_choices.with(|choices| choices.iter().any(|choice| choice.status == "running"))
    };
    let activity = crate::ui::activity::ActivityClock::start();
    let session = active_session(&host);
    // The row's own rule: the pulse is the primary accent, and green only when
    // the session keeps going by itself. The header reads the same list the row
    // does, so the two marks never disagree about the same session.
    let auto_continue = host.auto_continue_sessions;
    let on_stop_creating = host.on_close_new_session.clone();
    let has_menu = move || !creating.get() && session.with(Option::is_some);
    let title = computed(move || {
        if creating.get() {
            copy::text("New session", "新建会话", "新增工作階段").to_owned()
        } else {
            session.with(|session| {
                session
                    .as_ref()
                    .map_or_else(|| "Vibex".to_owned(), |session| session.title.clone())
            })
        }
    });
    let open_menu = {
        let host = host.clone();
        move || {
            let Some(current) = session.get_untracked() else {
                return;
            };
            // The tick a held drawer row gives the hand: the header raises the
            // same card, so opening it feels the same.
            crate::motion::tick();
            dismiss_keyboard();
            let mut lead = Vec::new();
            if !host.find_open.get_untracked() {
                let find = host.on_toggle_find.clone();
                lead.push(MenuEntry::new(
                    icon!("search"),
                    copy::text("Find in conversation", "在会话中查找", "在工作階段中尋找"),
                    move || find(),
                ));
            }
            let recallable = !host.sending.get_untracked()
                && host.timeline.with_untracked(|entries| {
                    entries.iter().any(|entry| entry.kind == EntryKind::User)
                });
            if recallable {
                let history = host.on_open_history.clone();
                lead.push(MenuEntry::new(
                    icon!("history"),
                    copy::text("Earlier messages", "历史消息", "歷史訊息"),
                    move || history(),
                ));
            }
            let team = host.on_open_team.clone();
            lead.push(MenuEntry::new(
                icon!("bot"),
                copy::text("Team", "团队", "團隊"),
                move || team(),
            ));
            // The header stands the menu under the bar and lifts the bar out
            // of the page, which is what a long press does to a drawer row: the
            // same card, the same shadow, so the menu belongs to something
            // rather than floating over a dimmed page.
            let header_top = safe_area().top.0;
            let raised = menu::MenuRow {
                key: current.id.clone(),
                top: header_top,
                height: theme::HEADER_HEIGHT,
                mark: Some(icon(agent_icon(&current.agent))),
                title: current.title.clone().into(),
                title_size: theme::font_detail(),
                trailing: String::new().into(),
            };
            overlays.menu.set(Some(
                session_menu(
                    &host,
                    overlays,
                    &current.id,
                    &current.title,
                    (header_top + theme::HEADER_HEIGHT / 2.0, MenuSide::Center),
                    lead,
                )
                .from_row(raised),
            ));
        }
    };
    // The bar floats over the conversation rather than sitting on top of it:
    // its controls are filled panes with a shadow, so a message scrolls
    // underneath and is read in the gaps between them instead of being cut off
    // at a hard edge. They take `header_pane` — the prompt card's own fill —
    // because the bar and the field are the same instrument at the two ends of
    // the page, and the card tone left the bar a dimmer block in the dark
    // appearance. `pt_safe_area` keeps the panes below the status bar.
    view()
        .w_full()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .z_index(2)
        .pt_safe_area()
        .child(
            view()
                .w_full()
                .h(px(theme::HEADER_HEIGHT))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_MD))
                .child(
                    surface::round_button(
                        icon!("menu"),
                        copy::text("Sessions", "会话", "工作階段"),
                        move || pager.open(ShellPage::Sessions),
                    )
                    .bg(theme::header_pane)
                    .shadow(theme::floating_pane_shadow()),
                )
                .child(
                    view().flex_1().min_w(px(0.0)).items_center().child(
                        button()
                            .max_w(relative(1.0))
                            .h(px(theme::COMPACT_CONTROL + 2.0))
                            .pl(px(theme::SPACING_MD))
                            .pr(px(theme::SPACING_MD))
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .rounded_full()
                            .bg(theme::header_pane)
                            .shadow(theme::floating_pane_shadow())
                            .active(|style| style.opacity(0.6))
                            .accessibility_label(move || {
                                SharedString::from(if has_menu() {
                                    format!(
                                        "{}: {}",
                                        copy::text("Conversation menu", "会话菜单", "工作階段選單"),
                                        title.get()
                                    )
                                } else {
                                    title.get()
                                })
                            })
                            .disabled(move || !has_menu())
                            .on_press(open_menu)
                            .child(dynamic(move || {
                                let state = if creating.get() {
                                    None
                                } else {
                                    session.with(|session| {
                                        session.as_ref().map(|session| session.state)
                                    })
                                };
                                match state {
                                    Some(SessionState::Running) => {
                                        let continues = session.with(|session| {
                                            session.as_ref().is_some_and(|session| {
                                                auto_continue.with(|continuing| {
                                                    continuing.iter().any(|id| id == &session.id)
                                                })
                                            })
                                        });
                                        crate::ui::activity::activity_ring(
                                            activity,
                                            if continues {
                                                theme::accent_green
                                            } else {
                                                theme::primary
                                            },
                                        )
                                        .into_element()
                                    }
                                    // The desktop answers this state with its
                                    // question-mark icon everywhere it reports it,
                                    // so the header draws the same mark as the row
                                    // instead of a dot that reads as one more
                                    // status.
                                    Some(SessionState::Waiting) => {
                                        svg(icon!("circle-question-mark"))
                                            .size(px(theme::SIDEBAR_META_ICON))
                                            .text_color(theme::accent_yellow)
                                            .into_element()
                                    }
                                    Some(SessionState::Failed) => {
                                        surface::status_dot(theme::accent_red).into_element()
                                    }
                                    _ => view().into_element(),
                                }
                            }))
                            .child(
                                text(move || title.get())
                                    .truncate()
                                    .min_w(px(0.0))
                                    .flex_shrink(1.0)
                                    .text_size(px(theme::font_detail()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::foreground),
                            )
                            .child(
                                svg(icon!("chevron-down"))
                                    .size(px(12.0))
                                    .flex_shrink_0()
                                    .text_color(theme::text_muted)
                                    .style_when(move || !has_menu(), |style| style.hidden()),
                            ),
                    ),
                )
                .child(
                    when(
                        move || creating.get(),
                        move || {
                            let stop = on_stop_creating.clone();
                            surface::round_button(
                                icon!("x"),
                                copy::text("Cancel", "取消", "取消"),
                                move || {
                                    dismiss_keyboard();
                                    stop();
                                },
                            )
                        },
                    )
                    .otherwise(move || {
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            // The drawer owns "new session": its own control is the
                            // one place to start one, so the header does not offer a
                            // second way to the same page.
                            .child(
                                surface::round_button(
                                    icon!("panel-right"),
                                    copy::text(
                                        "Files, Git, and terminal",
                                        "文件、Git 与终端",
                                        "檔案、Git 與終端機",
                                    ),
                                    move || pager.open(ShellPage::Workbench),
                                )
                                .bg(theme::header_pane)
                                .shadow(theme::floating_pane_shadow())
                                .relative()
                                .child(when(
                                    terminal_running,
                                    || {
                                        surface::status_dot(theme::accent_success)
                                            .absolute()
                                            .top(px(3.0))
                                            .right(px(3.0))
                                    },
                                )),
                            )
                    }),
                ),
        )
}

fn active_session(host: &SessionHost) -> Memo<Option<SessionSummary>> {
    let sessions = host.sessions;
    let active = host.active;
    computed(move || {
        let active = active.get()?;
        sessions.with(|sessions| {
            sessions
                .iter()
                .find(|session| session.id == active)
                .cloned()
        })
    })
}

/// The sessions of the open session's group, when it is in one with others:
/// the tabs above the transcript.
fn group_members(host: &SessionHost) -> Memo<Vec<String>> {
    let active = host.active;
    let sections = host.sidebar_sections;
    computed(move || {
        let Some(id) = active.get() else {
            return Vec::new();
        };
        sections.with(|sections| {
            sections
                .iter()
                .find(|section| {
                    section.kind == SidebarSectionKind::Group && section.session_ids.contains(&id)
                })
                .map(|section| section.session_ids.clone())
                .unwrap_or_default()
        })
    })
}

/// Whether the group tabs stand above the transcript: they do when the open
/// session shares its group with another, the only time the strip is drawn.
fn group_tabs_visible(host: &SessionHost) -> Memo<bool> {
    let members = group_members(host);
    computed(move || members.with(|members| members.len() > 1))
}

fn group_tabs(host: &SessionHost) -> impl IntoElement {
    let active = host.active;
    let sessions = host.sessions;
    let on_select = host.on_select.clone();
    let members = group_members(host);
    when(
        move || members.with(|members| members.len() > 1),
        move || {
            let on_select = on_select.clone();
            scroll()
                .horizontal()
                .scrollbar(false)
                .flex_shrink_0()
                .w_full()
                // The bar floats over the page, so the tabs clear it here: the
                // transcript below runs under the bar, the tabs do not.
                .pt(move || px(top_bar_inset()))
                // Above the glass: the band blurs what passes under it, and
                // the tabs are controls that sit on it.
                .z_index(2)
                .child(
                    view()
                        .h(px(theme::ROUND_CONTROL))
                        .px(px(theme::SPACING_MD))
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(each(
                            move || members.get(),
                            String::clone,
                            move |member, _| {
                                let on_select = on_select.clone();
                                let id = member.get_untracked();
                                let selected = move || {
                                    active.with(|active| {
                                        active.as_ref() == Some(&member.get_untracked())
                                    })
                                };
                                let session = computed(move || {
                                    sessions.with(|sessions| {
                                        sessions
                                            .iter()
                                            .find(|session| session.id == member.get_untracked())
                                            .cloned()
                                    })
                                });
                                button()
                                    .h(px(theme::CHIP_HEIGHT))
                                    .max_w(px(170.0))
                                    .flex_shrink_0()
                                    .overflow_hidden()
                                    .flex_row()
                                    .items_center()
                                    .px(px(theme::SPACING_MD))
                                    .gap(px(6.0))
                                    .rounded_full()
                                    .bg(move || {
                                        if selected() {
                                            theme::fill_strong()
                                        } else {
                                            theme::fill()
                                        }
                                    })
                                    .active(|style| style.opacity(0.6))
                                    .on_press(move || on_select(&id))
                                    .child(dynamic(move || {
                                        svg(icon(agent_icon(&session.with(|session| {
                                            session.as_ref().map_or_else(String::new, |session| {
                                                session.agent.clone()
                                            })
                                        }))))
                                        .size(px(14.0))
                                        .flex_shrink_0()
                                        .text_color(theme::text_secondary)
                                    }))
                                    .child(
                                        text(move || {
                                            session.with(|session| {
                                                session
                                                    .as_ref()
                                                    .map_or_else(String::new, |session| {
                                                        session.title.clone()
                                                    })
                                            })
                                        })
                                        .truncate()
                                        .min_w(px(0.0))
                                        .flex_shrink(1.0)
                                        .overflow_hidden()
                                        .text_size(px(theme::font_micro()))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(
                                            move || {
                                                if selected() {
                                                    theme::foreground()
                                                } else {
                                                    theme::text_secondary()
                                                }
                                            },
                                        ),
                                    )
                            },
                        )),
                )
        },
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
        .flex_shrink_0()
        .px(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_XS))
        .child(
            view()
                .w_full()
                .min_h(px(theme::ROUND_CONTROL))
                .pl(px(surface::ROW_INSET))
                .pr(px(theme::SPACING_XS))
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .rounded(px(theme::RADIUS_BUTTON))
                .bg(theme::fill)
                .child(surface::status_dot(move || match connection.get() {
                    ConnectionState::Connecting => theme::accent_yellow(),
                    ConnectionState::Online => theme::accent_success(),
                    ConnectionState::Offline => theme::accent_red(),
                }))
                .child(
                    text(move || {
                        error.get().unwrap_or_else(|| {
                            match connection.get() {
                                ConnectionState::Connecting => {
                                    copy::text("Reconnecting...", "正在重新连接…", "正在重新連線…")
                                }
                                ConnectionState::Online => {
                                    copy::text("Connected", "已连接", "已連線")
                                }
                                ConnectionState::Offline => copy::text(
                                    "Desktop is unavailable",
                                    "桌面端不可用",
                                    "桌面版無法使用",
                                ),
                            }
                            .into()
                        })
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::text_secondary),
                )
                .child(
                    action::link(copy::text("Retry", "重试", "重試"), move || on_retry())
                        .disabled(move || connection.get() == ConnectionState::Connecting),
                ),
        )
}

/// The transcript: a row per entry, or what to do when there is none.
fn timeline_view(
    host: SessionHost,
    jump_to_message: Signal<Option<i64>>,
    pager: Pager,
    tab: Signal<WorkbenchTab>,
) -> impl IntoElement {
    let on_terminal = move || {
        tab.set(WorkbenchTab::Terminal);
        pager.open(ShellPage::Workbench);
    };
    // The strips above take the bar's room when one of them is shown; the list
    // takes it only when none is (see its `content_padding_top`).
    let tabs_above = group_tabs_visible(&host);
    let connection = host.connection;
    let find_open = host.find_open;
    let banner_above = computed(move || connection.get() != ConnectionState::Online);
    // The tabs and the connection strip are siblings of the list, so when
    // either is up it has already cleared the bar and the list starts under it.
    let sibling_strip_above = computed(move || tabs_above.get() || banner_above.get());
    // The find bar sits inside the list's own column, under those siblings:
    // it clears the bar itself only when neither of them did.
    let strip_above = computed(move || sibling_strip_above.get() || find_open.get());
    let source = host.timeline;
    let visible = computed(move || source.with(|entries| visible_timeline(entries)));
    let position_for = move |predicate: &dyn Fn(&TimelineEntry) -> bool| {
        visible.with(|indices| {
            source.with(|entries| {
                indices
                    .iter()
                    .position(|&index| entries.get(index).is_some_and(predicate))
            })
        })
    };
    let reveal = signal(None::<String>);
    let reveal_row: RevealRow = Rc::new({
        let on_reveal_row = host.on_reveal_row.clone();
        move |id, detail| {
            on_reveal_row(id, detail);
            reveal.set(Some(id.to_owned()));
        }
    });
    let active = host.active;
    let status = host.conversation;
    let on_refresh = host.on_refresh.clone();
    let on_new = host.on_new_session.clone();
    let terminal = active_session(&host);
    let find_query = signal(String::new());
    let find_cursor = signal(0usize);
    let find_matches = computed(move || {
        let query = find_query.with(|query| query.trim().to_lowercase());
        if query.is_empty() {
            return Vec::new();
        }
        source.with(|entries| {
            entries
                .iter()
                .filter(|entry| searchable_match(entry, &query))
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
        })
    });
    // The row the bar is on: its own hits are painted stronger than the rest,
    // so the row it scrolled to is still identifiable among many.
    let find_active_row = computed(move || {
        find_matches.with(|matches| {
            matches
                .get(find_cursor.get().min(matches.len().saturating_sub(1)))
                .cloned()
        })
    });
    let row_actions = TranscriptActions {
        find: FindMarking {
            query: find_query,
            active_row: find_active_row,
        },
        toggle_turn: host.on_toggle_turn.clone(),
        toggle_detail: host.on_toggle_detail.clone(),
        fork: host.on_fork.clone(),
        editable_sequence: host.editable_sequence,
        edit: host.on_edit_message.clone(),
        attachment: host.on_open_attachment.clone(),
        jump: jump_to_message,
        entries: source,
        visible,
        reveal: reveal_row.clone(),
        clock: crate::ui::activity::ActivityClock::start(),
        open_file: {
            let open_files = host.on_open_files.clone();
            let open_file = host.on_open_file.clone();
            Rc::new(move |path| {
                open_files();
                open_file(path, None);
                tab.set(WorkbenchTab::Files);
                pager.open(ShellPage::Workbench);
            })
        },
    };
    let list_ref = element_ref();
    effect(move || {
        if let Some(id) = reveal.get()
            && position_for(&|entry| entry.id == id).is_some()
        {
            set_timeout(std::time::Duration::from_millis(16), move || {
                if reveal.get_untracked().as_ref() == Some(&id)
                    && let Some(index) = position_for(&|entry| entry.id == id)
                {
                    reveal.set(None);
                    // A find's match is aimed at, not centred: a row taller
                    // than the viewport cannot be shown whole, and the word
                    // the reader typed may sit anywhere inside it. Placing the
                    // match a quarter down the page puts it in view even in a
                    // long answer, which is the case the bar exists for.
                    let aim = find_query.with_untracked(|query| {
                        source.with_untracked(|entries| {
                            entries
                                .iter()
                                .find(|entry| entry.id == id)
                                .map_or(0.0, |entry| {
                                    find::match_aim(&searchable_text(entry), query)
                                })
                        })
                    });
                    let row_height = 72.0;
                    let view = window().height.0;
                    let offset = -(aim * row_height - view * 0.25).max(0.0);
                    list_ref.scroll_to_index(
                        index as u32,
                        Percentage(0.5),
                        px(offset),
                        !crate::motion::reduce_motion(),
                    );
                }
            });
        }
    });
    effect(move || {
        if let Some(sequence) = jump_to_message.get() {
            if let Some(index) = position_for(&|entry| {
                entry.kind == EntryKind::User && entry.last_sequence == sequence
            }) {
                list_ref.scroll_to_index(
                    index as u32,
                    Percentage(0.0),
                    px(0.0),
                    !crate::motion::reduce_motion(),
                );
            }
            jump_to_message.set(None);
        }
    });
    effect(move || {
        if !find_open.get() {
            find_query.set(String::new());
            find_cursor.set(0);
        }
    });
    effect(move || {
        active.get();
        find_query.set(String::new());
        find_cursor.set(0);
        reveal.set(None);
    });
    let away_from_end = signal(false);
    let find_reveal: Rc<dyn Fn(&str)> = Rc::new(move |id| {
        let query = find_query.with_untracked(|query| query.trim().to_lowercase());
        let open_detail = source.with_untracked(|entries| {
            entries
                .iter()
                .find(|entry| entry.id == id)
                .is_some_and(|entry| {
                    let matches = |value: &str| value.to_lowercase().contains(&query);
                    entry.kind == EntryKind::Reasoning && matches(&entry.body)
                        || entry.activity.as_ref().is_some_and(|activity| {
                            !matches(&entry.title)
                                && !matches(&activity.target)
                                && !activity.path.as_deref().is_some_and(matches)
                        })
                })
        });
        reveal_row(id, open_detail);
    });
    let has_older = host.has_older;
    let loading_older = host.loading_older;
    let on_load_older = host.on_load_older.clone();
    // A bottom-aligned transcript whose rendered rows fit inside the viewport
    // has zero scroll range (`content_size.height <= size.height`), so swiping
    // never fires `on_scroll` and `has_older` would be unreachable. Watch the
    // list's post-layout geometry and, once layout confirms the transcript
    // cannot scroll, fetch older pages automatically until either the list
    // overflows the viewport or history is exhausted.
    let last_scroll_unscrollable: Rc<Cell<Option<(nsh_core::NodeId, bool)>>> =
        Rc::new(Cell::new(None));
    let scroll_watch_rev = signal(0u64);
    {
        let last_scroll_unscrollable = last_scroll_unscrollable.clone();
        watch_scroll(
            move || list_ref.node(),
            move |event: &ScrollEvent| {
                let Some(node) = list_ref.node() else {
                    return;
                };
                let unscrollable = event.size.height.0 > 0.0
                    && event.content_size.height.0 - event.size.height.0 <= 1.0;
                if last_scroll_unscrollable.get() != Some((node, unscrollable)) {
                    last_scroll_unscrollable.set(Some((node, unscrollable)));
                    scroll_watch_rev.update(|rev| *rev = rev.wrapping_add(1));
                }
            },
        );
    }
    let last_auto_older_snapshot: Rc<RefCell<Option<(Option<String>, usize, Option<String>)>>> =
        Rc::new(RefCell::new(None));
    let auto_older_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    {
        let on_load_older = on_load_older.clone();
        let last_scroll_unscrollable = last_scroll_unscrollable.clone();
        let last_auto_older_snapshot = last_auto_older_snapshot.clone();
        let auto_older_gen = auto_older_gen.clone();
        effect(move || {
            let _active_id = active.get();
            let more = has_older.get();
            let loading = loading_older.get();
            let _watch_rev = scroll_watch_rev.get();
            let visible_len = visible.with(|indices| {
                let _first_id = indices.first().and_then(|&index| {
                    source.with(|entries| entries.get(index).map(|entry| entry.id.clone()))
                });
                indices.len()
            });
            if !more || visible_len == 0 {
                *last_auto_older_snapshot.borrow_mut() = None;
                auto_older_gen.set(auto_older_gen.get().wrapping_add(1));
                return;
            }
            if loading {
                return;
            }
            let Some(node_at_schedule) = list_ref.node() else {
                return;
            };
            let generation = auto_older_gen.get().wrapping_add(1);
            auto_older_gen.set(generation);
            let on_load_older = on_load_older.clone();
            let last_scroll_unscrollable = last_scroll_unscrollable.clone();
            let last_auto_older_snapshot = last_auto_older_snapshot.clone();
            let auto_older_gen = auto_older_gen.clone();
            spawn(async move {
                // Wait for the current commit's layout and `sync_watches` to
                // settle so `last_scroll_unscrollable` reflects the rows now on
                // screen rather than a shorter previous render.
                if list_ref.measure().await.is_none() {
                    return;
                }
                if auto_older_gen.get() != generation {
                    return;
                }
                if !has_older.get_untracked() || loading_older.get_untracked() {
                    return;
                }
                let Some(current_node) = list_ref.node() else {
                    return;
                };
                if current_node != node_at_schedule {
                    return;
                }
                if last_scroll_unscrollable.get() != Some((current_node, true)) {
                    *last_auto_older_snapshot.borrow_mut() = None;
                    return;
                }
                let (cur_len, cur_first_id) = visible.with_untracked(|indices| {
                    let first_id = indices.first().and_then(|&index| {
                        source.with_untracked(|entries| {
                            entries.get(index).map(|entry| entry.id.clone())
                        })
                    });
                    (indices.len(), first_id)
                });
                if cur_len == 0 {
                    return;
                }
                let snapshot = (active.get_untracked(), cur_len, cur_first_id);
                if *last_auto_older_snapshot.borrow() == Some(snapshot.clone()) {
                    return;
                }
                *last_auto_older_snapshot.borrow_mut() = Some(snapshot);
                on_load_older();
            });
        });
    }
    // The ring the "earlier messages" strip turns while a page is on its way.
    let older_clock = crate::ui::activity::ActivityClock::start();
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(when(
            move || active.get().is_none(),
            move || {
                let on_new = on_new.clone();
                surface::empty_state(
                    icon!("message-square"),
                    copy::text("No session open", "没有打开的会话", "沒有開啟的工作階段"),
                    copy::text(
                        "Pick one from the sessions page, or start a new one.",
                        "从会话页选择一个，或者新建一个。",
                        "從工作階段頁選擇一個，或者新增一個。",
                    ),
                )
                .child(
                    action::tonal(
                        copy::text("New session", "新建会话", "新增工作階段"),
                        move || on_new(),
                    )
                    .mt(px(theme::SPACING_SM)),
                )
            },
        ))
        .child(when(
            move || active.get().is_some() && visible.with(Vec::is_empty),
            {
                let on_terminal = Rc::new(on_terminal);
                move || {
                    dynamic({
                        let on_terminal = on_terminal.clone();
                        let on_refresh = on_refresh.clone();
                        move || {
                            if terminal.with(|session| {
                                session.as_ref().is_some_and(|session| session.terminal)
                            }) {
                                let on_terminal = on_terminal.clone();
                                return surface::empty_state(
                                    icon!("square-terminal"),
                                    copy::text(
                                        "This session runs in a terminal",
                                        "这个会话运行在终端中",
                                        "這個工作階段在終端機中執行",
                                    ),
                                    copy::text(
                                        "Its screen is on the workbench.",
                                        "它的画面在工作台里。",
                                        "它的畫面在工作台裡。",
                                    ),
                                )
                                .child(
                                    action::tonal(
                                        copy::text("Open terminal", "打开终端", "開啟終端機"),
                                        move || on_terminal(),
                                    )
                                    .mt(px(theme::SPACING_SM)),
                                )
                                .into_element();
                            }
                            match status.get() {
                                ConversationState::Loading => empty_hint(copy::text(
                                    "Loading conversation…",
                                    "正在加载会话…",
                                    "正在載入工作階段…",
                                ))
                                .into_element(),
                                ConversationState::Ready => surface::empty_state(
                                    icon!("message-square"),
                                    copy::text("No messages yet", "还没有消息", "還沒有訊息"),
                                    copy::text(
                                        "Write the first one below.",
                                        "在下方写下第一条消息。",
                                        "在下方寫下第一則訊息。",
                                    ),
                                )
                                .into_element(),
                                ConversationState::Failed(error) => {
                                    let on_refresh = on_refresh.clone();
                                    surface::empty_state(
                                        icon!("triangle-alert"),
                                        error,
                                        copy::text(
                                            "The conversation could not be read.",
                                            "无法读取这个会话。",
                                            "無法讀取這個工作階段。",
                                        ),
                                    )
                                    .child(
                                        action::tonal(
                                            copy::text("Retry", "重试", "重試"),
                                            move || on_refresh(),
                                        )
                                        .mt(px(theme::SPACING_SM)),
                                    )
                                    .into_element()
                                }
                            }
                        }
                    })
                }
            },
        ))
        .child(when(
            move || !visible.with(Vec::is_empty),
            move || {
                view()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex_col()
                    .child(when(move || find_open.get(), {
                        let close = find_open;
                        let find_reveal = find_reveal.clone();
                        // The bar floats over the page, so the find bar clears
                        // it here unless the tabs or the connection strip have
                        // already taken that room. It also sits above the
                        // glass: the band blurs what passes under it, and a
                        // field being typed into must not be under it.
                        let clear_bar = !sibling_strip_above.get_untracked();
                        move || {
                            view()
                                .z_index(2)
                                .style_when(
                                    move || clear_bar,
                                    |style| style.pt(px(top_bar_inset())),
                                )
                                .child(find_bar(
                                    find_query,
                                    find_matches,
                                    find_cursor,
                                    find_reveal.clone(),
                                    close,
                                ))
                        }
                    }))
                    // The older messages come by themselves: scrolling near the
                    // top of what is loaded asks for the next page, and the
                    // strip above the transcript says it is on its way. A
                    // button would have made the reader stop and press it at
                    // the one moment they were already reading backwards.
                    .child(when(
                        move || loading_older.get(),
                        move || {
                            view()
                                .w_full()
                                .items_center()
                                .py(px(theme::SPACING_XS))
                                .child(
                                    view()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(theme::SPACING_SM))
                                        .child(crate::ui::activity::activity_ring(
                                            older_clock,
                                            theme::primary,
                                        ))
                                        .child(
                                            text(copy::text(
                                                "Loading earlier...",
                                                "正在加载更早的消息…",
                                                "正在載入更早的訊息…",
                                            ))
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_muted),
                                        ),
                                )
                        },
                    ))
                    .child({
                        let actions = row_actions.clone();
                        view()
                            .relative()
                            .flex_1()
                            .min_h(px(0.0))
                            .child(
                                virtual_list(
                                    move || visible.with(Vec::len),
                                    move |at: Position| entry_row(at, actions.clone()),
                                )
                                .keys(move || {
                                    visible.with(|indices| {
                                        source.with(|entries| {
                                            indices
                                                .iter()
                                                .filter_map(|&index| {
                                                    entries.get(index).map(|entry| entry.id.clone())
                                                })
                                                .collect::<Vec<_>>()
                                        })
                                    })
                                })
                                .estimated_item_size(px(72.0))
                                .element_ref(&list_ref)
                                .test_id("transcript")
                                .alignment(ListAlignment::Bottom)
                                .follow_end(px(120.0))
                                // The bar floats over the list rather than
                                // sitting above it, so its height is the
                                // list's own leading space: the first row
                                // still clears the bar, while a message
                                // scrolled up travels on under it. When the
                                // group tabs or the connection strip are
                                // shown they have taken that room already,
                                // and the list starts under them.
                                .content_padding_top(move || {
                                    if strip_above.get() {
                                        px(0.0)
                                    } else {
                                        px(top_bar_inset())
                                    }
                                })
                                .on_scroll({
                                    let on_load_older = on_load_older.clone();
                                    move |event: &ScrollEvent| {
                                        // Only a transcript with somewhere to go
                                        // offers the way back to its end. A page
                                        // holding one short message has no scroll
                                        // range at all, and the top padding alone
                                        // can make the arithmetic below come out
                                        // positive — which put a "jump to latest"
                                        // button on a page that was already at it.
                                        let overflow =
                                            event.content_size.height.0 - event.size.height.0;
                                        away_from_end.set(
                                            overflow > 120.0 && overflow - event.offset.y.0 > 120.0,
                                        );
                                        // Reading near the top is the ask: fetch the
                                        // next page then, not when a button is
                                        // pressed. `loading_older` is the guard, so
                                        // a scroll that keeps firing while the page
                                        // is in flight asks once.
                                        if event.offset.y.0 <= OLDER_TRIGGER
                                            && has_older.get_untracked()
                                            && !loading_older.get_untracked()
                                        {
                                            on_load_older();
                                        }
                                    }
                                })
                                .on_scroll_drag_start({
                                    let on_load_older = on_load_older.clone();
                                    move |offset: &Point<Points>| {
                                        if offset.y.0 <= OLDER_TRIGGER
                                            && has_older.get_untracked()
                                            && !loading_older.get_untracked()
                                        {
                                            on_load_older();
                                        }
                                    }
                                })
                                .flex_1(),
                            )
                            // The transcript runs out under the prompt card
                            // rather than being cut by it. The top needs no
                            // strip of its own: the glass over the page does
                            // that, and a second band would only darken it.
                            .child(edge_fade())
                            .child(when(
                                move || away_from_end.get() && !find_open.get(),
                                move || {
                                    surface::round_button(
                                        icon!("chevron-down"),
                                        copy::text(
                                            "Jump to latest",
                                            "跳到最新消息",
                                            "跳到最新訊息",
                                        ),
                                        move || {
                                            list_ref.scroll_to_end(!crate::motion::reduce_motion());
                                            away_from_end.set(false);
                                        },
                                    )
                                    .absolute()
                                    .bottom(px(theme::SPACING_MD))
                                    .right(px(theme::SPACING_LG))
                                    .bg(theme::block_surface)
                                    .border_1()
                                    .border_color(theme::hairline)
                                    .shadow_md()
                                },
                            ))
                    })
            },
        ))
}

fn visible_timeline(entries: &[TimelineEntry]) -> Vec<usize> {
    let mut headers = std::collections::HashMap::new();
    let mut visible = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let shown = entry
            .parent_id
            .as_deref()
            .is_none_or(|id| headers.get(id).copied().unwrap_or(false));
        if matches!(
            entry.kind,
            EntryKind::ProcessHeader | EntryKind::ActivityGroup
        ) {
            headers.insert(entry.id.as_str(), shown && entry.expanded);
        }
        if shown {
            visible.push(index);
        }
    }
    visible
}

/// The text a find searches in one row: what the row actually shows, so a hit
/// is a word the reader can see.
fn searchable_text(entry: &TimelineEntry) -> String {
    let mut text = entry.title.clone();
    if !entry.body.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&entry.body);
    }
    if let Some(activity) = entry.activity.as_ref() {
        for part in std::iter::once(&activity.target)
            .chain(activity.path.as_ref())
            .chain(activity.details.iter().map(|detail| &detail.text))
        {
            if !part.is_empty() {
                text.push('\n');
                text.push_str(part);
            }
        }
    }
    text
}

fn searchable_match(entry: &TimelineEntry, query: &str) -> bool {
    let matches = |value: &str| value.to_lowercase().contains(query);
    !matches!(
        entry.kind,
        EntryKind::ProcessHeader
            | EntryKind::ActivityGroup
            | EntryKind::Generating
            | EntryKind::Changes
    ) && (matches(&entry.title)
        || matches(&entry.body)
        || entry.activity.as_ref().is_some_and(|activity| {
            matches(&activity.target)
                || activity.path.as_deref().is_some_and(matches)
                || activity.details.iter().any(|detail| matches(&detail.text))
        }))
}

fn find_bar(
    query: Signal<String>,
    matches: Memo<Vec<String>>,
    cursor: Signal<usize>,
    reveal: Rc<dyn Fn(&str)>,
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
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_SM))
        .pb(px(theme::SPACING_SM))
        .child(
            surface::field_pill()
                .flex_1()
                .min_w(px(0.0))
                .pr(px(theme::SPACING_MD))
                .mr(px(theme::SPACING_XS))
                .child(surface::field_glyph(icon!("search")))
                .child(
                    field::text_field_bare_with(
                        query,
                        copy::text("Find in conversation", "在会话中查找", "在工作階段中尋找"),
                        {
                            let reveal = reveal.clone();
                            move |_| {
                                cursor.set(0);
                                if let Some(id) = matches.with(|matches| matches.first().cloned()) {
                                    reveal(&id);
                                }
                            }
                        },
                    )
                    .auto_focus(true)
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full(),
                )
                .child(
                    text(move || count.get())
                        .flex_shrink_0()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                ),
        )
        .child(find_step(true, matches, cursor, reveal.clone()))
        .child(find_step(false, matches, cursor, reveal))
        .child(surface::ghost_button(
            icon!("x"),
            copy::text("Close", "关闭", "關閉"),
            move || {
                close.set(false);
                dismiss_keyboard();
            },
        ))
}

fn find_step(
    previous: bool,
    matches: Memo<Vec<String>>,
    cursor: Signal<usize>,
    reveal: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    surface::ghost_button(
        if previous {
            icon!("chevron-up")
        } else {
            icon!("chevron-down")
        },
        if previous {
            copy::text("Previous match", "上一个匹配", "上一個符合")
        } else {
            copy::text("Next match", "下一个匹配", "下一個符合")
        },
        move || {
            let total = matches.with(|matches| matches.len());
            if total == 0 {
                return;
            }
            let next = if previous {
                (cursor.get_untracked() + total - 1) % total
            } else {
                (cursor.get_untracked() + 1) % total
            };
            cursor.set(next);
            if let Some(id) = matches.with(|matches| matches.get(next).cloned()) {
                reveal(&id);
            }
        },
    )
    .disabled(move || matches.with(Vec::is_empty))
}

pub(super) fn empty_hint(message: &'static str) -> impl IntoElement {
    view().flex_1().items_center().justify_center().child(
        text(message)
            .text_size(px(theme::font_detail()))
            .text_color(theme::text_muted),
    )
}

#[derive(Clone)]
struct TranscriptActions {
    toggle_turn: Rc<dyn Fn(&str)>,
    toggle_detail: ToggleDetail,
    fork: Rc<dyn Fn(i64)>,
    editable_sequence: Signal<i64>,
    edit: EditMessage,
    attachment: OpenAttachment,
    jump: Signal<Option<i64>>,
    entries: Signal<Vec<TimelineEntry>>,
    visible: Memo<Vec<usize>>,
    reveal: RevealRow,
    open_file: Rc<dyn Fn(&str)>,
    /// The clock every working mark in the transcript turns by.
    clock: crate::ui::activity::ActivityClock,
    /// What the find bar is looking for, and which hit it is on. Every row
    /// reads it: the matched words are marked where they sit, and the row
    /// holding the current hit paints it stronger than the rest.
    find: FindMarking,
}

/// The find bar's query, as the rows read it.
#[derive(Clone, Copy)]
struct FindMarking {
    query: Signal<String>,
    /// The row the bar is on, so its own hits are painted stronger.
    active_row: Memo<Option<String>>,
}

/// The line a working row is named on.
/// How near the top of the transcript the reader scrolls before the older
/// messages are asked for. A few rows of lead, so the page is already there by
/// the time the finger reaches the first one.
const OLDER_TRIGGER: f32 = 240.0;

const STEP_HEIGHT: f32 = 38.0;
/// The column a turn's rail and its nodes take.
const RAIL_SLOT: f32 = 20.0;
/// A node on the rail: the glyph of one step.
const RAIL_NODE: f32 = 20.0;
/// The gap the rail keeps from a node.
const RAIL_GAP: f32 = 2.0;

/// How one working row hangs on its turn's rail. A turn's work is a fold and
/// the rows under it; the rail runs from the fold through each of them, so
/// they read as one run however many rows it takes.
#[derive(Clone, Copy)]
struct Rail {
    /// The row is inside a fold: the rail reaches it from above.
    above: Memo<bool>,
    /// The row after it is inside one too: the rail goes on below.
    below: Memo<bool>,
}

/// Whether a row opens prose rather than continuing a run of calls.
///
/// A fold's rows are its calls and the paragraphs between them; the rail is the
/// line that ties the calls together, so it ends where a paragraph starts and
/// picks up again after it.
fn starts_prose(entry: &TimelineEntry) -> bool {
    matches!(entry.kind, EntryKind::Agent | EntryKind::Reasoning)
}

/// The strip the transcript fades out in at its foot.
fn edge_fade() -> impl IntoElement {
    // The strip fades the page's own tone, which is the tone the conversation
    // is drawn on: a fade from the flat background left a paler band at the
    // end of the list wherever the page took its stepped tone.
    edge_fade_between(
        false,
        theme::SPACING_MD,
        theme::page_adaptive,
        theme::page_clear_adaptive,
    )
}

/// The glass the conversation passes under at the top of the page.
///
/// The bar above is three floating panes, not a strip, so what a message meets
/// on its way up is this band: the page's tone over a blur of whatever lies
/// behind it, both thinning out downwards. A message runs into the blur, keeps
/// showing through it, and is gone only at the very top — where the page's own
/// tone alone would cut the transcript off on a hard line at the bar's edge.
///
/// The tone and the blur are one view, so the mask that fades the band fades
/// both together and there is no seam where one stops and the other goes on.
fn top_glass() -> impl IntoElement {
    // From the top of the page (the status bar included) past the bar's lower
    // edge, with room under it for the fade to run out: the band is taller than
    // the bar so the transition is a long one rather than a short edge.
    let height = move || px(safe_area().top.0 + theme::HEADER_HEIGHT + theme::SPACING_LG * 3.0);
    view()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .h(height)
        // Over the transcript, under the bar's controls.
        .z_index(1)
        .bg(theme::block_page_veil_adaptive())
        .backdrop_blur(px(TOP_GLASS_BLUR))
        // The tone and the blur hold only through the status bar's band and
        // then let go slowly: the fade runs across most of the band's height,
        // so a message surfaces out of the glass gradually instead of crossing
        // a short edge, and is fully itself before the bar's lower rim.
        .mask_linear(
            deg(180.0),
            [
                (black(), percentage(0.0)),
                (black(), percentage(0.08)),
                (transparent_black(), percentage(0.86)),
            ],
        )
        .pointer_events(PointerEvents::None)
}

/// How much the top band blurs what passes under it.
const TOP_GLASS_BLUR: f32 = 18.0;

/// The space the bar takes at the top of the conversation: the status bar and
/// the floating controls over it.
fn top_bar_inset() -> f32 {
    safe_area().top.0 + theme::HEADER_HEIGHT
}

/// A strip that fades `opaque` into `clear` at one edge of the surface it
/// overlays, so content runs out under the band above or below it instead of
/// being cut by a rule. `top` puts the opaque end at the top.
fn edge_fade_between(
    top: bool,
    height: f32,
    opaque: fn() -> AdaptiveColor,
    clear: fn() -> AdaptiveColor,
) -> impl IntoElement {
    let strip = view()
        .absolute()
        .left(px(0.0))
        .right(px(0.0))
        .h(px(height))
        .gradient_from(opaque)
        .gradient_to(clear)
        .pointer_events(PointerEvents::None);
    if top {
        strip.top(px(0.0)).bg_linear_to_b()
    } else {
        strip.bottom(px(0.0)).bg_linear_to_t()
    }
}

/// A step of a turn's work: its node on the rail, the line that names it, and
/// what it opens under that line.
fn step(
    rail: Rail,
    node: impl IntoElement,
    line: impl IntoElement,
    body: impl IntoElement,
) -> impl IntoElement {
    let node_top = (STEP_HEIGHT - RAIL_NODE) / 2.0;
    let segment = || {
        view()
            .absolute()
            .left(px((RAIL_SLOT - 1.0) / 2.0))
            .w(px(1.0))
            .bg(theme::border_default)
    };
    view()
        .w_full()
        .min_w(px(0.0))
        .flex_row()
        .child(
            view()
                .w(px(RAIL_SLOT))
                .flex_shrink_0()
                .relative()
                .child(
                    segment()
                        .top(px(0.0))
                        .h(px(node_top - RAIL_GAP))
                        .style_when(move || !rail.above.get(), |style| style.hidden()),
                )
                .child(
                    segment()
                        .top(px(node_top + RAIL_NODE + RAIL_GAP))
                        .bottom(px(0.0))
                        .style_when(move || !rail.below.get(), |style| style.hidden()),
                )
                .child(
                    view()
                        .absolute()
                        .top(px(node_top))
                        .left(px(0.0))
                        .size(px(RAIL_NODE))
                        .items_center()
                        .justify_center()
                        .child(node),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .flex_col()
                .pl(px(theme::SPACING_SM))
                .child(line)
                .child(body),
        )
}

/// A step's node: the glyph of what it did, in the ink of how it went.
fn step_node(glyph: ImageSrc, ink: impl Fn() -> Hsla + 'static) -> impl IntoElement {
    svg(glyph).size(px(15.0)).text_color(ink)
}

/// The line a fold or a group is named on: its name, the working mark while
/// it runs, and the disclosure when it opens.
#[allow(clippy::too_many_arguments)]
fn step_line(
    title: Memo<String>,
    running: Memo<bool>,
    expanded: Memo<bool>,
    expandable: Memo<bool>,
    clock: crate::ui::activity::ActivityClock,
    query: Signal<String>,
    active: Memo<bool>,
    on_toggle: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(STEP_HEIGHT))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .accessibility_label(move || SharedString::from(title.get()))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled(move || !expandable.get())
        .on_press(on_toggle)
        .child(view().flex_1().min_w(px(0.0)).child(find::marked_line(
            title,
            query,
            active,
            |line| {
                line.truncate()
                    .text_size(px(theme::font_detail()))
                    .text_color(theme::text_secondary)
            },
        )))
        .child(when(
            move || running.get(),
            move || crate::ui::activity::activity_ring(clock, theme::primary),
        ))
        .child(when(
            move || expandable.get(),
            move || disclosure_chevron(expanded),
        ))
}

/// The line that says the Agent is at work before it has anything to show.
///
/// A V of dots leads it, one highlight walking the stroke: the mark the
/// desktop's timeline draws while a turn runs, so both clients say "still
/// working" with the same thing.
fn working_row(
    body: Memo<String>,
    _clock: crate::ui::activity::ActivityClock,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    // The V walks its own, slower lap: the row's shared clock is timed for the
    // ring, and at that speed the highlight jumps along the stroke.
    let clock = crate::ui::activity::ActivityClock::with_lap(crate::ui::activity::V_LAP_MS);
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        // The row sits directly above the composer, and the prompt card is a
        // surface of its own: generous air under it keeps the status line clear
        // of the bottom fade and the prompt card below.
        .pt(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_XL * 2.0))
        .child(crate::ui::activity::activity_v(clock, theme::text_muted))
        .child(view().flex_1().min_w(px(0.0)).child(find::marked_line(
            body,
            query,
            active,
            |line| {
                line.text_size(px(theme::font_detail()))
                    .text_color(theme::text_muted)
            },
        )))
}

/// A note from the system: said beside the conversation, not in it.
fn notice_row(body: Memo<String>, query: Signal<String>, active: Memo<bool>) -> impl IntoElement {
    view().w_full().py(px(theme::SPACING_XS)).child(
        view()
            .w_full()
            .rounded(px(theme::RADIUS_CHIP))
            .bg(theme::fill)
            .px(px(theme::SPACING_MD))
            .py(px(theme::SPACING_SM))
            .child(find::marked_line(body, query, active, |line| {
                line.text_size(px(theme::font_caption()))
                    .line_height(theme::line_height_for(theme::font_caption()))
                    .text_color(theme::text_secondary)
            })),
    )
}

fn entry_row(at: Position, actions: TranscriptActions) -> impl IntoElement {
    let entries = actions.entries;
    let visible = actions.visible;
    let entry = computed(move || {
        visible.with(|indices| {
            indices
                .get(at.index())
                .and_then(|&index| entries.with(|entries| entries.get(index).cloned()))
        })
    });
    // The rail ties a run of calls together, and only that: it is drawn
    // between two rows of the same fold that are both calls. A line into a
    // paragraph — or out of one — says the paragraph is part of the call's run,
    // which it is not.
    let step_row = move |index: usize| {
        visible.with(|indices| {
            indices.get(index).is_some_and(|&index| {
                entries.with(|entries| {
                    entries
                        .get(index)
                        .is_some_and(|entry| entry.parent_id.is_some() && !starts_prose(entry))
                })
            })
        })
    };
    let rail = Rail {
        above: computed(move || step_row(at.index().saturating_sub(1)) && step_row(at.index())),
        below: computed(move || step_row(at.index()) && step_row(at.index() + 1)),
    };
    view()
        .w_full()
        .px(px(theme::SPACING_LG))
        // No row-level tint: what a find marks is the word, not the message
        // around it. A whole row painted made the reader hunt for their own
        // query inside a highlighted paragraph, which is the opposite of what
        // a find is for. `entry_element` marks the matched text instead.
        .child(entry_element(entry, rail, actions))
}

fn entry_element(
    entry: Memo<Option<TimelineEntry>>,
    rail: Rail,
    actions: TranscriptActions,
) -> impl IntoElement {
    let TranscriptActions {
        toggle_turn: on_toggle_turn,
        toggle_detail: on_toggle_detail,
        fork: on_fork,
        editable_sequence,
        edit: on_edit_message,
        attachment: on_open_attachment,
        jump,
        entries,
        reveal,
        open_file,
        clock,
        find,
        ..
    } = actions;

    let kind = computed(move || entry.with(|e| e.as_ref().map(|e| e.kind)));
    let body =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.body.clone()).unwrap_or_default()));
    let title =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.title.clone()).unwrap_or_default()));
    let meta =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.meta.clone()).unwrap_or_default()));
    let agent =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.agent.clone()).unwrap_or_default()));
    let conclusion = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.conclusion)));
    let reaction = computed(move || entry.with(|e| e.as_ref().and_then(|e| e.reaction.clone())));
    let delivery = computed(move || entry.with(|e| e.as_ref().and_then(|e| e.delivery)));
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
    let expanded = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.expanded)));
    let sequence = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.last_sequence)));
    let prompt = computed(move || {
        let turn = turn_id.get();
        entries.with(|entries| {
            entries
                .iter()
                .find(|entry| entry.kind == EntryKind::User && entry.turn_id == turn)
                .map(|entry| entry.last_sequence)
        })
    });
    let started_at_ms = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.started_at_ms)));
    let ended_at_ms = computed(move || entry.with(|e| e.as_ref().map_or(0, |e| e.ended_at_ms)));
    let complete = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.complete)));
    let expandable = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.expandable)));
    let row_id = computed(move || {
        entry.with(|entry| {
            entry
                .as_ref()
                .map(|entry| entry.id.clone())
                .unwrap_or_default()
        })
    });
    // The row the bar is on paints its own hits stronger; a row with no match
    // is drawn exactly as it was, so a find costs the reader nothing they were
    // not looking for.
    let find_active = computed(move || {
        find.active_row
            .with(|active| active.as_deref() == Some(row_id.get().as_str()))
    });
    let toggle_detail: Rc<dyn Fn()> =
        Rc::new(move || on_toggle_detail(&row_id.get_untracked(), sequence.get_untracked()));
    view()
        .w_full()
        .flex_col()
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::User)),
            move || {
                user_bubble(
                    body,
                    meta,
                    attachments,
                    reaction,
                    delivery,
                    sequence,
                    editable_sequence,
                    on_edit_message.clone(),
                    on_open_attachment.clone(),
                    find.query,
                    find_active,
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
                        expanded,
                        expandable,
                        row_id,
                        agent,
                        rail,
                        clock,
                        on_toggle_turn.clone(),
                        find.query,
                        find_active,
                    )
                }
            },
        ))
        .child(when(
            move || kind.get() == Some(EntryKind::ActivityGroup),
            {
                let on_toggle_turn = on_toggle_turn.clone();
                move || {
                    let toggle = on_toggle_turn.clone();
                    step(
                        rail,
                        step_node(icon!("layers"), move || {
                            if streaming.get() {
                                theme::accent_blue()
                            } else {
                                theme::text_muted()
                            }
                        }),
                        step_line(
                            title,
                            streaming,
                            expanded,
                            expandable,
                            clock,
                            find.query,
                            find_active,
                            move || toggle(&row_id.get_untracked()),
                        ),
                        view(),
                    )
                }
            },
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Agent)),
            {
                let on_fork = on_fork.clone();
                move || {
                    agent_prose(
                        title,
                        body,
                        streaming,
                        sequence,
                        prompt,
                        jump,
                        conclusion,
                        on_fork.clone(),
                        find.query,
                        find_active,
                    )
                }
            },
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Reasoning)),
            {
                let toggle = toggle_detail.clone();
                move || {
                    reasoning_row(
                        body,
                        streaming,
                        expanded,
                        expandable,
                        rail,
                        clock,
                        toggle.clone(),
                        find.query,
                        find_active,
                    )
                }
            },
        ))
        .child(when(
            move || kind.get() == Some(EntryKind::Tool),
            move || {
                activity_row(
                    title,
                    body,
                    entry,
                    streaming,
                    expanded,
                    expandable,
                    rail,
                    toggle_detail.clone(),
                    find.query,
                    find_active,
                )
            },
        ))
        .child(when(
            move || kind.get() == Some(EntryKind::Generating),
            move || working_row(body, clock, find.query, find_active),
        ))
        .child(when(
            move || kind.get() == Some(EntryKind::Notice),
            move || notice_row(body, find.query, find_active),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Error)),
            move || error_card(body, find.query, find_active),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Changes)),
            move || {
                changes_card(
                    title,
                    entry,
                    reveal.clone(),
                    open_file.clone(),
                    find.query,
                    find_active,
                )
            },
        ))
        .child(when(
            move || {
                kind.with(|kind| matches!(kind, Some(EntryKind::Permission | EntryKind::Question)))
            },
            move || decision_card(title, body, meta, find.query, find_active),
        ))
}

/// The mark a reaction pill stands at.
const REACTION_SIZE: f32 = 22.0;
/// How far a reaction hangs past the bubble's lower edge.
///
/// It overlaps the bubble rather than floating clear of it: the desktop anchors
/// its own reaction to the same corner, and a pill set apart reads as another
/// row's.
const REACTION_HANG: f32 = 6.0;

/// What the reader wrote: a bubble on their side of the page, its one sharp
/// corner pointing at it.
#[allow(clippy::too_many_arguments)]
fn user_bubble(
    body: Memo<String>,
    meta: Memo<String>,
    attachments: Memo<Vec<AttachmentItem>>,
    reaction: Memo<Option<SharedString>>,
    delivery: Memo<Option<MessageDelivery>>,
    sequence: Memo<i64>,
    editable_sequence: Signal<i64>,
    on_edit: EditMessage,
    on_open_attachment: OpenAttachment,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    // The pill the bubble's lower edge carries: the Agent's reaction and, when
    // the message was not an ordinary prompt, the glyph for what it did. They
    // share one surface so the row does not grow a second one, as the desktop's
    // edge region does.
    let has_edge_marks =
        computed(move || reaction.with(Option::is_some) || delivery.with(Option::is_some));
    view()
        .w_full()
        .flex_col()
        .items_end()
        .gap(px(theme::SPACING_XS))
        .pt(px(theme::SPACING_MD))
        .child(when(
            move || !meta.with(String::is_empty),
            move || {
                text(move || meta.get())
                    .pr(px(theme::SPACING_XS))
                    .text_size(px(theme::font_hint()))
                    .text_color(theme::text_muted)
            },
        ))
        .child(
            // The bubble and the marks it earned: both are anchored to the
            // bubble's own lower corner rather than stacked under it, so they
            // read as belonging to this message the way the desktop's do, and
            // the row does not grow a second surface.
            view()
                .max_w(relative(0.86))
                .relative()
                .child(
                    view()
                        .rounded(px(theme::RADIUS_BUBBLE))
                        .rounded_br(px(theme::RADIUS_BUBBLE_TAIL))
                        .bg(theme::bubble)
                        .px(px(surface::ROW_INSET))
                        .py(px(10.0))
                        .child(find::marked_line(body, query, active, |line| {
                            line.selectable(true)
                                .text_size(px(theme::font_body()))
                                .line_height(theme::line_height_for(theme::font_body()))
                                .text_color(theme::foreground)
                        })),
                )
                .child(when(
                    move || has_edge_marks.get(),
                    move || {
                        view()
                            .absolute()
                            .bottom(px(-REACTION_HANG))
                            .right(px(theme::SPACING_XS))
                            .h(px(REACTION_SIZE))
                            .flex_row()
                            .items_center()
                            .gap(px(2.0))
                            .px(px(theme::SPACING_SM))
                            .rounded_full()
                            .bg(theme::block_surface)
                            .border_1()
                            .border_color(theme::hairline)
                            .child(when(
                                move || reaction.with(Option::is_some),
                                move || {
                                    text(move || {
                                        reaction
                                            .with(|reaction| reaction.clone())
                                            .unwrap_or_default()
                                    })
                                    .text_size(px(theme::font_caption()))
                                },
                            ))
                            .child(when(
                                move || delivery.with(Option::is_some),
                                move || {
                                    // The glyph the desktop uses for the same
                                    // fact: a message that went into a turn that
                                    // was already running, or one that cut it off
                                    // and took over. `icon!` takes a literal, so
                                    // each case names its own file.
                                    if delivery.get() == Some(MessageDelivery::Resend) {
                                        svg(icon!("rotate-ccw"))
                                            .size(px(13.0))
                                            .flex_shrink_0()
                                            .text_color(theme::accent_yellow())
                                    } else {
                                        svg(icon!("corner-down-right"))
                                            .size(px(13.0))
                                            .flex_shrink_0()
                                            .text_color(theme::accent_success())
                                    }
                                },
                            ))
                    },
                )),
        )
        .child(when(
            move || !attachments.with(|items| items.is_empty()),
            move || message_attachments(attachments, on_open_attachment.clone()),
        ))
        .child(
            view()
                .flex_row()
                .items_center()
                .mr(px(-6.0))
                .child(when(
                    move || sequence.get() > 0 && editable_sequence.get() == sequence.get(),
                    move || {
                        let on_edit = on_edit.clone();
                        row_action(
                            icon!("pencil"),
                            copy::text("Edit", "编辑", "編輯"),
                            move || {
                                on_edit(sequence.get_untracked(), &body.get_untracked());
                            },
                        )
                    },
                ))
                .child(copy_message(body))
                .child(share_message(body)),
        )
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
                    .gap(px(6.0))
                    .h(px(30.0))
                    .max_w(relative(1.0))
                    .px(px(10.0))
                    .rounded_full()
                    .bg(theme::fill)
                    .active(|style| style.opacity(0.6))
                    .child(
                        svg(icon!("paperclip"))
                            .size(px(13.0))
                            .flex_shrink_0()
                            .text_color(theme::text_muted),
                    )
                    .child(
                        text(move || item.with(|item| item.label.clone()))
                            .min_w(px(0.0))
                            .flex_shrink(1.0)
                            .truncate()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_secondary),
                    )
            },
        ))
}

#[allow(clippy::too_many_arguments)]
fn process_header_row(
    started_at_ms: Memo<i64>,
    ended_at_ms: Memo<i64>,
    complete: Memo<bool>,
    expanded: Memo<bool>,
    expandable: Memo<bool>,
    row_id: Memo<String>,
    agent: Memo<String>,
    rail: Rail,
    clock: crate::ui::activity::ActivityClock,
    on_toggle: Rc<dyn Fn(&str)>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    let title = computed(move || {
        let label = if complete.get() {
            copy::text("Worked for", "工作了", "工作了")
        } else {
            copy::text("Working for", "工作了", "工作了")
        };
        format!(
            "{label} {}",
            work_duration(started_at_ms.get(), ended_at_ms.get(), complete.get())
        )
    });
    let running = computed(move || !complete.get());
    // The mark is the Agent that ran the turn, as the desktop's header leads
    // with it: the fold is a summary of that Agent's work, so it says whose.
    view().w_full().pt(px(theme::SPACING_XS)).child(step(
        rail,
        // The mark reads the Agent when the row is built and again when the
        // row's Agent changes, so a turn that settles onto a named Agent draws
        // its logo rather than the default one it started with.
        //
        // `agent_icon` hands back the SVG's own markup, so it goes through
        // `icon` (`ImageSrc::svg_markup`) the way every other Agent mark does.
        // Handed to `svg` as a bare `&str` it was read as a URL, and the
        // header drew the failed-image placeholder instead of a logo.
        dynamic(move || {
            svg(icon(agent_icon(&agent.get())))
                .size(px(15.0))
                .text_color(theme::text_muted)
        }),
        step_line(
            title,
            running,
            expanded,
            expandable,
            clock,
            query,
            active,
            move || on_toggle(&row_id.get_untracked()),
        ),
        view(),
    ))
}

fn disclosure_chevron(expanded: Memo<bool>) -> impl IntoElement {
    dynamic(move || {
        svg(if expanded.get() {
            icon!("chevron-down")
        } else {
            icon!("chevron-right")
        })
        .size(px(14.0))
        .flex_shrink_0()
        .text_color(theme::text_muted)
    })
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

/// What the Agent answered: its name over the prose, and under it what the
/// reader can do with the answer.
#[allow(clippy::too_many_arguments)]
fn agent_prose(
    _title: Memo<String>,
    body: Memo<String>,
    streaming: Memo<bool>,
    sequence: Memo<i64>,
    prompt: Memo<Option<i64>>,
    jump: Signal<Option<i64>>,
    conclusion: Memo<bool>,
    on_fork: Rc<dyn Fn(i64)>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    let images = computed(move || markdown_images(&body.get()));
    view()
        .w_full()
        .flex_col()
        .gap(px(6.0))
        // Prose and the tool rows around it read as one run otherwise. The
        // step rows keep their own compact rhythm, so the room that separates
        // a paragraph from them belongs here.
        .pt(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_SM))
        // The prose stands on its own. The Agent that wrote it is named by the
        // turn's fold above, so a mark and a name over every paragraph only
        // pushed the text the reader came for further down the page — and a
        // ring there, over a paragraph that is already arriving, only pushed
        // it further. The fold's header says the turn is still working.
        .child(find::marked_markdown_view(
            body,
            query,
            active,
            streaming,
            |view| view.selectable(true),
        ))
        .child(when(
            move || images.with(|images| !images.is_empty()),
            move || {
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(theme::SPACING_SM))
                    .child(each(
                        move || images.get(),
                        |source| source.clone(),
                        move |source, _| {
                            image(move || ImageSrc::from(source.get_untracked()))
                                .w_full()
                                .max_h(px(320.0))
                                .rounded(px(theme::RADIUS_CHIP))
                                .object_fit(nsh_core::ResizeMode::Contain)
                        },
                    ))
            },
        ))
        .child(when(
            move || conclusion.get() && !streaming.get() && !body.with(String::is_empty),
            move || {
                let fork = on_fork.clone();
                view()
                    .flex_row()
                    .items_center()
                    .ml(px(-8.0))
                    .child(copy_message(body))
                    .child(share_message(body))
                    .child(when(
                        move || prompt.with(Option::is_some),
                        move || {
                            row_action(
                                icon!("arrow-to-top"),
                                copy::text(
                                    "Scroll to user message",
                                    "滚动到用户消息",
                                    "捲動到使用者訊息",
                                ),
                                move || jump.set(prompt.get_untracked()),
                            )
                        },
                    ))
                    .child(row_action(
                        icon!("git-branch"),
                        copy::text("Fork", "分叉", "分叉"),
                        move || fork(sequence.get_untracked()),
                    ))
            },
        ))
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

/// One of the quiet controls under a message.
fn row_action(icon: ImageSrc, label: &'static str, action: impl Fn() + 'static) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(action)
        .size(px(theme::CHIP_HEIGHT))
        .flex_shrink_0()
        .rounded_full()
        .items_center()
        .justify_center()
        .hit_slop(px(6.0))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.4))
        .child(svg(icon).size(px(15.0)).text_color(theme::text_muted))
}

fn copy_message(body: Memo<String>) -> ButtonElement {
    row_action(
        icon!("copy"),
        copy::text("Copy", "复制", "複製"),
        move || {
            crate::motion::tick();
            // 0.7 answers whether the clipboard took it; this row has no use
            // for the answer, so it is dropped.
            let _ = set_clipboard(body.get_untracked());
        },
    )
    .disabled(move || body.with(String::is_empty))
}

fn share_message(body: Memo<String>) -> ButtonElement {
    row_action(
        icon!("upload"),
        copy::text("Share", "分享", "分享"),
        move || {
            let text = body.get_untracked();
            spawn(async move {
                let _ = share(nsh_core::share::ShareRequest::new().text(text)).await;
            });
        },
    )
    .disabled(move || body.with(String::is_empty))
}

#[allow(clippy::too_many_arguments)]
fn reasoning_row(
    body: Memo<String>,
    streaming: Memo<bool>,
    expanded: Memo<bool>,
    expandable: Memo<bool>,
    rail: Rail,
    clock: crate::ui::activity::ActivityClock,
    toggle: Rc<dyn Fn()>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    let title = computed(move || copy::text("Thought", "思考", "思考").to_owned());
    step(
        rail,
        step_node(icon!("brain"), move || {
            if streaming.get() {
                theme::accent_blue()
            } else {
                theme::text_muted()
            }
        }),
        step_line(
            title,
            streaming,
            expanded,
            expandable,
            clock,
            query,
            active,
            move || toggle(),
        ),
        when(
            move || expanded.get(),
            move || {
                view()
                    .w_full()
                    .min_w(px(0.0))
                    .pb(px(theme::SPACING_SM))
                    .style_when(
                        move || streaming.get(),
                        |style| {
                            style
                                .max_h(theme::line_height_for(theme::font_detail()) * 6.0)
                                .overflow_hidden()
                                .justify_end()
                        },
                    )
                    .child(find::marked_markdown_view(
                        body,
                        query,
                        active,
                        streaming,
                        |view| {
                            view.flex_shrink_0()
                                .selectable(true)
                                .text_size(px(theme::font_detail()))
                                .text_color(theme::text_muted)
                        },
                    ))
            },
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn activity_row(
    title: Memo<String>,
    body: Memo<String>,
    entry: Memo<Option<TimelineEntry>>,
    running: Memo<bool>,
    expanded: Memo<bool>,
    expandable: Memo<bool>,
    rail: Rail,
    on_toggle: Rc<dyn Fn()>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    // A row filed under a group steps in from the rows of the fold itself.
    let grouped = computed(move || {
        entry.with(|entry| {
            entry
                .as_ref()
                .and_then(|entry| entry.parent_id.as_deref())
                .is_some_and(|id| id.starts_with("activity-group:"))
        })
    });
    let activity = computed(move || {
        entry.with(|entry| entry.as_ref().and_then(|entry| entry.activity.clone()))
    });
    let failed = computed(move || {
        activity.with(|value| value.as_ref().is_some_and(|activity| activity.failed))
    });
    let target = computed(move || {
        activity.with(|value| {
            value
                .as_ref()
                .map(|activity| activity.target.clone())
                .unwrap_or_default()
        })
    });
    let label = move || {
        let mut label = title.get();
        activity.with(|value| {
            if let Some(activity) = value.as_ref() {
                let target = activity.path.as_deref().unwrap_or(&activity.target);
                if !target.is_empty() {
                    label.push(' ');
                    label.push_str(target);
                }
            }
        });
        SharedString::from(label)
    };
    let ink = move || {
        if failed.get() {
            theme::accent_red()
        } else if running.get() {
            theme::accent_blue()
        } else {
            theme::text_muted()
        }
    };
    step(
        rail,
        dynamic(move || {
            svg(activity_glyph_icon(activity.with(|value| {
                value
                    .as_ref()
                    .map(|activity| activity.glyph)
                    .unwrap_or(ActivityGlyph::Tool)
            })))
            .size(px(15.0))
            .text_color(ink)
        }),
        button()
            .w_full()
            .min_h(px(STEP_HEIGHT))
            .flex_row()
            .items_center()
            .gap(px(theme::SPACING_SM))
            .rounded(px(theme::RADIUS_CONTROL))
            .my(px(2.0))
            .style_when(
                move || grouped.get(),
                |style| style.pl(px(theme::SPACING_SM)),
            )
            .accessibility_label(label)
            .active(|style| style.bg(theme::row_pressed_bg()))
            .disabled(move || !expandable.get())
            .on_press(move || on_toggle())
            .child(
                view()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(find::marked_line(title, query, active, |line| {
                        line.min_w(px(0.0))
                            .truncate()
                            .text_size(px(theme::font_detail()))
                            .text_color(theme::text_secondary)
                    }))
                    .child(when(
                        move || !target.with(String::is_empty),
                        move || {
                            view().flex_1().min_w(px(0.0)).child(find::marked_line(
                                target,
                                query,
                                active,
                                |line| {
                                    line.truncate()
                                        .text_size(px(theme::font_detail()))
                                        .text_color(theme::text_muted)
                                },
                            ))
                        },
                    )),
            )
            // An edit's size, in the colours a diff uses: the reader can tell
            // a one-line touch from a rewrite without opening the row.
            .child(when(
                move || {
                    activity.with(|value| {
                        value
                            .as_ref()
                            .is_some_and(|activity| activity.added_lines.is_some())
                    })
                },
                move || {
                    let added = computed(move || {
                        activity.with(|value| {
                            value
                                .as_ref()
                                .and_then(|activity| activity.added_lines)
                                .unwrap_or(0)
                        })
                    });
                    let removed = computed(move || {
                        activity.with(|value| {
                            value
                                .as_ref()
                                .and_then(|activity| activity.removed_lines)
                                .unwrap_or(0)
                        })
                    });
                    view()
                        .flex_row()
                        .items_center()
                        .flex_shrink_0()
                        .gap(px(4.0))
                        .child(
                            text(move || format!("+{}", added.get()))
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::accent_success()),
                        )
                        .child(
                            text(move || format!("-{}", removed.get()))
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::accent_red()),
                        )
                },
            ))
            .child(dynamic(move || {
                if failed.get() {
                    text(copy::text("Failed", "失败", "失敗"))
                        .flex_shrink_0()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::accent_red)
                        .into_element()
                } else {
                    // A running row keeps its last reported duration rather
                    // than a spinner: a turn runs several calls at once, and a
                    // ring on every one of them turned the transcript into a
                    // column of blinking dots. The fold's own header already
                    // says the turn is still working.
                    text(activity.with(|value| {
                        value
                            .as_ref()
                            .and_then(|activity| activity.duration.clone())
                            .unwrap_or_default()
                    }))
                    .flex_shrink_0()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
                    .into_element()
                }
            }))
            .child(when(
                move || expandable.get(),
                move || disclosure_chevron(expanded),
            )),
        view()
            .w_full()
            .min_w(px(0.0))
            .child(when(
                move || failed.get() && !expanded.get(),
                move || {
                    text(move || {
                        activity.with(|value| {
                            value
                                .as_ref()
                                .and_then(|activity| activity.error.clone())
                                .unwrap_or_default()
                        })
                    })
                    .truncate()
                    .pb(px(theme::SPACING_XS))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::accent_red)
                },
            ))
            .child(when(
                move || expanded.get(),
                move || activity_details_view(activity, body, query, active),
            )),
    )
}

fn activity_glyph_icon(glyph: ActivityGlyph) -> ImageSrc {
    match glyph {
        // The desktop's own marks for each kind of work, so a command or an
        // edit reads the same on both clients: a closed folder for a listing,
        // a pencil for an edit, a person for a hand-off, a signal for a retry,
        // and a spark for anything the others do not name.
        ActivityGlyph::Command => icon!("square-terminal"),
        ActivityGlyph::Search => icon!("search"),
        ActivityGlyph::Directory => icon!("folder"),
        ActivityGlyph::Read => icon!("file-text"),
        ActivityGlyph::Edit => icon!("pencil"),
        ActivityGlyph::Create => icon!("file-plus"),
        ActivityGlyph::Delete => icon!("trash-2"),
        ActivityGlyph::Plan => icon!("list-checks"),
        ActivityGlyph::Agent => icon!("user"),
        ActivityGlyph::Image => icon!("image"),
        ActivityGlyph::Integration => icon!("plug-zap"),
        ActivityGlyph::Web => icon!("globe"),
        ActivityGlyph::Retry => icon!("wifi-outlined"),
        ActivityGlyph::Tool => icon!("zap"),
    }
}

fn activity_details_view(
    activity: Memo<Option<ActivityView>>,
    body: Memo<String>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    dynamic(move || {
        let (details_loading, details_pending) = activity.with(|activity| {
            activity.as_ref().map_or((false, false), |activity| {
                (activity.details_loading, activity.details_pending)
            })
        });
        let details = activity
            .with(|activity| activity.as_ref().map(|activity| activity.details.clone()))
            .unwrap_or_else(|| {
                let body = body.get();
                vec![ActivityDetail {
                    label: copy::text("Details", "详情", "詳細資料").to_owned(),
                    text: body.clone(),
                    copy: body,
                }]
            });
        view()
            .w_full()
            .min_w(px(0.0))
            .gap(px(theme::SPACING_SM))
            .pb(px(theme::SPACING_SM))
            .when(details.is_empty(), |body| {
                body.child(
                    text(if details_loading {
                        copy::text("Loading details…", "正在加载详情…", "正在載入詳細資料…")
                    } else if details_pending {
                        copy::text("Details unavailable", "详情不可用", "詳細資料無法使用")
                    } else {
                        copy::text("No details", "没有详情", "沒有詳細資料")
                    })
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::text_muted),
                )
            })
            .children(details.into_iter().map(|detail| {
                let label = copy::text("Copy {label}", "复制{label}", "複製{label}")
                    .replace("{label}", &detail.label);
                let text_of_detail = computed({
                    let text = detail.text.clone();
                    move || text.clone()
                });
                // One block of a step's detail: what it is, the way to take
                // it away, and the text itself in a well.
                view()
                    .w_full()
                    .min_w(px(0.0))
                    .rounded(px(theme::RADIUS_CHIP))
                    .bg(theme::fill)
                    .pl(px(theme::SPACING_MD))
                    .pr(px(theme::SPACING_XS))
                    .pb(px(theme::SPACING_SM))
                    .child(
                        view()
                            .w_full()
                            .flex_row()
                            .items_center()
                            .child(
                                text(detail.label)
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_size(px(theme::font_hint()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_muted),
                            )
                            .child(
                                row_action(
                                    icon!("copy"),
                                    copy::text("Copy detail", "复制详情", "複製詳細資料"),
                                    move || {
                                        crate::motion::tick();
                                        let _ = set_clipboard(detail.copy.clone());
                                    },
                                )
                                .accessibility_label(label),
                            ),
                    )
                    .child(
                        scroll().max_h(px(192.0)).w_full().child(
                            view()
                                .w_full()
                                .pr(px(theme::SPACING_SM))
                                .child(find::marked_line(text_of_detail, query, active, |line| {
                                    line.w_full()
                                        .font_family("monospace")
                                        .selectable(true)
                                        .text_size(px(theme::font_micro()))
                                        .line_height(theme::line_height_for(theme::font_micro()))
                                        .text_color(theme::text_secondary)
                                })),
                        ),
                    )
            }))
    })
}

/// The files a turn changed: a card with a row a file.
fn changes_card(
    title: Memo<String>,
    entry: Memo<Option<TimelineEntry>>,
    reveal: RevealRow,
    open_file: Rc<dyn Fn(&str)>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    const PREVIEW_FILES: usize = 3;
    let files = computed(move || {
        entry.with(|entry| {
            entry
                .as_ref()
                .map(|entry| entry.changed_files.clone())
                .unwrap_or_default()
        })
    });
    let expanded = signal(false);
    let expandable = computed(move || files.with(|files| files.len() > PREVIEW_FILES));
    view().w_full().py(px(theme::SPACING_SM)).child(
        surface::card()
            .child(
                button()
                    .w_full()
                    .min_h(px(theme::ROW_HEIGHT))
                    .px(px(surface::ROW_INSET))
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_MD))
                    .accessibility_label(move || SharedString::from(title.get()))
                    .active(|style| style.bg(theme::row_pressed_bg()))
                    .disabled(move || !expandable.get())
                    .on_press(move || expanded.update(|expanded| *expanded = !*expanded))
                    .child(surface::tile(icon!("file-code")))
                    .child(view().flex_1().min_w(px(0.0)).child(find::marked_line(
                        title,
                        query,
                        active,
                        |line| {
                            line.truncate()
                                .text_size(px(theme::font_detail()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::foreground)
                        },
                    )))
                    .child(when(
                        move || expandable.get(),
                        move || disclosure_chevron(computed(move || expanded.get())),
                    )),
            )
            .child(each(
                move || {
                    files.with(|files| {
                        files
                            .iter()
                            .take(if expanded.get() {
                                files.len()
                            } else {
                                PREVIEW_FILES
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                },
                |file| file.path.clone(),
                move |file, _| {
                    let open_file = open_file.clone();
                    let reveal = reveal.clone();
                    let name = move || {
                        file.with(|file| {
                            file.path
                                .rsplit('/')
                                .next()
                                .unwrap_or(&file.path)
                                .to_owned()
                        })
                    };
                    let folder = move || {
                        file.with(|file| {
                            file.path
                                .rsplit_once('/')
                                .map_or_else(String::new, |(folder, _)| folder.to_owned())
                        })
                    };
                    view()
                        .w_full()
                        .min_w(px(0.0))
                        .flex_row()
                        .items_center()
                        .border_t_1()
                        .border_color(theme::hairline)
                        .pr(px(theme::SPACING_SM))
                        .child(
                            button()
                                .flex_1()
                                .min_w(px(0.0))
                                .min_h(px(theme::TOUCH_TARGET + 2.0))
                                .pl(px(surface::ROW_INSET))
                                .justify_center()
                                .items_start()
                                .accessibility_label(move || {
                                    file.with(|file| SharedString::from(file.path.clone()))
                                })
                                .disabled(move || file.with(|file| file.deleted))
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .child(
                                    text(name)
                                        .w_full()
                                        .truncate()
                                        .text_size(px(theme::font_caption()))
                                        .font_family("monospace")
                                        .text_color(move || {
                                            if file.with(|file| file.deleted) {
                                                theme::text_muted()
                                            } else {
                                                theme::foreground()
                                            }
                                        }),
                                )
                                .child(when(
                                    move || !folder().is_empty(),
                                    move || {
                                        text(folder)
                                            .w_full()
                                            .truncate()
                                            .text_size(px(theme::font_hint()))
                                            .text_color(theme::text_muted)
                                    },
                                ))
                                .on_press(move || {
                                    open_file(&file.with_untracked(|file| file.path.clone()))
                                }),
                        )
                        .child(surface::ghost_button(
                            icon!("chevrons-right-left"),
                            copy::text("View changes", "查看变更", "檢視變更"),
                            move || reveal(&file.with_untracked(|file| file.row_id.clone()), true),
                        ))
                },
            )),
    )
}

/// A question the Agent asked and how it was answered, kept in the record.
fn decision_card(
    title: Memo<String>,
    body: Memo<String>,
    meta: Memo<String>,
    query: Signal<String>,
    active: Memo<bool>,
) -> impl IntoElement {
    view().w_full().py(px(6.0)).child(
        surface::card()
            .px(px(surface::ROW_INSET))
            .py(px(theme::SPACING_MD))
            .gap(px(theme::SPACING_XS))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        text(move || title.get())
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(theme::font_detail()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::foreground),
                    )
                    .child(when(
                        move || !meta.with(String::is_empty),
                        move || {
                            surface::badge(
                                move || SharedString::from(meta.get()),
                                theme::text_secondary,
                            )
                        },
                    )),
            )
            .child(when(
                move || !body.with(String::is_empty),
                move || {
                    find::marked_line(body, query, active, |line| {
                        line.text_size(px(theme::font_caption()))
                            .line_height(theme::line_height_for(theme::font_caption()))
                            .text_color(theme::text_secondary)
                    })
                },
            )),
    )
}

/// A failure: what went wrong, on the tint failures are told by.
fn error_card(body: Memo<String>, query: Signal<String>, active: Memo<bool>) -> impl IntoElement {
    view().w_full().py(px(6.0)).child(
        view()
            .w_full()
            .flex_row()
            .items_start()
            .gap(px(10.0))
            .rounded(px(theme::RADIUS_GROUP))
            .bg(move || theme::tint(theme::accent_red()))
            .px(px(surface::ROW_INSET))
            .py(px(theme::SPACING_MD))
            .child(surface::status_dot(theme::accent_red).mt(px(6.0)))
            .child(view().flex_1().min_w(px(0.0)).child(find::marked_line(
                body,
                query,
                active,
                |line| {
                    line.selectable(true)
                        .text_size(px(theme::font_detail()))
                        .line_height(theme::line_height_for(theme::font_detail()))
                        .text_color(theme::accent_red)
                },
            ))),
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
        .flex_shrink_0()
        .px(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_XS))
        .child(
            surface::card()
                .max_h(px(240.0))
                .shadow_md()
                .child(
                    scroll()
                        .max_h(px(240.0))
                        .child(view().flex_col().child(each(
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
                ),
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
        .min_h(px(theme::TOUCH_TARGET))
        .px(px(surface::ROW_INSET))
        .py(px(theme::SPACING_SM))
        .flex_col()
        .items_start()
        .justify_center()
        .gap(px(1.0))
        .border_b_1()
        .border_b_color(theme::hairline)
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            text(move || command.get().label)
                .truncate()
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::foreground),
        )
        .child(when(
            move || command.with(|command| !command.description.is_empty()),
            move || {
                text(move || command.get().description)
                    .truncate()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
            },
        ))
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
    // Two answers stand side by side, the one that lets the Agent go on at
    // the trailing edge; more than two stack, in the runtime's order.
    let paired = request.options.len() == 2;
    let mut options = request.options;
    if paired {
        options.sort_by_key(|option| option.choice == PermissionChoice::Allow);
    }
    let buttons = options.into_iter().map({
        let on_permission = on_permission.clone();
        let request_id = request_id.clone();
        move |option| {
            let id = request_id.clone();
            let choice = option.choice;
            let on_permission = on_permission.clone();
            let button = if choice == PermissionChoice::Allow {
                action::solid(option.label, move || on_permission(&id, choice))
            } else {
                action::tonal(option.label, move || on_permission(&id, choice))
            };
            let button = if paired {
                button.flex_1().min_w(px(0.0))
            } else {
                button.w_full()
            };
            button.disabled(move || resolving.get())
        }
    });
    view()
        .w_full()
        .flex_shrink_0()
        .px(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_XS))
        .child(
            waiting_card(request.title)
                .children(request.details.into_iter().map(|detail| {
                    text(detail)
                        .text_size(px(theme::font_caption()))
                        .line_height(theme::line_height_for(theme::font_caption()))
                        .text_color(theme::text_secondary)
                }))
                .child(
                    view()
                        .w_full()
                        .gap(px(theme::SPACING_SM))
                        .pt(px(theme::SPACING_XS))
                        .when(paired, |row| row.flex_row())
                        .when(!paired, |column| column.flex_col())
                        .children(buttons),
                ),
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
        let mut column = view().w_full().gap(px(theme::SPACING_XS)).child(
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
                column = column.child(field::text_field_well(value, move || placeholder.clone()));
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
        .flex_shrink_0()
        .px(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_XS))
        .child(
            waiting_card(request.title)
                .child(
                    text(request.message)
                        .text_size(px(theme::font_caption()))
                        .line_height(theme::line_height_for(theme::font_caption()))
                        .text_color(theme::text_secondary),
                )
                // A question can carry more fields than a phone's card can
                // show at once; the fields scroll while the title and the
                // submit stay put.
                .child(
                    scroll().max_h(px(280.0)).child(
                        view()
                            .w_full()
                            .flex_col()
                            .gap(px(theme::SPACING_MD))
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
                            action::tonal(copy::text("Decline", "拒绝", "拒絕"), decline)
                                .flex_1()
                                .min_w(px(0.0))
                                .disabled(move || resolving.get()),
                        )
                        .child(
                            action::solid(copy::text("Submit", "提交", "提交"), submit)
                                .flex_1()
                                .min_w(px(0.0))
                                .disabled(move || resolving.get()),
                        ),
                ),
        )
}

/// One selectable row of a choice field.
fn choice_button(label: String, value: String, selected: Signal<Option<String>>) -> ButtonElement {
    let pressed = value.clone();
    let chosen = computed(move || selected.with(|selected| selected.as_deref() == Some(&value)));
    option_row(
        label,
        move || chosen.get(),
        move || selected.set(Some(pressed.clone())),
    )
    .w_full()
}

/// One toggle row of a multi-select field.
fn multi_button(label: String, value: String, chosen: Signal<Vec<String>>) -> ButtonElement {
    let pressed = value.clone();
    let ticked = computed(move || chosen.with(|chosen| chosen.contains(&value)));
    option_row(
        label,
        move || ticked.get(),
        move || {
            chosen.update(|chosen| {
                if let Some(index) = chosen.iter().position(|item| item == &pressed) {
                    chosen.remove(index);
                } else {
                    chosen.push(pressed.clone());
                }
            });
        },
    )
    .w_full()
}

/// One side of a yes/no field.
fn boolean_button(label: String, value: bool, selected: Signal<Option<bool>>) -> ButtonElement {
    option_row(
        label,
        move || selected.with(|selected| *selected == Some(value)),
        move || selected.set(Some(value)),
    )
    .flex_1()
    .min_w(px(0.0))
}

/// The card the Agent waits on the reader in: what it needs, said once, and
/// the answers under it. It stands over the composer rather than in the
/// transcript, because nothing moves until it is answered.
fn waiting_card(title: String) -> View {
    surface::card()
        .px(px(surface::ROW_INSET))
        .py(px(surface::ROW_INSET))
        .gap(px(theme::SPACING_SM))
        .shadow_md()
        .child(
            view()
                .w_full()
                .flex_row()
                .items_start()
                .gap(px(theme::SPACING_SM))
                .child(surface::status_dot(theme::accent_yellow).mt(px(7.0)))
                .child(
                    text(title)
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(theme::font_body()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                ),
        )
}

/// One answer among several: a line on a wash, marked once it is chosen.
fn option_row(
    label: String,
    selected: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_press)
        .min_h(px(theme::TOUCH_TARGET))
        .px(px(surface::ROW_INSET))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .rounded(px(theme::RADIUS_BUTTON))
        .bg(move || {
            if selected() {
                theme::fill_strong()
            } else {
                theme::fill()
            }
        })
        .active(|style| style.opacity(0.7))
        .child(
            text(label)
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(theme::font_detail()))
                .text_color(theme::foreground),
        )
        .child(surface::check_mark(move || selected().then_some(true)))
}

/// A line above the prompt card that says how the next message stands, with
/// the one thing to do about it.
fn composer_notice(
    message: &'static str,
    action_label: &'static str,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .w_full()
        .min_h(px(theme::ROUND_CONTROL))
        .pl(px(surface::ROW_INSET))
        .pr(px(theme::SPACING_XS))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_BUTTON))
        .bg(theme::fill)
        .child(
            text(message)
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
        .child(action::link(action_label, on_press))
}

/// What can be added to the message being written: something attached to it,
/// or an hour to hold it until.
fn composer_add_sheet(
    open: Signal<bool>,
    photo: Action,
    camera: Action,
    file: Action,
    can_schedule: impl Fn() -> bool + 'static,
    on_schedule: Action,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
    let attach =
        move |icon: ImageSrc, label: &'static str, detail: &'static str, action: Action| {
            surface::row_captioned(icon, label, detail, move || {
                open.set(false);
                action();
            })
            .accessibility_label(label)
        };
    bottom_sheet(
        move || open.get(),
        move || open.set(false),
        view()
            .w_full()
            .child(sheet_header(
                copy::text("Add to message", "添加到消息", "加入訊息"),
                move || open.set(false),
            ))
            .child(
                sheet_body()
                    .child(
                        sheet_card()
                            .child(attach(
                                icon!("image"),
                                copy::text("Photo", "图片", "圖片"),
                                copy::text(
                                    "Scaled down before it is sent",
                                    "发送前会先缩放",
                                    "傳送前會先縮放",
                                ),
                                photo,
                            ))
                            .child(surface::row_divider())
                            .child(attach(
                                icon!("file-video-camera"),
                                copy::text("Camera", "拍照", "拍照"),
                                copy::text(
                                    "Take a photograph now",
                                    "现在拍一张照片",
                                    "現在拍一張照片",
                                ),
                                camera,
                            ))
                            .child(surface::row_divider())
                            .child(attach(
                                icon!("file-plus"),
                                copy::text("File", "文件", "檔案"),
                                copy::text(
                                    "Sent as it is, up to 8 MB",
                                    "原样发送，最大 8 MB",
                                    "原樣傳送，最大 8 MB",
                                ),
                                file,
                            )),
                    )
                    .child(
                        sheet_card().child(
                            surface::row_captioned(
                                icon!("clock"),
                                copy::text("Send later", "稍后发送", "稍後傳送"),
                                copy::text(
                                    "Hold what is written until a later hour",
                                    "把已写好的消息留到稍后再发",
                                    "把已寫好的訊息留到稍後再傳",
                                ),
                                move || {
                                    open.set(false);
                                    on_schedule();
                                },
                            )
                            .accessibility_label(copy::text(
                                "Schedule message",
                                "定时发送",
                                "定時傳送",
                            ))
                            .disabled(move || !can_schedule())
                            .child(surface::chevron()),
                        ),
                    ),
            ),
    )
}

/// The composer, kept above the software keyboard.
fn composer(
    host: SessionHost,
    composer_input: ElementRef,
    runtime_panel: Signal<RuntimePanel>,
) -> impl IntoElement {
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
    let runtime_busy = host.runtime_busy;
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
    let on_open_runtime: Rc<dyn Fn(RuntimePanel)> = {
        let open = host.on_open_runtime.clone();
        Rc::new(move |panel| {
            dismiss_keyboard();
            runtime_panel.set(panel);
            open();
        })
    };
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
    let add_open = signal(false);
    let has_content = computed(move || {
        !draft.with(|draft| draft.trim().is_empty()) || !attachments.with(Vec::is_empty)
    });
    let can_send = move || !sending.get() && active.with(Option::is_some) && has_content.get();
    // The notices, the cards and the prompt card stand in one column. The
    // sheets the card raises are kept out of it: they take no room, and a gap
    // beside one would still be a gap.
    let column = view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_SM))
        .child(when(move || needs_continue.get(), {
            let on_continue_turn = on_continue_turn.clone();
            move || {
                let on_continue_turn = on_continue_turn.clone();
                composer_notice(
                    copy::text(
                        "The last turn was paused.",
                        "上一回合已暂停。",
                        "上一回合已暫停。",
                    ),
                    copy::text("Continue", "继续", "繼續"),
                    move || on_continue_turn(),
                )
            }
        }))
        .child(when(move || editing.get(), {
            let on_cancel_edit = on_cancel_edit.clone();
            move || {
                let on_cancel_edit = on_cancel_edit.clone();
                composer_notice(
                    copy::text(
                        "Replacing the last message",
                        "正在替换上一条消息",
                        "正在取代上一則訊息",
                    ),
                    copy::text("Cancel", "取消", "取消"),
                    move || on_cancel_edit(),
                )
            }
        }))
        .child(when(move || scheduled.get(), {
            let on_cancel_schedule = on_cancel_schedule.clone();
            move || {
                let on_cancel_schedule = on_cancel_schedule.clone();
                composer_notice(
                    copy::text("Message scheduled", "消息已定时", "訊息已定時"),
                    copy::text("Cancel", "取消", "取消"),
                    move || on_cancel_schedule(),
                )
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
        .child(
            compose::card()
                .child(when(
                    move || !attachments.with(Vec::is_empty),
                    move || {
                        let on_remove = on_remove_attachment.clone();
                        compose::chips().child(each(
                            move || attachments.get(),
                            |item| item.id.clone(),
                            move |item, _| {
                                let on_remove = on_remove.clone();
                                compose::chip(
                                    move || {
                                        SharedString::from(item.with(|item| item.label.clone()))
                                    },
                                    move || on_remove(&item.with_untracked(|item| item.id.clone())),
                                )
                            },
                        ))
                    },
                ))
                .child(
                    compose::field(draft, compose::placeholder(), move |text: &str| {
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
                    })
                    .element_ref(&composer_input),
                )
                .child(
                    compose::controls()
                        .child(
                            compose::add_button(
                                copy::text("Add to message", "添加到消息", "加入訊息"),
                                move || {
                                    dismiss_keyboard();
                                    add_open.set(true);
                                },
                            )
                            .disabled(move || sending.get() || active.with(Option::is_none)),
                        )
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex_row()
                                .items_center()
                                .overflow_hidden()
                                .child(runtime_trigger(
                                    runtime_summary,
                                    move || sending.get() || runtime_busy.get(),
                                    move || on_open_runtime(RuntimePanel::Model),
                                )),
                        )
                        .child(when(
                            move || show_token_usage.get(),
                            move || token_chip(token_usage),
                        ))
                        .child(dynamic(move || {
                            if running.get() && !has_content.get() {
                                let stop = on_interrupt.clone();
                                compose::send_button(
                                    icon!("stop"),
                                    copy::text("Stop", "停止", "停止"),
                                    || true,
                                    move || stop(),
                                )
                            } else {
                                let submit = submit.clone();
                                compose::send_button(
                                    icon!("send"),
                                    if editing.get() {
                                        copy::text("Replace message", "替换消息", "取代訊息")
                                    } else if running.get() {
                                        copy::text(
                                            "Queue for the turn",
                                            "排入队列，回合结束后发送",
                                            "排入佇列，回合結束後傳送",
                                        )
                                    } else {
                                        copy::text("Send", "发送", "傳送")
                                    },
                                    can_send,
                                    move || submit(),
                                )
                            }
                        })),
                ),
        );
    view()
        .w_full()
        .flex_shrink_0()
        .flex_col()
        .px(px(theme::SPACING_MD))
        .pt(px(theme::SPACING_XS))
        .pb_keyboard_plus(px(theme::SPACING_SM))
        .child(column)
        .child(composer_add_sheet(
            add_open,
            host.on_attach_image,
            host.on_attach_camera,
            host.on_attach_file,
            move || can_send() && !editing.get(),
            on_open_schedule,
        ))
        .child(delay_sheet(
            move || schedule_open.get(),
            {
                let close = on_close_schedule.clone();
                move || close()
            },
            move |seconds| on_schedule(seconds),
        ))
        // `pb_keyboard` covers the keyboard while it is up; with it down the
        // composer still has to clear the home indicator.
        .child(when(
            move || keyboard_height().0 == 0.0,
            || view().h(safe_area().bottom),
        ))
}

/// The prompts written ahead of the running turn.
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
    surface::card()
        .child(
            view()
                .w_full()
                .min_h(px(theme::TOUCH_TARGET))
                .pl(px(surface::ROW_INSET))
                .pr(px(theme::SPACING_XS))
                .flex_row()
                .items_center()
                .child(
                    text(move || {
                        format!(
                            "{} · {}",
                            copy::text("Queued", "队列", "佇列"),
                            queued.with(|queued| queued.len())
                        )
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(theme::font_caption()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_secondary),
                )
                .child(action::link(
                    move || {
                        SharedString::from(if manual.get() {
                            copy::text("Auto", "自动", "自動")
                        } else {
                            copy::text("Manual", "手动", "手動")
                        })
                    },
                    move || on_toggle_manual(),
                ))
                .child(action::link(
                    move || {
                        SharedString::from(if paused.get() {
                            copy::text("Resume", "继续", "繼續")
                        } else {
                            copy::text("Pause", "暂停", "暫停")
                        })
                    },
                    move || on_toggle_paused(),
                ))
                .child(action::link(
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
                    .min_h(px(theme::TOUCH_TARGET))
                    .pl(px(surface::ROW_INSET))
                    .pr(px(theme::SPACING_XS))
                    .flex_row()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::hairline)
                    .style_when(move || dragging.get(), |style| style.bg(theme::fill()))
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
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(theme::font_caption()))
                        .text_color(theme::foreground),
                    )
                    .child({
                        let on_move = on_move.clone();
                        row_action(
                            icon!("chevron-up"),
                            copy::text("Move up", "上移", "上移"),
                            move || on_move(id, -1),
                        )
                    })
                    .child({
                        let on_move = on_move.clone();
                        row_action(
                            icon!("chevron-down"),
                            copy::text("Move down", "下移", "下移"),
                            move || on_move(id, 1),
                        )
                    })
                    .child(row_action(
                        icon!("send"),
                        copy::text("Send now", "立即发送", "立即傳送"),
                        move || send(id),
                    ))
                    .child(row_action(
                        icon!("x"),
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
    surface::card()
        .pl(px(surface::ROW_INSET))
        .pr(px(theme::SPACING_XS))
        .py(px(theme::SPACING_SM))
        .gap(px(2.0))
        .child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    svg(icon!("crosshair"))
                        .size(px(theme::ICON_SM))
                        .flex_shrink_0()
                        .text_color(theme::text_muted),
                )
                .child(
                    text(move || {
                        goal.with(|goal| {
                            goal.as_ref()
                                .map(|goal| goal.objective.clone())
                                .unwrap_or_default()
                        })
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(theme::font_detail()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::foreground),
                )
                .child(
                    text(move || {
                        goal.with(|goal| goal.as_ref().map(goal_phase_label).unwrap_or_default())
                    })
                    .flex_shrink_0()
                    .pr(px(theme::SPACING_SM))
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(move || {
                        goal.with(|goal| {
                            goal.as_ref()
                                .map(goal_phase_color)
                                .unwrap_or_else(theme::text_muted)
                        })
                    }),
                ),
        )
        .child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .child(
                    text(move || {
                        goal.with(|goal| {
                            goal.as_ref()
                                .and_then(|goal| goal.progress.clone())
                                .unwrap_or_default()
                        })
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
                )
                .child(when(
                    move || goal.with(|goal| goal.as_ref().is_some_and(|goal| goal.can_pause)),
                    {
                        let pause = pause.clone();
                        move || {
                            let pause = pause.clone();
                            action::link(copy::text("Pause", "暂停", "暫停"), move || {
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
                            action::link(copy::text("Resume", "继续", "繼續"), move || {
                                resume(GoalControl::Resume)
                            })
                        }
                    },
                ))
                .child(when(
                    move || goal.with(|goal| goal.as_ref().is_some_and(|goal| goal.can_clear)),
                    {
                        let clear = clear.clone();
                        move || {
                            let clear = clear.clone();
                            action::link(copy::text("Clear", "清除", "清除"), move || {
                                clear(GoalControl::Clear)
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
    let expanded = computed(move || open.get());
    surface::card()
        .child(
            button()
                .w_full()
                .min_h(px(theme::ROW_HEIGHT))
                .pl(px(surface::ROW_INSET))
                .pr(px(theme::SPACING_MD))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .accessibility_label(copy::text("Steps", "步骤", "步驟"))
                .active(|style| style.bg(theme::row_pressed_bg()))
                .on_press(move || open.update(|open| *open = !*open))
                .child(
                    svg(icon!("list-checks"))
                        .size(px(theme::ICON_SM))
                        .flex_shrink_0()
                        .text_color(theme::text_muted),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex_col()
                        .child(
                            text(move || {
                                plan.with(|plan| {
                                    plan.as_ref()
                                        .map(|plan| plan.title.clone())
                                        .filter(|title| !title.trim().is_empty())
                                        .unwrap_or_else(|| {
                                            copy::text("Current plan", "当前计划", "目前計劃")
                                                .to_owned()
                                        })
                                })
                            })
                            .truncate()
                            .text_size(px(theme::font_detail()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::foreground),
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
                        ),
                )
                .child(
                    text(move || {
                        plan.with(|plan| {
                            plan.as_ref()
                                .map(|plan| format!("{}/{}", plan.completed, plan.steps.len()))
                                .unwrap_or_default()
                        })
                    })
                    .flex_shrink_0()
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_muted),
                )
                .child(disclosure_chevron(expanded)),
        )
        .child(when(
            move || open.get(),
            move || {
                view()
                    .w_full()
                    .flex_col()
                    .px(px(surface::ROW_INSET))
                    .py(px(theme::SPACING_SM))
                    .gap(px(theme::SPACING_SM))
                    .border_t_1()
                    .border_color(theme::hairline)
                    .child(each(
                        move || {
                            plan.with(|plan| {
                                plan.as_ref()
                                    .map(|plan| plan.steps.clone())
                                    .unwrap_or_default()
                            })
                        },
                        |step| step.title.clone(),
                        move |step, _| {
                            let ink = move || match step.with(|step| step.state) {
                                PlanStepState::Completed => theme::accent_success(),
                                PlanStepState::Running => theme::accent_blue(),
                                PlanStepState::Failed => theme::accent_red(),
                                PlanStepState::Pending => theme::text_muted().opacity(0.5),
                            };
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_SM))
                                .child(surface::status_dot(ink))
                                .child(
                                    text(move || step.with(|step| step.title.clone()))
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(theme::font_caption()))
                                        .text_color(move || {
                                            if step.with(|step| step.state)
                                                == PlanStepState::Pending
                                            {
                                                theme::text_muted()
                                            } else {
                                                theme::text_secondary()
                                            }
                                        }),
                                )
                        },
                    ))
            },
        ))
}

/// The panels of the runtime sheet: the model, and the Agent's own options.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimePanel {
    Model,
    Reasoning,
    Mode,
    Options,
}

impl RuntimePanel {
    /// The panel's name on the sheet's segmented control.
    fn label(self) -> &'static str {
        match self {
            Self::Model => copy::text("Model", "模型", "模型"),
            Self::Reasoning => copy::text("Reasoning", "推理", "推理"),
            Self::Mode => copy::text("Mode", "模式", "模式"),
            Self::Options => copy::text("Options", "选项", "選項"),
        }
    }
}

fn token_chip(usage: Signal<Option<TokenUsageView>>) -> impl IntoElement {
    let label = copy::text("Token usage", "Token 使用情况", "Token 使用情況");
    button()
        .accessibility_label(label)
        .on_press(move || {
            let usage = usage.get_untracked();
            dismiss_keyboard();
            spawn(async move {
                let body = token_usage_detail(usage.as_ref());
                let _ = nsh_base::alert_dialog()
                    .title(label)
                    .description(body)
                    .show()
                    .await;
            });
        })
        .size(px(theme::COMPOSER_OPTION_SIZE))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            canvas(move || {
                use std::f32::consts::{FRAC_PI_2, TAU};
                let percent = usage
                    .with(|usage| usage.as_ref().and_then(|usage| usage.percent))
                    .unwrap_or(0)
                    .min(100);
                let color = match percent {
                    91.. => theme::accent_red(),
                    70..=90 => theme::accent_yellow(),
                    _ => theme::text_muted(),
                };
                let ring = Painter::new()
                    .line_width(2.0)
                    .stroke_color(theme::text_muted().opacity(0.3))
                    .arc(10.0, 10.0, 8.0, 0.0, TAU)
                    .stroke();
                if percent == 0 {
                    ring
                } else {
                    ring.begin_path()
                        .stroke_color(color)
                        .arc(
                            10.0,
                            10.0,
                            8.0,
                            -FRAC_PI_2,
                            TAU * f32::from(percent) / 100.0 - FRAC_PI_2,
                        )
                        .stroke()
                }
            })
            .size(px(20.0)),
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

/// The prompts already sent, one tap from the composer.
fn history_sheet(
    host: SessionHost,
    jump: Signal<Option<i64>>,
    composer_input: ElementRef,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
    let open = host.history_open;
    let history = host.history;
    let limit = signal(12usize);
    let close = host.on_close_history;
    let recall = host.on_recall_history;
    effect(move || {
        if !open.get() {
            limit.set(12);
        }
    });
    bottom_sheet(
        move || open.get(),
        {
            let close = close.clone();
            move || close()
        },
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.72))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Earlier messages", "历史消息", "歷史訊息"),
                {
                    let close = close.clone();
                    move || close()
                },
            ))
            .child(
                scroll().min_h(px(0.0)).flex_shrink(1.0).child(
                    sheet_body()
                        .child(sheet_card().child(each(
                            move || {
                                history.with(|history| {
                                    history
                                        .iter()
                                        .take(limit.get())
                                        .cloned()
                                        .collect::<Vec<_>>()
                                })
                            },
                            |entry| entry.id.clone(),
                            move |entry, _| {
                                let close = close.clone();
                                let recall = recall.clone();
                                view()
                                    .w_full()
                                    .flex_row()
                                    .items_center()
                                    .pr(px(theme::SPACING_SM))
                                    .border_b_1()
                                    .border_b_color(theme::hairline)
                                    .child(
                                        button()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .min_h(px(theme::ROW_HEIGHT))
                                            .pl(px(surface::ROW_INSET))
                                            .pr(px(theme::SPACING_SM))
                                            .py(px(theme::SPACING_SM))
                                            .items_start()
                                            .justify_center()
                                            .active(|style| style.bg(theme::row_pressed_bg()))
                                            .on_press(move || {
                                                close();
                                                jump.set(Some(entry.with(|entry| entry.sequence)));
                                            })
                                            .child(
                                                text(move || {
                                                    entry.with(|entry| entry.text.clone())
                                                })
                                                .number_of_lines(2u32)
                                                .text_size(px(theme::font_detail()))
                                                .line_height(theme::line_height_for(
                                                    theme::font_detail(),
                                                ))
                                                .text_color(theme::foreground),
                                            ),
                                    )
                                    .child(surface::ghost_button(
                                        icon!("text-cursor-input"),
                                        copy::text("Use in composer", "带入输入框", "帶入輸入框"),
                                        move || {
                                            recall(&entry.with(|entry| entry.text.clone()));
                                            set_timeout(
                                                std::time::Duration::from_millis(220),
                                                move || composer_input.focus(),
                                            );
                                        },
                                    ))
                            },
                        )))
                        .child(when(
                            move || history.with(Vec::len) > limit.get(),
                            move || {
                                view().w_full().items_center().child(action::link(
                                    copy::text("Show older", "显示更早", "顯示更早"),
                                    move || limit.update(|limit| *limit += 12),
                                ))
                            },
                        )),
                ),
            ),
    )
}

/// The runtime sheet: what runs the session, and how. One sheet with a panel
/// a kind of choice: the model, and the Agent's own option groups.
pub(crate) fn runtime_sheet(
    host: RuntimePickerHost,
    panel: Signal<RuntimePanel>,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_card, sheet_header};
    let open = host.open;
    let close = host.on_close;
    let query = signal(String::new());
    let agent = signal(None::<String>);
    let options = host.options;
    let busy = host.busy;
    let reasoning = host.reasoning;
    let modes = host.modes;
    let features = host.features;
    effect(move || {
        if !open.get() {
            query.set(String::new());
            agent.set(None);
            panel.set(RuntimePanel::Model);
        }
    });
    // The panels the runtime has something to offer on.
    let panels = computed(move || {
        let mut panels = vec![RuntimePanel::Model];
        if !reasoning.with(Vec::is_empty) {
            panels.push(RuntimePanel::Reasoning);
        }
        if !modes.with(Vec::is_empty) {
            panels.push(RuntimePanel::Mode);
        }
        if !features.with(Vec::is_empty) {
            panels.push(RuntimePanel::Options);
        }
        panels
    });
    effect(move || {
        let current = panel.get_untracked();
        if !panels.with(|panels| panels.contains(&current)) {
            panel.set(RuntimePanel::Model);
        }
    });
    let agents = computed(move || {
        options.with(|options| {
            let mut seen = std::collections::HashSet::new();
            options
                .iter()
                .filter(|option| seen.insert(&option.agent_id))
                .map(|option| (option.agent_id.clone(), option.agent.clone()))
                .collect::<Vec<_>>()
        })
    });
    let active_agent = computed(move || {
        agent.get().or_else(|| {
            options.with(|options| {
                options
                    .iter()
                    .find(|option| option.selected)
                    .or_else(|| options.iter().find(|option| option.available))
                    .or_else(|| options.first())
                    .map(|option| option.agent_id.clone())
            })
        })
    });
    let filtered = computed(move || {
        let search = query.get().trim().to_lowercase();
        let agent = active_agent.get();
        options.with(|options| {
            options
                .iter()
                .filter(|option| {
                    agent.as_ref() == Some(&option.agent_id)
                        && (search.is_empty()
                            || [
                                &option.model,
                                &option.model_id,
                                &option.source_label,
                                &option.detail,
                            ]
                            .iter()
                            .any(|text| text.to_lowercase().contains(&search)))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    });

    // One fold per source, in the order the sources first appear: a reader
    // choosing a model is choosing where it comes from too, and a flat list of
    // every model says nothing about that.
    let grouped = computed(move || {
        let mut groups: Vec<(String, String, Vec<RuntimeOptionView>)> = Vec::new();
        for option in filtered.get() {
            match groups
                .iter_mut()
                .find(|(key, _, _)| *key == option.source_key)
            {
                Some((_, _, options)) => options.push(option),
                None => {
                    // The provider, account or route's own name: a fold is
                    // titled by where its models come from, never by the Agent.
                    let label = if !option.source_label.is_empty() {
                        option.source_label.clone()
                    } else if !option.detail.is_empty() {
                        option.detail.clone()
                    } else {
                        option.agent.clone()
                    };
                    groups.push((option.source_key.clone(), label, vec![option]));
                }
            }
        }
        groups
    });
    // A fold starts closed unless it holds the current choice: a reader who
    // opens the picker to see what they are on should not have to open a fold
    // to find it, and every other source starts out of the way. While searching,
    // every matching fold opens so matches are immediately visible.
    let collapsed = signal(Vec::<String>::new());
    effect(move || {
        // Recomputed whenever the visible set changes — a new Agent, a new
        // search — because the folds that are worth opening change with it.
        let searching = !query.get().trim().is_empty();
        let groups = grouped.get();
        collapsed.set(if searching {
            Vec::new()
        } else {
            groups
                .iter()
                .filter(|(_, _, options)| !options.iter().any(|option| option.selected))
                .map(|(key, _, _)| key.clone())
                .collect()
        });
    });
    let finish_reasoning = close.clone();
    let finish_mode = close.clone();
    let finish_options = close.clone();
    bottom_sheet(
        move || open.get(),
        {
            let close = close.clone();
            move || close()
        },
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.78))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Agent settings", "Agent 设置", "Agent 設定"),
                move || {
                    dismiss_keyboard();
                    close();
                },
            ))
            .child(when(
                move || panels.with(|panels| panels.len() > 1),
                move || {
                    view()
                        .w_full()
                        .flex_shrink_0()
                        .px(px(theme::SPACING_MD))
                        .pb(px(theme::SPACING_MD))
                        .child(dynamic(move || {
                            let mut track = surface::segment_track();
                            for candidate in panels.get() {
                                track = track.child(surface::segment(
                                    candidate.label(),
                                    move || panel.get() == candidate,
                                    move || {
                                        dismiss_keyboard();
                                        panel.set(candidate);
                                    },
                                ));
                            }
                            track
                        }))
                },
            ))
            .child(when(
                move || panel.get() == RuntimePanel::Model,
                move || {
                    let strip_initialized = stored(false);
                    effect(move || {
                        if !open.get() {
                            strip_initialized.replace(false);
                        }
                    });
                    view()
                        .w_full()
                        .flex_shrink_0()
                        .gap(px(theme::SPACING_SM))
                        .pb(px(theme::SPACING_SM))
                        .child(
                            scroll().horizontal().scrollbar(false).w_full().child(
                                view()
                                    .flex_row()
                                    .px(px(theme::SPACING_MD))
                                    .gap(px(6.0))
                                    .child(each(
                                        move || agents.get(),
                                        |row| row.0.clone(),
                                        move |row, _| {
                                            let agent_id =
                                                row.with_untracked(|row| row.0.clone());
                                            let chip = element_ref();
                                            let laid_out = signal(false);
                                            let chosen = move || {
                                                active_agent.with(|id| {
                                                    row.with(|row| id.as_ref() == Some(&row.0))
                                                })
                                            };
                                            effect(move || {
                                                if laid_out.get()
                                                    && open.get()
                                                    && active_agent.with(|id| {
                                                        id.as_deref() == Some(agent_id.as_str())
                                                    })
                                                {
                                                    let animated =
                                                        strip_initialized.replace(true);
                                                    chip.reveal(
                                                        RevealOptions::default()
                                                            .horizontal(ScrollAlignment::Center)
                                                            .animated(animated),
                                                    );
                                                }
                                            });
                                            view()
                                                .flex_shrink_0()
                                                .element_ref(&chip)
                                                .on_layout(move |_: &Bounds<Points>| {
                                                    if !laid_out.get_untracked() {
                                                        laid_out.set(true);
                                                    }
                                                })
                                                .child(
                                                    button()
                                                        .h(px(theme::SEGMENT_HEIGHT))
                                                        .px(px(theme::SPACING_MD))
                                                        .flex_shrink_0()
                                                        .flex_row()
                                                        .items_center()
                                                        .gap(px(6.0))
                                                        .rounded_full()
                                                        .bg(move || {
                                                            if chosen() {
                                                                theme::action_primary()
                                                            } else {
                                                                theme::fill()
                                                            }
                                                        })
                                                        .active(|style| style.opacity(0.7))
                                                        .on_press(move || {
                                                            agent.set(Some(row.with_untracked(
                                                                |row| row.0.clone(),
                                                            )))
                                                        })
                                                        .child(dynamic(move || {
                                                            svg(icon(agent_icon(
                                                                &row.with(|row| row.0.clone()),
                                                            )))
                                                            .size(px(14.0))
                                                            .flex_shrink_0()
                                                            .text_color(move || {
                                                                if chosen() {
                                                                    theme::action_primary_label()
                                                                } else {
                                                                    theme::text_secondary()
                                                                }
                                                            })
                                                        }))
                                                        .child(
                                                            text(move || {
                                                                row.with(|row| row.1.clone())
                                                            })
                                                            .text_size(px(theme::font_caption()))
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .text_color(move || {
                                                                if chosen() {
                                                                    theme::action_primary_label()
                                                                } else {
                                                                    theme::text_secondary()
                                                                }
                                                            }),
                                                        ),
                                                )
                                        },
                                    )),
                            ),
                        )
                        .child(
                            surface::field_pill()
                                .mx(px(theme::SPACING_MD))
                                .child(surface::field_glyph(icon!("search")))
                                .child(
                                    field::text_field_bare(
                                        query,
                                        copy::text("Search models", "搜索模型", "搜尋模型"),
                                    )
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .h_full(),
                                ),
                        )
                },
            ))
            .child(
                scroll().flex_shrink(1.0).min_h(px(0.0)).child(
                    view()
                        .w_full()
                        .px(px(theme::SPACING_MD))
                        // This sheet has a search field above it: the
                        // keyboard's room comes out of the list, so a focused
                        // field is not pushed under the sheet's lip.
                        .pb_keyboard_plus(px(theme::SPACING_LG))
                        .child(when(move || panel.get() == RuntimePanel::Model, {
                            let choose = host.on_choose_runtime.clone();
                            move || {
                                let choose = choose.clone();
                                view()
                                    .w_full()
                                    .child(when(
                                        move || !filtered.with(Vec::is_empty),
                                        move || {
                                            let choose = choose.clone();
                                            let collapsed = collapsed;
                                            dynamic(move || {
                                            view().w_full().flex_col().gap(px(theme::SPACING_MD)).children(
                                                grouped.get().into_iter().map(
                                                    |(key, label, options)| {
                                                        let choose = choose.clone();
                                                        let fold_key = key.clone();
                                                        let open = move || {
                                                            !collapsed.with(|folded| {
                                                                folded.iter().any(|folded| *folded == fold_key)
                                                            })
                                                        };
                                                        let toggle = {
                                                            let key = key.clone();
                                                            move || {
                                                                let key = key.clone();
                                                                collapsed.update(|folded| {
                                                                    if let Some(at) = folded
                                                                        .iter()
                                                                        .position(|folded| *folded == key)
                                                                    {
                                                                        folded.remove(at);
                                                                    } else {
                                                                        folded.push(key);
                                                                    }
                                                                });
                                                            }
                                                        };
                                                        // The source's own mark, so a fold says
                                                        // whose models are under it before the
                                                        // name is read.
                                                        let source_mark = source_mark_svg(
                                                            &key,
                                                            &label,
                                                            &options[0].agent_id,
                                                            options.iter().find_map(|option| {
                                                                (!option.model_id.is_empty())
                                                                    .then_some(option.model_id.as_str())
                                                            }),
                                                        );
                                                        let selected_here = options.iter().any(|o| o.selected);
                                                        sheet_card()
                                                            .child(
                                                                button()
                                                                    .w_full()
                                                                    .min_h(px(theme::ROW_HEIGHT))
                                                                    .px(px(surface::ROW_INSET))
                                                                    .flex_row()
                                                                    .items_center()
                                                                    .gap(px(theme::SPACING_MD))
                                                                    .active(|style| {
                                                                        style.bg(theme::row_pressed_bg())
                                                                    })
                                                                    .on_press(move || toggle())
                                                                    .child(
                                                                        view()
                                                                            .flex_shrink_0()
                                                                            .child(
                                                                                svg(source_mark)
                                                                                    .size(px(theme::ICON_MD))
                                                                                    .text_color(theme::foreground),
                                                                            ),
                                                                    )
                                                                    .child(
                                                                        text(label)
                                                                            .flex_1()
                                                                            .min_w(px(0.0))
                                                                            .truncate()
                                                                            .text_size(px(theme::font_caption()))
                                                                            .font_weight(FontWeight::MEDIUM)
                                                                            .text_color(theme::text_secondary),
                                                                    )
                                                                    .child(when(
                                                                        move || selected_here,
                                                                        || {
                                                                            svg(icon!("check"))
                                                                                .size(px(theme::ICON_SM))
                                                                                .flex_shrink_0()
                                                                                .text_color(theme::text_muted)
                                                                        },
                                                                    ))
                                                                    .child({
                                                                        // `icon!` takes a
                                                                        // literal, so the two
                                                                        // faces are two calls.
                                                                        if open() {
                                                                            svg(icon!("chevron-up"))
                                                                                .size(px(theme::ICON_SM))
                                                                                .flex_shrink_0()
                                                                                .text_color(theme::text_muted)
                                                                                .into_element()
                                                                        } else {
                                                                            svg(icon!("chevron-down"))
                                                                                .size(px(theme::ICON_SM))
                                                                                .flex_shrink_0()
                                                                                .text_color(theme::text_muted)
                                                                                .into_element()
                                                                        }
                                                                    }),
                                                            )
                                                            .child(when(open, {
                                                                let choose = choose.clone();
                                                                let options = options.clone();
                                                                move || {
                                                                    let choose = choose.clone();
                                                                    view().w_full().flex_col().children(
                                                                        options.iter().map(|row| {
                                                                            let choose = choose.clone();
                                                                            let id = row.id.clone();
                                                                            let selected = row.selected;
                                                                            let available = row.available;
                                                                            let model = row.model.clone();
                                                                            let detail = row.detail.clone();
                                                                            let model_mark =
                                                                                model_mark_svg(&row.model_id);
                                                                            button()
                                                                                .w_full()
                                                                                .min_h(px(theme::ROW_HEIGHT))
                                                                                .pl(px(theme::SPACING_XL + theme::SPACING_MD))
                                                                                .pr(px(surface::ROW_INSET))
                                                                                .py(px(theme::SPACING_SM))
                                                                                .flex_row()
                                                                                .items_center()
                                                                                .gap(px(theme::SPACING_MD))
                                                                                .active(|style| {
                                                                                    style.bg(theme::row_pressed_bg())
                                                                                })
                                                                                .on_press(move || {
                                                                                    dismiss_keyboard();
                                                                                    choose(&id);
                                                                                })
                                                                                .disabled(move || busy.get() || !available)
                                                                                .disabled_style(|style| {
                                                                                    style.opacity(0.45)
                                                                                })
                                                                                .child(
                                                                                    svg(model_mark)
                                                                                        .size(px(theme::ICON_SM))
                                                                                        .flex_shrink_0()
                                                                                        .text_color(theme::text_secondary),
                                                                                )
                                                                                .child(
                                                                                    view()
                                                                                        .flex_1()
                                                                                        .min_w(px(0.0))
                                                                                        .flex_col()
                                                                                        .gap(px(1.0))
                                                                                        .child(
                                                                                            text(model)
                                                                                                .truncate()
                                                                                                .text_size(px(theme::font_detail()))
                                                                                                .text_color(theme::foreground),
                                                                                        )
                                                                                        .children(
                                                                                            (!detail.is_empty()).then(|| {
                                                                                                text(detail)
                                                                                                    .truncate()
                                                                                                    .text_size(px(theme::font_micro()))
                                                                                                    .text_color(theme::text_muted)
                                                                                            }),
                                                                                        ),
                                                                                )
                                                                                .child(when(
                                                                                    move || selected,
                                                                                    || {
                                                                                        svg(icon!("check"))
                                                                                            .size(px(theme::ICON_MD))
                                                                                            .flex_shrink_0()
                                                                                            .text_color(theme::foreground)
                                                                                    },
                                                                                ))
                                                                        }),
                                                                    )
                                                                }
                                                            }))
                                                    },
                                                ),
                                            )
                                            })
                                        },
                                    ))
                                    .child(when(
                                        move || filtered.with(Vec::is_empty),
                                        move || {
                                            text(move || {
                                                if busy.get() {
                                                    copy::text("Loading…", "正在加载…", "正在載入…")
                                                } else {
                                                    copy::text(
                                                        "No matching choices",
                                                        "没有匹配选项",
                                                        "沒有符合的選項",
                                                    )
                                                }
                                            })
                                            .p(px(theme::SPACING_SM))
                                            .text_size(px(theme::font_caption()))
                                            .text_color(theme::text_muted)
                                        },
                                    ))
                            }
                        }))
                        .child(when(
                            move || panel.get() == RuntimePanel::Reasoning,
                            move || {
                                choice_section(
                                    reasoning,
                                    busy,
                                    host.on_choose_reasoning.clone(),
                                    finish_reasoning.clone(),
                                )
                            },
                        ))
                        .child(when(
                            move || panel.get() == RuntimePanel::Mode,
                            move || {
                                choice_section(
                                    modes,
                                    busy,
                                    host.on_choose_mode.clone(),
                                    finish_mode.clone(),
                                )
                            },
                        ))
                        .child(when(
                            move || panel.get() == RuntimePanel::Options,
                            move || {
                                feature_section(
                                    features,
                                    busy,
                                    host.on_choose_feature.clone(),
                                    finish_options.clone(),
                                )
                            },
                        )),
                ),
            ),
    )
}

fn choice_section(
    choices: Signal<Vec<RuntimeChoiceView>>,
    busy: Signal<bool>,
    on_choose: Rc<dyn Fn(&str)>,
    close: Action,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .flex_wrap()
        .gap(px(theme::SPACING_SM))
        .child(each(
            move || choices.get(),
            |choice| choice.value.clone(),
            move |choice, _| {
                let value = choice.with(|choice| choice.value.clone());
                let on_choose = on_choose.clone();
                let close = close.clone();
                surface::choice_pill(
                    move || choice.with(|choice| choice.label.clone()),
                    move || choice.with(|choice| choice.selected),
                    move || {
                        on_choose(&value);
                        close();
                    },
                )
                .disabled(move || busy.get())
            },
        ))
}

fn feature_section(
    features: Signal<Vec<RuntimeFeatureView>>,
    busy: Signal<bool>,
    on_choose: ChooseFeature,
    close: Action,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_LG))
        .child(each(
            move || features.get(),
            |feature| feature.id.clone(),
            move |feature, _| {
                let on_choose = on_choose.clone();
                let close = close.clone();
                view()
                    .w_full()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        view()
                            .w_full()
                            .px(px(theme::SPACING_XS))
                            .gap(px(2.0))
                            .child(
                                text(move || feature.with(|feature| feature.label.clone()))
                                    .text_size(px(theme::font_detail()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::foreground),
                            )
                            .child(when(
                                move || feature.with(|feature| !feature.detail.is_empty()),
                                move || {
                                    text(move || feature.with(|feature| feature.detail.clone()))
                                        .text_size(px(theme::font_micro()))
                                        .line_height(theme::line_height_for(theme::font_micro()))
                                        .text_color(theme::text_muted)
                                },
                            )),
                    )
                    .child(dynamic(move || {
                        let snapshot = feature.get();
                        let mut choices =
                            vec![(None, copy::text("Default", "默认", "預設").to_owned())];
                        if snapshot.toggle {
                            choices.extend([
                                (
                                    Some("true".to_owned()),
                                    copy::text("On", "开", "開").to_owned(),
                                ),
                                (
                                    Some("false".to_owned()),
                                    copy::text("Off", "关", "關").to_owned(),
                                ),
                            ]);
                        } else {
                            choices.extend(
                                snapshot
                                    .values
                                    .into_iter()
                                    .map(|value| (Some(value.value), value.label)),
                            );
                        }
                        view()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(theme::SPACING_SM))
                            .children(choices.into_iter().map(|(value, label)| {
                                let selected = value == snapshot.explicit_value;
                                let id = snapshot.id.clone();
                                let on_choose = on_choose.clone();
                                let close = close.clone();
                                surface::choice_pill(
                                    move || label.clone(),
                                    move || selected,
                                    move || {
                                        on_choose(&id, value.as_deref());
                                        close();
                                    },
                                )
                                .disabled(move || busy.get())
                            }))
                    }))
            },
        ))
}

/// What floats over the workspace's three pages: the menu a held row raises
/// and the sheets its commands lead to. One set serves every page, so a
/// session's commands read the same from its row and from its own header.
#[derive(Clone, Copy)]
struct Overlays {
    menu: Signal<Option<Menu>>,
    /// Whether the session the host's `session_actions` names is being
    /// renamed.
    rename: Signal<bool>,
    /// The held send being moved to another hour.
    reschedule: Signal<Option<(String, u64)>>,
}

/// The sheets a folder's or a group's menu leads to.
#[derive(Clone, Copy)]
struct SectionSheets {
    /// The folder or group being renamed, and the name being written for it.
    rename_folder: Signal<Option<String>>,
    rename_group: Signal<Option<String>>,
    name: Signal<String>,
    /// The folder whose archive window is being chosen.
    archive: Signal<Option<String>>,
    /// The group whose members are being edited, and the ones ticked so far.
    members: Signal<Option<String>>,
    members_selected: Signal<Vec<String>>,
}

/// Asks before a command that cannot be taken back, then runs it.
///
/// The drawer's destructive commands all leave the same way: a native alert
/// that names what goes away, and the command only when its own button was
/// pressed. A long press followed by a tap is one slip away from losing a
/// session, so nothing here runs on a single tap.
fn confirm_destructive(
    title: &'static str,
    description: impl Into<SharedString> + 'static,
    action_id: &'static str,
    action_label: &'static str,
    run: impl FnOnce() + 'static,
) {
    // Under an owner of its own. The menu closes the instant its entry is
    // pressed, and a task spawned under that entry's owner is cancelled with
    // it — before its first poll, so the alert never appeared and the command
    // never ran. This is the same reason the drawer's service calls detach.
    Owner::detached().run(move || {
        spawn(async move {
            let answer = nsh_base::alert_dialog()
                .title(title)
                .description(description)
                .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                .destructive(action_id, action_label)
                .show()
                .await;
            if matches!(answer, Ok(answer) if answer.action.as_deref() == Some(action_id)) {
                run();
            }
        });
    });
}

/// The commands of one session, as its row and its own header both raise
/// them. `lead` goes first: what only the place that raised the menu offers.
fn session_menu(
    host: &SessionHost,
    overlays: Overlays,
    id: &str,
    title: &str,
    place: (f32, MenuSide),
    lead: Vec<MenuEntry>,
) -> Menu {
    // The service's own commands (rename, move, archive, delete) act on the
    // session named here.
    (host.on_session_menu)(id, title);
    let listed =
        |list: Signal<Vec<String>>| list.with_untracked(|list| list.iter().any(|item| item == id));
    let pinned = listed(host.pinned);
    let auto_continue = listed(host.auto_continue_sessions);
    let paused = listed(host.auto_continue_paused);
    let send = host
        .scheduled_sends
        .with_untracked(|sends| sends.iter().find(|send| send.session_id == id).cloned());
    let summary = host
        .sessions
        .with_untracked(|sessions| sessions.iter().find(|session| session.id == id).cloned());
    let sections = host.sidebar_sections.get_untracked();
    // A session belongs to at most one group, so leaving it and joining one
    // are the two halves of one question.
    let group = sections
        .iter()
        .find(|section| {
            section.kind == SidebarSectionKind::Group
                && section.session_ids.iter().any(|member| member == id)
        })
        .cloned();
    // Groups the session could join: the same project and worktree, since a
    // group is a pane layout over one checkout.
    let joinable = summary.as_ref().map_or_else(Vec::new, |session| {
        sections
            .iter()
            .filter(|section| {
                section.kind == SidebarSectionKind::Group
                    && section.project_id.as_deref() == Some(session.project_id.as_str())
                    && section.workspace_id.as_deref() == Some(session.workspace_id.as_str())
                    && !section.session_ids.iter().any(|member| member == id)
            })
            .cloned()
            .collect()
    });
    let id = id.to_owned();
    // A command that needs no sheet lets go of the session once it ran.
    let done = host.on_close_session_actions.clone();

    let mut menu = Menu::at(place.0).side(place.1).on_dismiss({
        let done = done.clone();
        move || done()
    });
    for entry in lead {
        menu = menu.entry(entry);
    }
    menu = menu
        .entry(MenuEntry::new(
            icon!("pencil"),
            copy::text("Rename", "重命名", "重新命名"),
            move || overlays.rename.set(true),
        ))
        .entry(MenuEntry::new(
            icon!("pin"),
            if pinned {
                copy::text("Unpin", "取消置顶", "取消置頂")
            } else {
                copy::text("Pin", "置顶", "置頂")
            },
            {
                let toggle = host.on_toggle_pin.clone();
                let done = done.clone();
                let id = id.clone();
                move || {
                    toggle(&id);
                    done();
                }
            },
        ))
        .entry(MenuEntry::new(
            icon!("git-branch"),
            copy::text("Fork session", "分叉会话", "分支會話"),
            {
                let fork = host.on_fork_sessions.clone();
                let done = done.clone();
                let id = id.clone();
                move || {
                    fork(std::slice::from_ref(&id));
                    done();
                }
            },
        ))
        .entry(MenuEntry::new(
            icon!("zap"),
            if auto_continue {
                copy::text("Disable auto continue", "关闭自动继续", "關閉自動繼續")
            } else {
                copy::text("Auto continue", "自动继续", "自動繼續")
            },
            {
                let toggle = host.on_set_session_auto_continue.clone();
                let done = done.clone();
                let id = id.clone();
                move || {
                    toggle(&id, !auto_continue);
                    done();
                }
            },
        ))
        .entry_if(paused, || {
            let resume = host.on_resume_auto_continue.clone();
            let done = done.clone();
            let id = id.clone();
            MenuEntry::new(
                icon!("rotate-ccw"),
                copy::text("Resume auto continue", "恢复自动继续", "恢復自動繼續"),
                move || {
                    resume(&id);
                    done();
                },
            )
        });
    if let Some(send) = send {
        let cancel = host.on_cancel_scheduled_send.clone();
        let done = done.clone();
        let later = (send.session_id.clone(), send.message_id);
        menu = menu
            .entry(MenuEntry::new(
                icon!("clock"),
                copy::text("Send later instead", "改期发送", "改期傳送"),
                {
                    let done = done.clone();
                    move || {
                        // The sheet carries the send it moves.
                        overlays.reschedule.set(Some(later.clone()));
                        done();
                    }
                },
            ))
            .entry(
                MenuEntry::new(
                    icon!("x"),
                    copy::text("Cancel scheduled send", "取消定时发送", "取消定時傳送"),
                    move || {
                        cancel(&send.session_id, send.message_id);
                        done();
                    },
                )
                .danger(),
            );
    }
    if let Some(group) = group {
        let set_members = host.on_set_group_members.clone();
        let done = done.clone();
        let id = id.clone();
        menu = menu.entry(MenuEntry::new(
            icon!("layers"),
            copy::text("Remove from session group", "移出会话组", "移出會話組"),
            move || {
                let members = group
                    .session_ids
                    .iter()
                    .filter(|member| **member != id)
                    .cloned()
                    .collect::<Vec<_>>();
                set_members(&group.id, &members);
                done();
            },
        ));
    }
    for group in joinable {
        let set_members = host.on_set_group_members.clone();
        let done = done.clone();
        let id = id.clone();
        menu = menu.entry(MenuEntry::new(
            icon!("layers"),
            add_to_group_label(&group.name),
            move || {
                let mut members = group.session_ids.clone();
                members.push(id.clone());
                set_members(&group.id, &members);
                done();
            },
        ));
    }
    menu = menu.entry(MenuEntry::new(
        icon!("folder"),
        copy::text("Move to folder", "移动到文件夹", "移動到資料夾"),
        {
            let open = host.on_open_move.clone();
            let done = done.clone();
            move || {
                // The sheet takes the session with it.
                open();
                done();
            }
        },
    ));
    menu.entry(MenuEntry::new(
        icon!("package"),
        copy::text("Archive", "归档", "封存"),
        {
            let archive = host.on_archive_session.clone();
            move || archive()
        },
    ))
    .entry(
        MenuEntry::new(
            icon!("trash-2"),
            copy::text("Delete", "删除", "刪除"),
            {
                let delete = host.on_delete_session.clone();
                let title = title.to_owned();
                move || {
                    let delete = delete.clone();
                    let title = title.clone();
                    confirm_destructive(
                        copy::text("Delete this session?", "删除该会话？", "刪除該工作階段？"),
                        format!(
                            "“{title}” {}",
                            copy::text(
                                "and its timeline leave the workbench. The files it changed stay on disk.",
                                "及其时间线会一并移出工作台，它改动的文件会保留在磁盘上。",
                                "及其時間軸會一併移出工作台，它改動的檔案會保留在磁碟上。",
                            )
                        ),
                        "delete",
                        copy::text("Delete", "删除", "刪除"),
                        move || delete(),
                    );
                }
            },
        )
        .danger(),
    )
}

/// The commands of a project or of one of its worktrees.
fn scope_menu(
    host: &SessionHost,
    row: &SidebarRowView,
    anchor: f32,
    on_new_session: Rc<dyn Fn(&SidebarRowView)>,
) -> Menu {
    let project = row.kind == SidebarRowKind::Project;
    let menu = Menu::at(anchor)
        .entry(MenuEntry::new(
            icon!("plus"),
            copy::text("New session", "新建会话", "新增工作階段"),
            {
                let row = row.clone();
                move || on_new_session(&row)
            },
        ))
        .entry(MenuEntry::new(
            icon!("folder-plus"),
            copy::text("New folder", "新建文件夹", "新增資料夾"),
            {
                let create = host.sidebar.on_new_folder.clone();
                let row = row.clone();
                move || create(&row)
            },
        ));
    if !project {
        return menu;
    }
    let sessions = host.sessions;
    menu.entry(MenuEntry::new(
        icon!("git-branch"),
        if row.project_starts_in_worktree {
            copy::text(
                "Start sessions in the checkout",
                "默认在当前检出中开始",
                "預設在目前檢出中開始",
            )
        } else {
            copy::text(
                "Start sessions in a new worktree",
                "默认在新建 Worktree 中开始",
                "預設在新建 Worktree 中開始",
            )
        },
        {
            let toggle = host.on_toggle_project_new_session_location.clone();
            let id = row.id.clone();
            move || toggle(&id)
        },
    ))
    .entry(
        MenuEntry::new(
            icon!("trash-2"),
            copy::text("Remove project", "移除项目", "移除專案"),
            {
                let remove = host.on_remove_project.clone();
                let project_id = row.id.clone();
                move || {
                    let count = sessions.with_untracked(|sessions| {
                        sessions
                            .iter()
                            .filter(|session| session.project_id == project_id)
                            .count()
                    });
                    let remove = remove.clone();
                    let project_id = project_id.clone();
                    spawn(async move {
                        let answer = nsh_base::alert_dialog()
                            .title(copy::text(
                                "Remove this project?",
                                "移除该项目？",
                                "移除該專案？",
                            ))
                            .description(format!(
                                "{} {}",
                                count,
                                copy::text(
                                    "sessions leave the workbench with it. Nothing on disk is deleted.",
                                    "个会话会一并移出工作台，磁盘上的文件不会被删除。",
                                    "個工作階段會一併移出工作台，磁碟上的檔案不會被刪除。",
                                )
                            ))
                            .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                            .destructive("remove", copy::text("Remove", "移除", "移除"))
                            .show()
                            .await;
                        if matches!(answer, Ok(answer) if answer.action.as_deref() == Some("remove"))
                        {
                            remove(&project_id);
                        }
                    });
                }
            },
        )
        .danger(),
    )
}

/// The commands of a folder.
fn folder_menu(
    host: &SessionHost,
    row: &SidebarRowView,
    anchor: f32,
    sheets: SectionSheets,
    on_new_session: Rc<dyn Fn(&SidebarRowView)>,
) -> Menu {
    let id = row.id.clone();
    let days = host.sidebar_sections.with_untracked(|sections| {
        sections
            .iter()
            .find(|section| section.kind == SidebarSectionKind::Folder && section.id == id)
            .and_then(|section| section.auto_archive_after_days)
    });
    let menu = Menu::at(anchor)
        .entry(MenuEntry::new(
            icon!("message-square"),
            new_chat_in_folder_label(&row.title),
            {
                let row = row.clone();
                move || on_new_session(&row)
            },
        ))
        .entry(MenuEntry::new(
            icon!("pencil"),
            copy::text("Rename", "重命名", "重新命名"),
            {
                let id = id.clone();
                let title = row.title.clone();
                move || {
                    sheets.name.set(title.clone());
                    sheets.rename_folder.set(Some(id.clone()));
                }
            },
        ))
        .entry(MenuEntry::new(
            icon!("folder-plus"),
            copy::text("New folder", "新建文件夹", "新增資料夾"),
            {
                let open = host.on_open_subfolder.clone();
                let id = id.clone();
                move || open(&id)
            },
        ))
        .entry(MenuEntry::new(
            icon!("package"),
            format!(
                "{} · {}",
                copy::text("Scheduled archive", "定时归档", "定時封存"),
                folder_archive_label(days)
            ),
            {
                let id = id.clone();
                move || sheets.archive.set(Some(id.clone()))
            },
        ));
    menu.entry(
        MenuEntry::new(
            icon!("trash-2"),
            copy::text("Delete", "删除", "刪除"),
            {
                let delete = host.on_delete_folder.clone();
                let title = row.title.clone();
                move || {
                    let delete = delete.clone();
                    let title = title.clone();
                    let id = id.clone();
                    confirm_destructive(
                        copy::text("Delete this folder?", "删除该文件夹？", "刪除該資料夾？"),
                        format!(
                            "“{title}” {}",
                            copy::text(
                                "and the folders inside it leave the drawer. Its sessions move up one level; nothing on disk is deleted.",
                                "及其中的文件夹会移出侧边栏，其中的会话会上移一级；磁盘上的文件不会被删除。",
                                "及其中的資料夾會移出側邊欄，其中的工作階段會上移一級；磁碟上的檔案不會被刪除。",
                            )
                        ),
                        "delete",
                        copy::text("Delete", "删除", "刪除"),
                        move || delete(&id),
                    );
                }
            },
        )
        .danger(),
    )
}

/// The commands of a session group.
fn group_menu(
    host: &SessionHost,
    row: &SidebarRowView,
    anchor: f32,
    sheets: SectionSheets,
) -> Menu {
    let id = row.id.clone();
    let section = host.sidebar_sections.with_untracked(|sections| {
        sections
            .iter()
            .find(|section| section.kind == SidebarSectionKind::Group && section.id == id)
            .cloned()
    });
    let pinned = section.as_ref().is_some_and(|section| section.pinned);
    let auto_continue = section
        .as_ref()
        .is_some_and(|section| section.auto_continue);
    let members = section
        .map(|section| section.session_ids)
        .unwrap_or_default();
    Menu::at(anchor)
        .entry(MenuEntry::new(
            icon!("pencil"),
            copy::text("Rename", "重命名", "重新命名"),
            {
                let id = id.clone();
                let title = row.title.clone();
                move || {
                    sheets.name.set(title.clone());
                    sheets.rename_group.set(Some(id.clone()));
                }
            },
        ))
        .entry(MenuEntry::new(
            icon!("pin"),
            if pinned {
                copy::text("Unpin", "取消置顶", "取消置頂")
            } else {
                copy::text("Pin", "置顶", "置頂")
            },
            {
                let pin = host.on_set_group_pinned.clone();
                let id = id.clone();
                move || pin(&id, !pinned)
            },
        ))
        .entry(MenuEntry::new(
            icon!("zap"),
            if auto_continue {
                copy::text("Disable auto continue", "关闭自动继续", "關閉自動繼續")
            } else {
                copy::text("Auto continue", "自动继续", "自動繼續")
            },
            {
                let toggle = host.on_set_group_auto_continue.clone();
                let id = id.clone();
                move || toggle(&id, !auto_continue)
            },
        ))
        .entry(MenuEntry::new(
            icon!("user"),
            copy::text("Edit members", "编辑成员", "編輯成員"),
            {
                let id = id.clone();
                move || {
                    sheets.members_selected.set(members.clone());
                    sheets.members.set(Some(id.clone()));
                }
            },
        ))
        .entry(
            MenuEntry::new(
                icon!("layers"),
                copy::text("Dissolve group", "解散分组", "解散群組"),
                {
                    let dissolve = host.on_delete_group.clone();
                    let title = row.title.clone();
                    move || {
                        let dissolve = dissolve.clone();
                        let title = title.clone();
                        let id = id.clone();
                        confirm_destructive(
                            copy::text("Dissolve this group?", "解散该分组？", "解散該群組？"),
                            format!(
                                "“{title}” {}",
                                copy::text(
                                    "goes away and its sessions return to the drawer on their own.",
                                    "会被移除，其中的会话会各自回到侧边栏。",
                                    "會被移除，其中的工作階段會各自回到側邊欄。",
                                )
                            ),
                            "dissolve",
                            copy::text("Dissolve", "解散", "解散"),
                            move || dissolve(&id),
                        );
                    }
                },
            )
            .danger(),
        )
}

/// A sheet that asks for one name: the field, and the action that takes it.
#[allow(clippy::too_many_arguments)]
fn name_sheet(
    open: impl Fn() -> bool + 'static,
    title: &'static str,
    placeholder: &'static str,
    name: Signal<String>,
    action_label: &'static str,
    busy: impl Fn() -> bool + 'static,
    on_submit: impl Fn() + 'static,
    on_close: impl Fn() + Clone + 'static,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_header};
    bottom_sheet(
        open,
        on_close.clone(),
        view()
            .w_full()
            .child(sheet_header(title, move || {
                dismiss_keyboard();
                on_close();
            }))
            .child(
                sheet_body()
                    .child(field::text_field_well(name, placeholder))
                    .child(
                        action::solid(action_label, move || {
                            dismiss_keyboard();
                            on_submit();
                        })
                        .w_full()
                        .disabled(move || busy() || name.with(|name| name.trim().is_empty())),
                    ),
            ),
    )
}

/// Renames the session the host's `session_actions` names.
fn rename_session_sheet(host: &SessionHost, open: Signal<bool>) -> impl IntoElement + use<> {
    let actions = host.session_actions;
    let busy = host.session_busy;
    let rename = host.on_rename_session.clone();
    let close = host.on_close_session_actions.clone();
    // The service lets go of the session once it carries its new name.
    effect(move || {
        if actions.get().is_none() {
            open.set(false);
        }
    });
    name_sheet(
        move || open.get(),
        copy::text("Rename session", "重命名会话", "重新命名工作階段"),
        copy::text("Session title", "会话标题", "工作階段標題"),
        host.session_rename,
        copy::text("Save", "保存", "儲存"),
        move || busy.get(),
        move || rename(),
        move || {
            if open.get_untracked() {
                open.set(false);
                close();
            }
        },
    )
}

/// The three hours a message can be held until, as both the composer's
/// schedule and a held send's new deadline ask them: a deadline is a deadline
/// whichever side of the conversation set it.
fn delay_sheet(
    open: impl Fn() -> bool + 'static,
    on_close: impl Fn() + Clone + 'static,
    on_pick: impl Fn(u64) + Clone + 'static,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
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
    let mut card = sheet_card();
    for (index, (label, seconds)) in choices.into_iter().enumerate() {
        let on_pick = on_pick.clone();
        if index > 0 {
            card = card.child(surface::row_divider());
        }
        card = card.child(surface::row(icon!("clock"), label, move || {
            on_pick(seconds)
        }));
    }
    bottom_sheet(
        open,
        on_close.clone(),
        view()
            .w_full()
            .child(sheet_header(
                copy::text("Send later", "稍后发送", "稍後傳送"),
                on_close,
            ))
            .child(sheet_body().child(card)),
    )
}

/// Moves one held send to a new hour.
fn scheduled_send_sheet(
    open: Signal<Option<(String, u64)>>,
    on_reschedule: RescheduleScheduledAction,
) -> impl IntoElement {
    delay_sheet(
        move || open.with(Option::is_some),
        move || open.set(None),
        move |seconds| {
            let Some((session_id, message_id)) = open.get_untracked() else {
                return;
            };
            open.set(None);
            on_reschedule(&session_id, message_id, seconds);
        },
    )
}

fn move_session_sheet(
    open: Signal<Option<String>>,
    sections: Signal<Vec<SidebarSectionView>>,
    on_move: MoveSessionAction,
    on_close: Action,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
    let folders = computed(move || {
        sections.with(|sections| {
            sections
                .iter()
                .filter(|section| section.kind == SidebarSectionKind::Folder)
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    bottom_sheet(
        move || open.with(Option::is_some),
        {
            let on_close = on_close.clone();
            move || on_close()
        },
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.7))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Move to folder", "移动到文件夹", "移動到資料夾"),
                move || on_close(),
            ))
            .child(
                scroll().flex_shrink(1.0).min_h(px(0.0)).child(
                    sheet_body().child(
                        sheet_card()
                            .child({
                                let on_move = on_move.clone();
                                surface::row(
                                    icon!("x"),
                                    copy::text("No folder", "不放入文件夹", "不放入資料夾"),
                                    move || {
                                        if let Some(id) = open.get_untracked() {
                                            on_move(&id, None);
                                        }
                                    },
                                )
                            })
                            .child(each(
                                move || folders.get(),
                                |folder| folder.id.clone(),
                                move |folder, _| {
                                    let on_move = on_move.clone();
                                    view().w_full().child(surface::row_divider()).child(
                                        surface::row(
                                            icon!("folder"),
                                            move || folder.with(|folder| folder.name.clone()),
                                            move || {
                                                if let Some(id) = open.get_untracked() {
                                                    on_move(
                                                        &id,
                                                        Some(&folder.with_untracked(|folder| {
                                                            folder.id.clone()
                                                        })),
                                                    );
                                                }
                                            },
                                        ),
                                    )
                                },
                            )),
                    ),
                ),
            ),
    )
}

fn attachment_preview_sheet(
    preview: Signal<Option<AttachmentItem>>,
    close: Action,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_header};
    let image_shown = move || {
        preview.with(|preview| {
            preview
                .as_ref()
                .is_some_and(|preview| preview.mime_type.starts_with("image/"))
        })
    };
    bottom_sheet(
        move || preview.with(Option::is_some),
        {
            let close = close.clone();
            move || close()
        },
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.8))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Attachment", "附件", "附件"),
                move || close(),
            ))
            .child(
                sheet_body()
                    .child(when(image_shown, move || {
                        image(move || {
                            ImageSrc::from(preview.with(|preview| {
                                preview
                                    .as_ref()
                                    .map(|preview| preview.uri.clone())
                                    .unwrap_or_default()
                            }))
                        })
                        .w_full()
                        .rounded(px(theme::RADIUS_GROUP))
                        .object_fit(nsh_core::ResizeMode::Contain)
                    }))
                    .child(
                        surface::card().child(
                            view()
                                .w_full()
                                .min_h(px(theme::ROW_HEIGHT))
                                .px(px(surface::ROW_INSET))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_MD))
                                .child(surface::tile(icon!("paperclip")))
                                .child(
                                    text(move || {
                                        preview.with(|preview| {
                                            preview
                                                .as_ref()
                                                .map(|preview| preview.label.clone())
                                                .unwrap_or_default()
                                        })
                                    })
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme::font_body()))
                                    .text_color(theme::foreground),
                                ),
                        ),
                    ),
            ),
    )
}

/// The mark a model wears in the picker.
///
/// The model's own brand, from the same table the desktop reads; a model no
/// brand owns falls back to the generic sparkle the desktop uses for one.
fn model_mark_svg(model_id: &str) -> ImageSrc {
    vibex_brands::model_mark(model_id)
        .and_then(|mark| vibex_brands::mark_bytes(mark.path))
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .map(ImageSrc::svg_markup)
        .unwrap_or_else(|| icon!("sparkles"))
}

fn runtime_mark_svg(model_id: &str, agent_id: &str) -> ImageSrc {
    vibex_brands::model_mark(model_id)
        .or_else(|| vibex_brands::agent_mark(agent_id))
        .and_then(|mark| vibex_brands::mark_bytes(mark.path))
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .map(ImageSrc::svg_markup)
        .unwrap_or_else(|| icon!("sparkles"))
}

/// The unified composer trigger for Agent settings, shared between open
/// sessions and the new-session screen.
pub(crate) fn runtime_trigger(
    summary: Signal<Option<RuntimeSummaryView>>,
    disabled: impl Fn() -> bool + 'static,
    on_open: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    compose::runtime_pill(
        move || {
            summary.with(|summary| {
                summary.as_ref().map_or_else(
                    || icon!("sparkles"),
                    |summary| runtime_mark_svg(&summary.model_id, &summary.agent_id),
                )
            })
        },
        move || {
            SharedString::from(summary.with(|summary| {
                summary.as_ref().map_or_else(
                    || copy::text("Model", "模型", "模型").to_owned(),
                    |summary| summary.model.clone(),
                )
            }))
        },
        move || {
            summary.with(|summary| {
                summary
                    .as_ref()
                    .filter(|summary| summary.reasoning_set && !summary.reasoning.is_empty())
                    .map(|summary| SharedString::from(summary.reasoning.clone()))
            })
        },
        move || {
            if summary.with(|summary| summary.as_ref().is_some_and(|summary| !summary.available)) {
                theme::accent_yellow()
            } else {
                theme::foreground()
            }
        },
        on_open,
    )
    .disabled(disabled)
}

/// The mark a source fold wears in the runtime picker.
///
/// Matches desktop and GPUI mobile: an Agent account wears the Agent's own
/// mark, a route group wears the branch glyph, and a provider profile wears
/// its brand mark (resolved from the provider's name, then its first model id)
/// falling back to the database glyph when no brand owns it.
fn source_mark_svg(
    source_key: &str,
    source_label: &str,
    agent_id: &str,
    first_model_id: Option<&str>,
) -> ImageSrc {
    if source_key.starts_with("account:") {
        return ImageSrc::svg_markup(agent_icon(agent_id));
    }
    if source_key.starts_with("route:") {
        return icon!("git-branch");
    }
    [Some(source_label), first_model_id]
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            vibex_brands::model_mark(candidate)
                .and_then(|mark| vibex_brands::mark_bytes(mark.path))
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .map(ImageSrc::svg_markup)
        })
        .unwrap_or_else(|| icon!("database"))
}

/// The mark a session's Agent wears in the drawer.
///
/// The same catalogue the desktop and the terminal read, so an Agent has one
/// logo across every client rather than a default glyph wherever a client
/// forgot to add it. The marks are themed: those drawn in the reader's own
/// colour follow the appearance, and the ones with brand colours keep them.
fn agent_icon(agent: &str) -> &'static str {
    vibex_brands::agent_mark(agent)
        .and_then(|mark| vibex_brands::mark_bytes(mark.path))
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or_else(|| include_str!("../../../assets/icons/bot.svg"))
}

/// A quiet action written out in words, with an optional glyph before them.
fn drawer_text_action(
    icon: Option<ImageSrc>,
    label: impl IntoReactive<SharedString>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_press)
        .h(px(32.0))
        .px(px(theme::SPACING_SM))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .rounded(px(theme::SIDEBAR_ROW_RADIUS))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.4))
        .children(icon.into_iter().map(|icon| {
            svg(icon)
                .w(px(14.0))
                .h(px(14.0))
                .text_color(theme::text_secondary)
        }))
        .child(
            text(label)
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_secondary),
        )
}

/// One command of the selection bar: a glyph over the word for it.
fn drawer_batch_action(
    icon: ImageSrc,
    caption: &'static str,
    label: &'static str,
    danger: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let ink: fn() -> Hsla = if danger {
        theme::accent_red
    } else {
        theme::text_secondary
    };
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .flex_1()
        .min_w(px(0.0))
        .h(px(52.0))
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.0))
        .rounded(px(theme::SIDEBAR_ROW_RADIUS))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.4))
        .child(svg(icon).w(px(20.0)).h(px(20.0)).text_color(ink))
        .child(
            text(caption)
                .max_w(relative(1.0))
                .truncate()
                .text_size(px(theme::font_hint()))
                .text_color(ink),
        )
}

/// What a lifted row shows over the scrim: the mark it leads with, its title,
/// and what trails it in the list.
fn menu_row(row: &SidebarRowView, top: f32) -> menu::MenuRow {
    let mark = row.project_mark.clone().map(|(mark, _)| mark).or_else(|| {
        Some(match row.kind {
            SidebarRowKind::Project => icon!("boxes"),
            SidebarRowKind::Workspace => icon!("git-branch"),
            SidebarRowKind::Folder if row.collapsed => icon!("folder"),
            SidebarRowKind::Folder => icon!("folder-open"),
            SidebarRowKind::Group => icon!("layers"),
            SidebarRowKind::Session | SidebarRowKind::EmptyWorkspace => {
                icon(agent_icon(&row.detail))
            }
        })
    });
    let trailing = if !row.metadata.is_empty() {
        row.metadata.clone()
    } else if row.count > 0 {
        row.count.to_string()
    } else {
        String::new()
    };
    menu::MenuRow {
        key: row.key(),
        top,
        height: theme::sidebar_row_height(),
        mark,
        title: row.title.clone().into(),
        title_size: if matches!(
            row.kind,
            SidebarRowKind::Session | SidebarRowKind::EmptyWorkspace
        ) {
            theme::font_detail()
        } else {
            theme::font_sidebar_row()
        },
        trailing: trailing.into(),
    }
}

/// How far a tick of the list's edge scrolling moves it, and how often it
/// ticks while a carried row rests near an edge.
const DRAG_SCROLL_STEP: f32 = 8.0;
const DRAG_SCROLL_TICK_MS: u64 = 16;

fn drawer(host: SessionHost, pager: Pager, overlays: Overlays) -> impl IntoElement {
    let menu = overlays.menu;
    // The row a finger is carrying. Raising a row's menu and carrying the
    // row begin with the same hold, so the first travel puts the menu away.
    let drag = signal(None::<tree::DraggedRow>);
    // The nested row a drag has collapsed locally, so its children leave
    // the list at once without waiting for the backend. The row itself is
    // untouched, so the element holding the touch is not rebuilt.
    let drag_collapse = signal(None::<String>);
    // The row being carried belongs to the page rather than to a row: the
    // list rebuilds its rows while a gesture is in flight.
    let shifts = signal(std::collections::HashMap::new());
    // The list's viewport and scroll, so a carried row near an edge keeps
    // the list scrolling under it.
    let list = signal(None::<(f32, f32)>);
    let list_offset = signal(0.0f32);
    let scroll_travel = signal(0.0f32);
    let auto_scroll = signal(0.0f32);
    let list_ref = element_ref();
    {
        let list_ref = list_ref;
        let list_offset = list_offset;
        let scroll_travel = scroll_travel;
        let auto_scroll = auto_scroll;
        effect(move || {
            // One ticker per page: it runs only while a row is carried near
            // an edge, and moves the list a step at a time.
            let _ = set_interval(
                std::time::Duration::from_millis(DRAG_SCROLL_TICK_MS),
                move || {
                    if drag.with_untracked(Option::is_none) {
                        return;
                    }
                    let edge = auto_scroll.get_untracked();
                    if edge == 0.0 {
                        return;
                    }
                    let offset = (list_offset.get_untracked() + edge * DRAG_SCROLL_STEP).max(0.0);
                    list_offset.set(offset);
                    scroll_travel.update(|travel| *travel += edge * DRAG_SCROLL_STEP);
                    list_ref.scroll_to_offset(px(offset), false);
                },
            );
        });
    }
    effect(move || {
        if drag.with(Option::is_some) {
            menu::dismiss(menu);
        }
    });
    let sessions = host.sessions;
    let active = host.active;
    let query = signal(String::new());
    let select_mode = signal(false);
    let selected = signal(Vec::<String>::new());
    pager.on_sessions_back(move || {
        if !query.with_untracked(String::is_empty) {
            query.set(String::new());
            dismiss_keyboard();
        } else if select_mode.get_untracked() {
            select_mode.set(false);
            selected.set(Vec::new());
        } else {
            return false;
        }
        true
    });
    let locate = signal(None::<String>);
    let sidebar_sections = host.sidebar_sections;
    let sheets = SectionSheets {
        rename_folder: signal(None),
        rename_group: signal(None),
        name: signal(String::new()),
        archive: signal(None),
        members: signal(None),
        members_selected: signal(Vec::new()),
    };
    let group_open = signal(false);
    let group_name = signal(String::new());
    let runtime = host.runtime;
    // A session is written on the conversation page. Asking for one while
    // another is already being written points that one at the place asked
    // for, so the page is turned to it either way.
    let writing = host.new_session_open;
    let to_writing = move || {
        if writing.get_untracked() {
            pager.close();
        }
    };
    let on_new_session: Action = {
        let open = host.on_new_session.clone();
        Rc::new(move || {
            open();
            to_writing();
        })
    };
    let on_new_session_in: Rc<dyn Fn(&SidebarRowView)> = {
        let open = host.sidebar.on_new_session.clone();
        Rc::new(move |row| {
            open(row);
            to_writing();
        })
    };
    let on_switch_runtime = host.on_switch_runtime.clone();
    let on_open_settings = host.on_open_settings.clone();
    let connection = host.connection;
    let rows = computed({
        let rows = host.sidebar.rows.clone();
        move || rows(&query.get())
    });
    let matching = computed(move || {
        let query = query.get().trim().to_lowercase();
        sessions.with(|sessions| {
            sessions
                .iter()
                .filter(|session| {
                    query.is_empty()
                        || [
                            &session.title,
                            &session.project_name,
                            &session.workspace_label,
                            &session.agent,
                        ]
                        .iter()
                        .any(|value| value.to_lowercase().contains(&query))
                })
                .map(|session| session.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let can_group = computed(move || {
        let ids = selected.get();
        if ids.len() < 2 {
            return false;
        }
        sessions.with(|sessions| {
            let mut members = sessions.iter().filter(|session| ids.contains(&session.id));
            let Some(first) = members.next() else {
                return false;
            };
            members
                .filter(|member| {
                    member.project_id == first.project_id
                        && member.workspace_id == first.workspace_id
                })
                .count()
                + 1
                == ids.len()
        })
    });
    let all_selected = computed(move || {
        matching.with(|matching| {
            !matching.is_empty()
                && selected.with(|selected| matching.iter().all(|id| selected.contains(id)))
        })
    });
    let none_selected = move || selected.with(Vec::is_empty);
    let leave_selection = move || {
        select_mode.set(false);
        selected.set(Vec::new());
    };
    // The commands that act on the whole tree rather than on a row.
    let root_menu: Rc<dyn Fn(f32, MenuSide)> = Rc::new({
        let tree = host.sidebar.clone();
        let new_folder = host.on_open_folder.clone();
        move |anchor, side| {
            dismiss_keyboard();
            let reveal = tree.on_reveal_active.clone();
            let new_folder = new_folder.clone();
            menu.set(Some(
                Menu::at(anchor)
                    .side(side)
                    .entry(MenuEntry::new(
                        icon!("list-checks"),
                        copy::text("Select or reorder", "选择或排序", "選取或排序"),
                        move || select_mode.set(true),
                    ))
                    .entry(MenuEntry::new(
                        icon!("git-branch"),
                        if tree.detailed.get_untracked() {
                            copy::text("Hide worktrees", "隐藏 Worktree 层级", "隱藏 Worktree 層級")
                        } else {
                            copy::text("Show worktrees", "显示 Worktree 层级", "顯示 Worktree 層級")
                        },
                        {
                            let toggle = tree.on_toggle_hierarchy.clone();
                            move || toggle()
                        },
                    ))
                    .entry(MenuEntry::new(
                        icon!("chevrons-down-up"),
                        copy::text(
                            "Collapse or expand projects",
                            "折叠或展开项目",
                            "摺疊或展開專案",
                        ),
                        {
                            let collapse = tree.on_collapse_all.clone();
                            move || collapse()
                        },
                    ))
                    .entry(MenuEntry::new(
                        icon!("crosshair"),
                        copy::text("Locate current session", "定位当前会话", "定位目前工作階段"),
                        move || {
                            query.set(String::new());
                            locate.set(active.get_untracked());
                            reveal();
                        },
                    ))
                    .entry(MenuEntry::new(
                        icon!("folder-plus"),
                        copy::text("New folder", "新建文件夹", "新增資料夾"),
                        move || new_folder(),
                    )),
            ));
        }
    });
    let tree_actions = tree::TreeActions {
        toggle: host.sidebar.on_toggle_row.clone(),
        select: {
            let select = host.on_select.clone();
            let creating = host.new_session_open;
            let stop_creating = host.on_close_new_session.clone();
            Rc::new(move |row| {
                // Opening a session puts away the one that was being written.
                if creating.get_untracked() {
                    stop_creating();
                }
                select(&row.id)
            })
        },
        menu: {
            let host = host.clone();
            let on_new_session_in = on_new_session_in.clone();
            Rc::new(move |row, anchor| {
                dismiss_keyboard();
                let raised = match row.kind {
                    SidebarRowKind::Session => session_menu(
                        &host,
                        overlays,
                        &row.id,
                        &row.title,
                        (anchor, MenuSide::Leading),
                        Vec::new(),
                    ),
                    SidebarRowKind::Folder => {
                        folder_menu(&host, row, anchor, sheets, on_new_session_in.clone())
                    }
                    SidebarRowKind::Group => group_menu(&host, row, anchor, sheets),
                    SidebarRowKind::Project | SidebarRowKind::Workspace => {
                        scope_menu(&host, row, anchor, on_new_session_in.clone())
                    }
                    SidebarRowKind::EmptyWorkspace => return,
                };
                // The row is lifted with the menu, so the card stands against
                // the thing it is about rather than beside the finger.
                menu.set(Some(raised.from_row(menu_row(row, anchor))));
            })
        },
        move_row: host.sidebar.on_move_row.clone(),
        new_session: on_new_session_in,
        batch_scope: host.sidebar.batch_scope.clone(),
        // A carry whose row was recycled still has to land: the list reports
        // the lifted finger, and the row the drag started on is read from the
        // carry itself rather than from the element that began it.
        end_drag: {
            let rows = host.sidebar.rows.clone();
            let query = query;
            let move_row = host.sidebar.on_move_row.clone();
            let toggle = host.sidebar.on_toggle_row.clone();
            Rc::new(move || {
                let Some(dragged) = drag.get_untracked() else {
                    return;
                };
                let delta = dragged.delta + scroll_travel.get_untracked();
                let row = rows(&query.get_untracked())
                    .into_iter()
                    .find(|row| row.key() == dragged.key);
                drag.set(None);
                drag_collapse.set(None);
                auto_scroll.set(0.0);
                scroll_travel.set(0.0);
                let Some(row) = row else {
                    return;
                };
                if matches!(row.kind, SidebarRowKind::Folder | SidebarRowKind::Group)
                    && !row.collapsed
                {
                    toggle(&row);
                }
                move_row(&row, delta);
            })
        },
    };
    view()
        .w_full()
        .h_full()
        .relative()
        .flex_col()
        // The two side pages are one product surface with the conversation
        // between them, so this page takes the same tone as the pages it
        // slides over rather than the dimmed field grouped pages use.
        .bg(theme::block_page_adaptive())
        .test_id("sessions-page")
        .pt_safe_area()
        // The page carries the drawer's search field: the keyboard takes its
        // room out of the list rather than pushing the whole page up.
        .pb_keyboard_plus(px(theme::SPACING_SM))
        // The head: the product's own lockup, the tree's own menu, and the
        // way back to the conversation. The page is the phone's home surface,
        // so it wears the whole word at the header's own step rather than a
        // tile and a title; the runtime it is bound to lives in the foot.
        .child(
            view()
                .flex_shrink_0()
                .pl(px(theme::SPACING_LG))
                .pr(px(theme::SPACING_MD))
                .pt(px(theme::SPACING_SM))
                .pb(px(theme::SPACING_SM))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(crate::ui::brand::wordmark_at(theme::font_title())),
                )
                .child(
                    when(
                        move || select_mode.get(),
                        move || {
                            drawer_text_action(
                                None,
                                copy::text("Done", "完成", "完成"),
                                leave_selection,
                            )
                            .into_element()
                        },
                    )
                    .otherwise({
                        let root_menu = root_menu.clone();
                        move || {
                            let root_menu = root_menu.clone();
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_SM))
                                .child(surface::ghost_button(
                                    icon!("ellipsis"),
                                    copy::text("Sidebar options", "侧栏选项", "側欄選項"),
                                    move || {
                                        root_menu(
                                            safe_area().top.0 + theme::SPACING_XL,
                                            MenuSide::Trailing,
                                        )
                                    },
                                ))
                                // The page fills the screen, so the way back
                                // is a control of its own, where the
                                // conversation lies.
                                .child(surface::round_button(
                                    icon!("chevron-right"),
                                    copy::text("Back to session", "返回会话", "返回工作階段"),
                                    move || pager.close(),
                                ))
                                .into_element()
                        }
                    }),
                ),
        )
        // Search, always at hand, and the one thing the page is for beside it.
        .child(
            view()
                .flex_shrink_0()
                .px(px(theme::SIDEBAR_LIST_PADDING))
                .py(px(6.0))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    surface::field_pill()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(surface::field_glyph(icon!("search")))
                        .child(
                            field::text_field_bare(
                                query,
                                copy::text("Search sessions", "搜索会话", "搜尋工作階段"),
                            )
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full(),
                        )
                        .child(when(
                            move || !query.with(String::is_empty),
                            move || {
                                surface::field_clear(
                                    copy::text("Clear search", "清除搜索", "清除搜尋"),
                                    move || {
                                        query.set(String::new());
                                        dismiss_keyboard();
                                    },
                                )
                            },
                        )),
                )
                .child({
                    let on_new_session = on_new_session.clone();
                    // A speech bubble taking a plus, not a bare plus: the
                    // button makes a conversation, and the drawer's other plus
                    // makes a project — two marks that said the same thing sat
                    // three rows apart. The mark is the button: drawn at the
                    // size of the round controls beside it, with no disc behind
                    // it and in a softer ink so the bubble and plus sit gently
                    // next to the search pill. The ink is the duotone one: the
                    // bubble carries its own quarter-opacity wash, which the
                    // light page shows as a grey plate under a glyph that is
                    // meant to be quiet.
                    surface::bare_icon_button(
                        icon!("chat-alt-add"),
                        copy::text("New session", "新建会话", "新增工作階段"),
                        move || on_new_session(),
                        theme::ROUND_CONTROL,
                        theme::sidebar_duotone_ink,
                    )
                }),
        )
        // The tree's name, and the way to add to its top level.
        .child(
            view()
                .flex_shrink_0()
                .pl(px(theme::SPACING_XL + theme::SPACING_XS))
                .pr(px(theme::SIDEBAR_LIST_PADDING))
                .pt(px(theme::SPACING_SM))
                .pb(px(2.0))
                .flex_row()
                .items_center()
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .h(px(32.0))
                        .justify_center()
                        .on_long_press({
                            let root_menu = root_menu.clone();
                            move |event: &PressEvent| {
                                crate::motion::tick();
                                root_menu(event.page_position.y.0, MenuSide::Leading)
                            }
                        })
                        .child(
                            text(copy::text("Projects", "项目", "專案"))
                                .truncate()
                                .text_size(px(theme::font_caption()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::sidebar_text_muted),
                        ),
                )
                .child({
                    let new_project = host.on_new_project.clone();
                    drawer_text_action(
                        Some(icon!("plus")),
                        copy::text("New project", "新建项目", "新增專案"),
                        move || new_project(),
                    )
                    .accessibility_label(copy::text("New project", "新建项目", "新增專案"))
                }),
        )
        // The rows run out under the foot rather than being cut by a rule, the
        // way the transcript runs out under the conversation's header.
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .flex_col()
                .relative()
                .on_layout(move |bounds: &Bounds<Points>| {
                    list.set(Some((bounds.origin.y.0, bounds.size.height.0)));
                })
                .child(tree::list(
                    tree::TreeState {
                        pager,
                        rows,
                        query,
                        select_mode,
                        selected,
                        locate,
                        drag,
                        drag_collapse,
                        menu: overlays.menu,
                        shifts,
                        list,
                        list_offset,
                        scroll_travel,
                        auto_scroll,
                        list_ref,
                    },
                    tree_actions,
                ))
                .child(edge_fade_between(
                    false,
                    theme::SPACING_LG,
                    theme::page_adaptive,
                    theme::page_clear_adaptive,
                )),
        )
        // The foot: what the page is bound to, or what to do with the sessions
        // being selected.
        .child(
            when(move || select_mode.get(), {
                let on_pin_sessions = host.on_pin_sessions.clone();
                let on_fork_sessions = host.on_fork_sessions.clone();
                let on_archive_sessions = host.on_archive_sessions.clone();
                let on_delete_sessions = host.on_delete_sessions.clone();
                move || {
                    let on_pin_sessions = on_pin_sessions.clone();
                    let on_fork_sessions = on_fork_sessions.clone();
                    let on_archive_sessions = on_archive_sessions.clone();
                    let on_delete_sessions = on_delete_sessions.clone();
                    view()
                        .w_full()
                        .flex_shrink_0()
                        .flex_col()
                        .px(px(theme::SIDEBAR_LIST_PADDING))
                        .pt(px(theme::SPACING_XS))
                        .child(
                            view()
                                .w_full()
                                .pl(px(theme::SPACING_SM))
                                .flex_row()
                                .items_center()
                                .child(
                                    text(move || {
                                        format!(
                                            "{} {}",
                                            selected.with(|selected| selected.len()),
                                            copy::text("selected", "已选择", "已選取")
                                        )
                                    })
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme::font_detail()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::foreground),
                                )
                                .child(
                                    drawer_text_action(
                                        None,
                                        move || {
                                            if all_selected.get() {
                                                copy::text("Deselect all", "取消全选", "取消全選")
                                            } else {
                                                copy::text("Select all", "全选", "全選")
                                            }
                                        },
                                        move || {
                                            tree::toggle_batch(selected, &matching.get_untracked())
                                        },
                                    )
                                    .disabled(move || matching.with(Vec::is_empty)),
                                ),
                        )
                        .child(
                            view()
                                .w_full()
                                .flex_row()
                                .items_center()
                                .child(
                                    drawer_batch_action(
                                        icon!("pin"),
                                        copy::text("Pin", "置顶", "釘選"),
                                        copy::text(
                                            "Pin selected sessions",
                                            "置顶选中会话",
                                            "釘選已選工作階段",
                                        ),
                                        false,
                                        move || on_pin_sessions(&selected.get_untracked()),
                                    )
                                    .disabled(none_selected),
                                )
                                .child(
                                    drawer_batch_action(
                                        icon!("git-branch"),
                                        copy::text("Fork", "分叉", "分叉"),
                                        copy::text(
                                            "Fork selected sessions",
                                            "分叉选中会话",
                                            "分叉已選工作階段",
                                        ),
                                        false,
                                        move || on_fork_sessions(&selected.get_untracked()),
                                    )
                                    .disabled(none_selected),
                                )
                                .child(
                                    drawer_batch_action(
                                        icon!("layers"),
                                        copy::text("Group", "分组", "分組"),
                                        copy::text("Group", "分组", "分組"),
                                        false,
                                        move || group_open.set(true),
                                    )
                                    .disabled(move || !can_group.get()),
                                )
                                .child(
                                    drawer_batch_action(
                                        icon!("package"),
                                        copy::text("Archive", "归档", "封存"),
                                        copy::text("Archive", "归档", "封存"),
                                        false,
                                        move || {
                                            let ids = selected.get_untracked();
                                            if !ids.is_empty() {
                                                on_archive_sessions(&ids);
                                            }
                                            selected.set(Vec::new());
                                        },
                                    )
                                    .disabled(none_selected),
                                )
                                .child(
                                    drawer_batch_action(
                                        icon!("trash-2"),
                                        copy::text("Delete", "删除", "刪除"),
                                        copy::text("Delete", "删除", "刪除"),
                                        true,
                                        move || {
                                            let ids = selected.get_untracked();
                                            if ids.is_empty() {
                                                return;
                                            }
                                            let on_delete_sessions = on_delete_sessions.clone();
                                            spawn(async move {
                                                let answer = nsh_base::alert_dialog()
                                                    .title(copy::text(
                                                        "Delete selected sessions?",
                                                        "删除所选会话？",
                                                        "刪除所選工作階段？",
                                                    ))
                                                    .description(copy::text(
                                                        "Their conversation history will be permanently deleted.",
                                                        "对话记录将被永久删除。",
                                                        "對話記錄將永久刪除。",
                                                    ))
                                                    .cancel(
                                                        "cancel",
                                                        copy::text("Cancel", "取消", "取消"),
                                                    )
                                                    .destructive(
                                                        "delete",
                                                        copy::text("Delete", "删除", "刪除"),
                                                    )
                                                    .show()
                                                    .await;
                                                if matches!(answer, Ok(answer) if answer.action.as_deref() == Some("delete"))
                                                {
                                                    on_delete_sessions(&ids);
                                                    selected.update(|selected| {
                                                        selected.retain(|id| !ids.contains(id))
                                                    });
                                                }
                                            });
                                        },
                                    )
                                    .disabled(none_selected),
                                ),
                        )
                }
            })
            .otherwise({
                let on_open_settings = on_open_settings.clone();
                let on_switch = on_switch_runtime.clone();
                move || {
                    let on_open_settings = on_open_settings.clone();
                    let on_switch = on_switch.clone();
                    view()
                        .w_full()
                        .flex_shrink_0()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .px(px(theme::SIDEBAR_LIST_PADDING))
                        .pt(px(theme::SPACING_XS))
                        // Which runtime this page is bound to. The dot is the
                        // only place the link's state is drawn; the words stay
                        // in the control's name. The row is the way into the
                        // runtime list, so it keeps the remaining width.
                        .child(
                            button()
                                .flex_1()
                                .min_w(px(0.0))
                                .h(px(theme::TOUCH_TARGET))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_SM))
                                .px(px(theme::SPACING_SM))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .accessibility_label(copy::text(
                                    "Switch runtime",
                                    "切换运行时",
                                    "切換執行環境",
                                ))
                                .on_press(move || {
                                    pager.close();
                                    on_switch()
                                })
                                .child(
                                    view()
                                        .size(px(theme::ICON_STATUS))
                                        .flex_shrink_0()
                                        .rounded_full()
                                        .bg(move || match connection.get() {
                                            ConnectionState::Online => theme::accent_success(),
                                            ConnectionState::Connecting => theme::accent_yellow(),
                                            ConnectionState::Offline => theme::text_muted(),
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
                                    .flex_shrink(1.0)
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme::font_detail()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::sidebar_text_secondary),
                                ),
                        )
                        // The way into settings, as the round block the
                        // page's own controls wear: the icon alone.
                        .child(
                            button()
                                .size(px(theme::TOUCH_TARGET))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(theme::fill)
                                .items_center()
                                .justify_center()
                                .active(|style| style.opacity(0.6))
                                .accessibility_label(copy::text("Settings", "设置", "設定"))
                                .on_press(move || {
                                    pager.close();
                                    on_open_settings()
                                })
                                .child(
                                    svg(icon!("settings"))
                                        .size(px(theme::ICON_MD))
                                        .text_color(theme::text_secondary),
                                ),
                        )
                }
            }),
        )
        // The row a finger is carrying, at its own size: the list opens the
        // slot it would land in beneath it.
        .child(dynamic(move || match drag.get() {
            Some(dragged) => {
                let card = menu::MenuRow {
                    key: dragged.key.clone(),
                    top: dragged.top,
                    height: theme::sidebar_row_height(),
                    mark: dragged.mark.clone(),
                    title: dragged.title.clone(),
                    title_size: dragged.title_size,
                    trailing: dragged.trailing.clone(),
                };
                view()
                    .absolute()
                    .inset(px(0.0))
                    .pointer_events(PointerEvents::None)
                    .child(menu::carried_row(&card, dragged.top + dragged.delta))
                    .into_element()
            }
            None => view().absolute().into_element(),
        }))
        .child(name_sheet(
            move || sheets.rename_folder.with(Option::is_some),
            copy::text("Rename folder", "重命名文件夹", "重新命名資料夾"),
            copy::text("Folder name", "文件夹名称", "資料夾名稱"),
            sheets.name,
            copy::text("Save", "保存", "儲存"),
            || false,
            {
                let rename = host.on_rename_folder.clone();
                move || {
                    if let Some(id) = sheets.rename_folder.get_untracked() {
                        rename(&id, sheets.name.get_untracked().trim());
                        sheets.rename_folder.set(None);
                    }
                }
            },
            move || sheets.rename_folder.set(None),
        ))
        .child(name_sheet(
            move || sheets.rename_group.with(Option::is_some),
            copy::text("Rename group", "重命名分组", "重新命名分組"),
            copy::text("Group name", "分组名称", "分組名稱"),
            sheets.name,
            copy::text("Save", "保存", "儲存"),
            || false,
            {
                let rename = host.on_rename_group.clone();
                move || {
                    if let Some(id) = sheets.rename_group.get_untracked() {
                        rename(&id, sheets.name.get_untracked().trim());
                        sheets.rename_group.set(None);
                    }
                }
            },
            move || sheets.rename_group.set(None),
        ))
        .child(group_members_sheet(
            sheets.members,
            sheets.members_selected,
            sessions,
            host.on_set_group_members.clone(),
        ))
        .child(name_sheet(
            move || group_open.get(),
            copy::text("New group", "新建分组", "新增分組"),
            copy::text("Group name", "分组名称", "分組名稱"),
            group_name,
            copy::text("Create", "创建", "建立"),
            move || selected.with(Vec::is_empty),
            {
                let create = host.on_create_group.clone();
                move || {
                    let ids = selected.get_untracked();
                    create(group_name.get_untracked().trim(), &ids);
                    group_name.set(String::new());
                    group_open.set(false);
                }
            },
            move || group_open.set(false),
        ))
        .child(folder_archive_sheet(
            sheets.archive,
            sidebar_sections,
            host.on_set_folder_auto_archive.clone(),
        ))
        .child(name_sheet(
            {
                let open = host.folder_open;
                move || open.get()
            },
            copy::text("New folder", "新建文件夹", "新增資料夾"),
            copy::text("Folder name", "文件夹名称", "資料夾名稱"),
            host.folder_name,
            copy::text("Create", "创建", "建立"),
            || false,
            {
                let create = host.on_create_folder.clone();
                move || create()
            },
            {
                let close = host.on_close_folder.clone();
                move || close()
            },
        ))
}

fn group_members_sheet(
    group: Signal<Option<String>>,
    members: Signal<Vec<String>>,
    sessions: Signal<Vec<SessionSummary>>,
    on_save: CreateGroupAction,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
    bottom_sheet(
        move || group.with(Option::is_some),
        move || group.set(None),
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.75))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Group members", "分组成员", "分組成員"),
                move || group.set(None),
            ))
            .child(
                scroll().flex_shrink(1.0).min_h(px(0.0)).child(
                    view()
                        .w_full()
                        .px(px(theme::SPACING_MD))
                        .pt(px(theme::SPACING_XS))
                        .child(sheet_card().child(each(
                            move || sessions.get(),
                            |session| session.id.clone(),
                            move |session, _| {
                                let id = session.with_untracked(|session| session.id.clone());
                                let ticked = {
                                    let id = id.clone();
                                    computed(move || members.with(|members| members.contains(&id)))
                                };
                                button()
                                    .on_press(move || {
                                        members.update(|members| {
                                            if let Some(index) =
                                                members.iter().position(|member| member == &id)
                                            {
                                                members.remove(index);
                                            } else {
                                                members.push(id.clone());
                                            }
                                        });
                                    })
                                    .w_full()
                                    .min_h(px(theme::ROW_HEIGHT))
                                    .px(px(surface::ROW_INSET))
                                    .flex_row()
                                    .items_center()
                                    .gap(px(theme::SPACING_MD))
                                    .border_b_1()
                                    .border_b_color(theme::hairline)
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .child(surface::check_mark(move || {
                                        ticked.get().then_some(true)
                                    }))
                                    .child(
                                        text(move || session.with(|session| session.title.clone()))
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .truncate()
                                            .text_size(px(theme::font_body()))
                                            .text_color(theme::foreground),
                                    )
                            },
                        ))),
                ),
            )
            .child(
                sheet_body().pt(px(theme::SPACING_MD)).child(
                    action::solid(copy::text("Save", "保存", "儲存"), move || {
                        if let Some(group_id) = group.get_untracked() {
                            on_save(&group_id, &members.get_untracked());
                            group.set(None);
                        }
                    })
                    .w_full(),
                ),
            ),
    )
}

/// The schedules a folder can archive on. `None` turns the window off; every
/// day the desktop accepts is offered, because a phone sheet can list them and
/// a value it cannot set would make the row's current window unchangeable.
fn folder_archive_sheet(
    open: Signal<Option<String>>,
    sections: Signal<Vec<SidebarSectionView>>,
    on_set: FolderArchiveAction,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
    // The desktop's `SIDEBAR_AUTO_ARCHIVE_MAX_DAYS`; a value it accepts has to
    // be settable here, or the sheet would show a window the row cannot use.
    const MAX_DAYS: u8 = 14;
    let current = computed(move || {
        let id = open.get()?;
        sections.with(|sections| {
            sections
                .iter()
                .find(|section| section.kind == SidebarSectionKind::Folder && section.id == id)
                .map(|section| section.auto_archive_after_days)
        })
    });
    let mut card = sheet_card();
    for days in (0..=MAX_DAYS).map(|days| (days > 0).then_some(days)) {
        let on_set = on_set.clone();
        card = card.child(
            button()
                .on_press(move || {
                    let Some(id) = open.get_untracked() else {
                        return;
                    };
                    open.set(None);
                    on_set(&id, days);
                })
                .w_full()
                .min_h(px(theme::MENU_ROW_HEIGHT))
                .px(px(theme::SPACING_LG))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_MD))
                .when(days.is_some(), |row| {
                    row.border_t_1().border_t_color(theme::hairline)
                })
                .active(|style| style.bg(theme::row_pressed_bg()))
                .child(
                    text(folder_archive_label(days))
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(theme::font_body()))
                        .text_color(theme::foreground),
                )
                .child(when(
                    move || current.get() == Some(days),
                    || {
                        svg(icon!("check"))
                            .size(px(theme::ICON_MD))
                            .flex_shrink_0()
                            .text_color(theme::foreground)
                    },
                )),
        );
    }
    bottom_sheet(
        move || open.with(Option::is_some),
        move || open.set(None),
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.75))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Scheduled archive", "定时归档", "定時封存"),
                move || open.set(None),
            ))
            .child(
                scroll()
                    .flex_shrink(1.0)
                    .min_h(px(0.0))
                    .child(sheet_body().child(card)),
            ),
    )
}

/// The entry that adds a session to one group, named after the group the way
/// the desktop names it.
fn add_to_group_label(name: &str) -> String {
    match copy::language() {
        copy::Language::SimplifiedChinese => format!("加入「{name}」"),
        copy::Language::TraditionalChinese => format!("加入「{name}」"),
        _ => format!("Add to {name}"),
    }
}

/// The entry that starts a session inside one folder, in the desktop's words.
fn new_chat_in_folder_label(name: &str) -> String {
    match copy::language() {
        copy::Language::SimplifiedChinese => {
            format!("在「{name}」文件夹中新建会话")
        }
        copy::Language::TraditionalChinese => {
            format!("在「{name}」資料夾中建立會話")
        }
        _ => format!("New chat in folder {name}"),
    }
}

/// The window a folder archives on, said the way the desktop says it.
fn folder_archive_label(days: Option<u8>) -> String {
    let Some(days) = days else {
        return copy::text("Off", "关闭", "關閉").to_owned();
    };
    match copy::language() {
        copy::Language::SimplifiedChinese => format!("{days}天后"),
        copy::Language::TraditionalChinese => format!("{days}天後"),
        _ if days == 1 => "After 1 day".to_owned(),
        _ => format!("After {days} days"),
    }
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

#[cfg(test)]
mod tests {
    /// The composer's chip wears the selected model's own brand mark.
    ///
    /// It used to fall back to the generic sparkle for every model, because
    /// the mark was resolved from a summary that arrived with the catalogue
    /// rather than with the selection. The resolution itself is what this
    /// pins: a model id the brand tables know must answer that brand's own
    /// mark, not the fallback.
    #[test]
    fn a_known_model_resolves_its_brand_mark() {
        let fallback = icon!("sparkles");
        for model in [
            "claude-sonnet-4-5",
            "gpt-5",
            "gemini-2.5-pro",
            "qwen3-coder",
            "deepseek-chat",
        ] {
            let mark = super::runtime_mark_svg(model, "");
            assert_ne!(
                mark, fallback,
                "`{model}` draws its own brand mark, not the sparkle"
            );
        }
    }

    /// An unknown model still draws something, and the Agent answers next.
    ///
    /// A model the tables do not know leaves the Agent's mark; an Agent they
    /// do not know either leaves the sparkle, so the chip is never blank.
    #[test]
    fn an_unknown_model_falls_back_to_the_agent_then_the_sparkle() {
        let sparkle = icon!("sparkles");
        let unknown_model = super::runtime_mark_svg("no-such-model-xyz", "");
        assert_eq!(unknown_model, sparkle, "nothing left to draw");

        let agent = super::runtime_mark_svg("no-such-model-xyz", "claude");
        assert_ne!(agent, sparkle, "the Agent's own mark answers second");
    }
}

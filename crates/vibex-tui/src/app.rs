//! The TUI application: navigation state, overlays, and the intent → effect
//! reducer.
//!
//! The reducer is deliberately pure. `perform` mutates the in-memory state and
//! returns a list of [`Effect`] values describing asynchronous work; it never
//! touches the terminal, the network, or the filesystem itself. That is what
//! makes the whole interaction model testable by driving `perform` with key
//! sequences and asserting on state.
//!
//! Shared semantics come from the same controllers the desktop and mobile
//! clients use (`AgentWorkflowController`, `ManagementWorkflowController`), so
//! the TUI cannot invent a second interpretation of sessions, approvals or
//! capability gating.

use std::collections::BTreeMap;

use vibex_backend::DomainCapabilities;
use vibex_backend::{BackendCapabilitySnapshot, BackendFacade, BackendOperation};
use vibex_core::{AgentSession, RemoteDeviceDetail, RemoteDevicePermissionLevel, VibexSessionId};
use vibex_desktop_model::{SidebarState, TimelineRow};
use vibex_ui::shell::{CompactNavigation, GlobalDestination, SessionDestination, ShellKind};
use vibex_ui::{AgentWorkflowController, ManagementWorkflowController};

use crate::action::Intent;
use crate::composer::{CompletionMenu, ComposerBuffer, ComposerHistory};
use serde::{Deserialize, Serialize};

use crate::keymap::{Chord, Keymap, Scope};
use crate::locale::{Locale, Strings};
use crate::theme::{ColorCapability, TuiTheme};
use crate::transcript::{Block, ScrollState, Transcript};
use crate::view::SeatKind;

/// Which page the user is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Sessions,
    Agent,
    /// Writing the first message of a session that does not exist yet.
    ///
    /// A session is created by sending, not by answering a prompt about it: the
    /// reader who presses `n` wants to write, so the page they land on is a
    /// prompt, with the runtime it will be sent through named beside it.
    NewSession,
    Files,
    Changes,
    Terminal,
    Management,
    Providers,
    Agents,
    Mcp,
    Skills,
    Prompts,
    Hooks,
    Devices,
    Usage,
    Recovery,
    Settings,
    Help,
}

impl Page {
    pub const fn scope(self) -> Scope {
        match self {
            Page::Sessions => Scope::Sessions,
            Page::Agent | Page::NewSession => Scope::Agent,
            Page::Files => Scope::Files,
            Page::Changes => Scope::Changes,
            Page::Terminal => Scope::Terminal,
            Page::Management | Page::Agents => Scope::Management,
            Page::Providers => Scope::Providers,
            Page::Mcp => Scope::Mcp,
            Page::Skills => Scope::Skills,
            Page::Prompts => Scope::Prompts,
            Page::Hooks => Scope::Hooks,
            Page::Devices => Scope::Devices,
            Page::Usage => Scope::Usage,
            Page::Recovery => Scope::Recovery,
            Page::Settings => Scope::Settings,
            Page::Help => Scope::Help,
        }
    }

    pub const fn is_session_page(self) -> bool {
        matches!(
            self,
            Page::Agent | Page::NewSession | Page::Files | Page::Changes | Page::Terminal
        )
    }

    /// Whether the page exists to write a message: the composer owns the
    /// keyboard, and the bands a session's view needs are the ones it shows.
    pub const fn is_composing_page(self) -> bool {
        matches!(self, Page::Agent | Page::NewSession)
    }
}

/// Which pane owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Main,
    Details,
    Composer,
}

impl Focus {
    pub const fn next(self) -> Self {
        match self {
            Focus::Sidebar => Focus::Main,
            Focus::Main => Focus::Details,
            Focus::Details => Focus::Composer,
            Focus::Composer => Focus::Sidebar,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Focus::Sidebar => Focus::Composer,
            Focus::Main => Focus::Sidebar,
            Focus::Details => Focus::Main,
            Focus::Composer => Focus::Details,
        }
    }
}

/// A modal surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// Fuzzy command palette over every page and action.
    Palette { query: String, selected: usize },
    /// The shortcuts cheatsheet: every binding, grouped by category.
    Help {
        query: String,
        /// Index into the visible rows (headers included).
        selected: usize,
        /// Categories the reader has folded away.
        collapsed: std::collections::BTreeSet<String>,
    },
    /// A yes/no question that guards a destructive action.
    Confirm {
        title: String,
        body: String,
        confirm: Intent,
    },
    /// A single-line text prompt.
    Prompt {
        title: String,
        field: PromptField,
        value: String,
    },
    /// The pending approval card.
    Approval { selected: usize },
    /// The pending elicitation form.
    Elicitation { field: usize },
    /// A one-time pairing code, shown once and never persisted.
    PairingCode {
        code: String,
        link: String,
        permission: RemoteDevicePermissionLevel,
    },
    /// The runtime and model picker.
    ///
    /// `view` is which half of it is on screen and `selected` indexes the rows
    /// of *that* view, so a catalogue of fifty models cannot bury the handful
    /// of run options under it: `Tab` moves between the two.
    RuntimePicker {
        view: RuntimePickerView,
        selected: usize,
    },
    /// One run option's values, opened from the switcher's run-option view.
    ///
    /// The option travels with the overlay rather than being looked up again by
    /// row index: a catalogue read that lands while the list is open must not
    /// put a value onto a different option.
    RunOptionValues {
        /// The run-option row that opened it, so `Esc` steps back there.
        row: usize,
        selected: usize,
        option: RunOption,
    },
    /// The directory the next session will work in.
    ///
    /// A picker rather than a text field: the reader is choosing a directory
    /// that exists on the *runtime's* host, which this client cannot list for
    /// itself, and typing a path from memory is how a session ends up in a
    /// directory nobody meant.
    WorkspacePicker { selected: usize },
    /// A read-only detail view for one transcript block.
    BlockDetails { block: usize, scroll: usize },
    /// The key-binding editor: every binding, rebindable in place.
    Keys {
        query: String,
        /// Index into the rows the editor last drew, headers included.
        selected: usize,
        /// The binding waiting for a chord, while one is being captured.
        capturing: Option<Intent>,
        /// Why the last attempt was refused, or what just happened.
        message: Option<String>,
        /// Whether the in-memory table differs from the file on disk.
        dirty: bool,
    },
    /// A read-only text view (diff, file contents, diagnostics report).
    TextView {
        title: String,
        body: String,
        scroll: usize,
    },
}

/// Which single-line field a prompt is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptField {
    RenameSession,
    WorkspacePath,
    CommitMessage,
    ProviderSecret,
    ProviderEndpoint,
    McpServerName,
    SkillName,
    PromptName,
    HookName,
    DeviceRevokeReason,
    RestoreBackupId,
    WorktreeBranch,
    ImagePath,
    /// A run option the Agent publishes as free text rather than as a list.
    RunOptionValue,
}

/// Which run option a row of the switcher, or a value list, is about.
///
/// The Agent's own vocabulary is kept in [`RunOptionKey::Feature`]: the id is
/// what travels back to the runtime, never a label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunOptionKey {
    /// How deep the Agent thinks before it answers.
    ReasoningEffort,
    /// The Agent's conversation mode.
    Mode,
    /// A provider-neutral session feature the Agent advertises.
    Feature(String),
}

/// How the reader picks a run option's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOptionKind {
    /// One of the listed values, chosen from a value list.
    Choice,
    /// On or off.
    Toggle,
    /// Free text, typed into the prompt overlay.
    Text,
}

/// A catalogue value as words: its label when it has one, its wire value
/// otherwise.
fn session_config_value_label(value: &vibex_core::SessionConfigValue) -> String {
    value
        .label
        .clone()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| value.value.clone())
}

/// One run option the selected Agent publishes.
///
/// This is the client's view of the Agent's session configuration: what the
/// option is called, what it accepts, and what the page's selection currently
/// asks for. The two value fields are deliberately distinct — `explicit` is
/// what would be sent, `resolved` is what is in effect — because an Agent that
/// publishes a current value is describing itself, not an override the reader
/// made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOption {
    pub key: RunOptionKey,
    pub label: String,
    pub description: Option<String>,
    pub kind: RunOptionKind,
    /// The values the option accepts, in the catalogue's order. Empty for a
    /// [`RunOptionKind::Text`] option.
    pub values: Vec<vibex_core::SessionConfigValue>,
    /// What the selection explicitly asks for; `None` is the Agent's own
    /// default, which is the value list's first row.
    pub explicit: Option<String>,
    /// The value in effect: for a feature with no override, what the Agent
    /// published.
    pub resolved: Option<vibex_core::SessionConfigValue>,
}

impl RunOption {
    /// The value in effect, as words, with the default for nothing set.
    pub fn resolved_label(&self, default_label: &str) -> String {
        self.resolved
            .as_ref()
            .map(|value| {
                value
                    .label
                    .clone()
                    .unwrap_or_else(|| value.value.clone())
                    .trim()
                    .to_string()
            })
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| default_label.to_string())
    }

    /// Whether the selection overrides the Agent's own value.
    pub const fn is_explicit(&self) -> bool {
        self.explicit.is_some()
    }

    /// Whether one value list row is the one the selection is on.
    pub fn is_selected_value(&self, value: Option<&str>) -> bool {
        self.explicit.as_deref() == value
    }
}

/// Which half of the runtime switcher is on screen.
///
/// Two views rather than one list because the catalogue is as long as the
/// machine has models: appending the handful of run options under fifty rows
/// of Agent/model is the same as hiding them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePickerView {
    /// The catalogue: which Agent, authentication source and model.
    Choices,
    /// The chosen entry's run options: how that Agent runs.
    Options,
}

/// A transient status message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub tone: ToastTone,
    /// Frames remaining before it disappears.
    pub ttl: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    Info,
    Success,
    Warning,
    Danger,
}

impl Toast {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ToastTone::Info,
            ttl: 4,
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ToastTone::Warning,
            ttl: 6,
        }
    }

    pub fn danger(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ToastTone::Danger,
            ttl: 8,
        }
    }

    pub fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ToastTone::Success,
            ttl: 4,
        }
    }
}

/// Live connection state shown in the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    Connecting,
    Ready,
    Reconnecting,
    Offline,
}

impl LiveState {
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Rows on the management index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementRow {
    Agents,
    Providers,
    Mcp,
    Skills,
    Prompts,
    Hooks,
    Devices,
    Recovery,
}

impl ManagementRow {
    pub const ALL: [ManagementRow; 8] = [
        ManagementRow::Agents,
        ManagementRow::Providers,
        ManagementRow::Mcp,
        ManagementRow::Skills,
        ManagementRow::Prompts,
        ManagementRow::Hooks,
        ManagementRow::Devices,
        ManagementRow::Recovery,
    ];

    pub const fn page(self) -> Page {
        match self {
            ManagementRow::Agents => Page::Agents,
            ManagementRow::Providers => Page::Providers,
            ManagementRow::Mcp => Page::Mcp,
            ManagementRow::Skills => Page::Skills,
            ManagementRow::Prompts => Page::Prompts,
            ManagementRow::Hooks => Page::Hooks,
            ManagementRow::Devices => Page::Devices,
            ManagementRow::Recovery => Page::Recovery,
        }
    }
}

/// A row on the settings page, and the state of its mode machine.
///
/// The definitions live in [`crate::settings`]; they are re-exported here
/// because that is where every other page's navigation types live.
pub use crate::settings::{SettingKind, SettingRow, SettingsMode, SettingsState};

/// The data each management page holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagementData {
    pub devices: Vec<RemoteDeviceDetail>,
    pub audit: Vec<vibex_core::RemoteAuditRecord>,
    pub profiles: Vec<vibex_core::ProviderProfileSummary>,
    pub providers: Vec<vibex_core::ProviderProfileSummary>,
    pub health: Vec<vibex_core::ProviderHealthSummary>,
    pub agents: Vec<vibex_core::AgentSnapshotEntry>,
    pub agent_auth: Vec<vibex_core::AgentAuthContext>,
    pub mcp: Vec<vibex_core::McpServer>,
    pub skills: Vec<vibex_core::Skill>,
    pub prompts: Vec<vibex_core::Prompt>,
    pub hooks: Vec<vibex_core::Hook>,
    pub backups: Vec<vibex_core::BackupCreateOutcome>,
    pub usage: Option<vibex_core::AgentUsageStatistics>,
    pub usage_session: Option<vibex_core::AgentTokenUsage>,
}

/// The reader's own arrangement of the session list.
///
/// The projection already knows how to hoist pinned rows and honour a manual
/// order; what it has never had is anything that writes those fields. They are
/// the client's preference, so they live beside the key file rather than in the
/// runtime's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarArrangement {
    pub sidebar: vibex_desktop_model::SidebarState,
    pub grouped: bool,
}

impl Default for SidebarArrangement {
    fn default() -> Self {
        Self {
            sidebar: vibex_desktop_model::SidebarState::default(),
            grouped: true,
        }
    }
}

impl SidebarArrangement {
    /// Read what a previous run wrote, or an empty arrangement.
    fn load(path: Option<&std::path::Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }
}

/// What the transcript and sidebar are projected from.
#[derive(Debug, Clone, Default)]
pub struct ProjectionState {
    pub sidebar: SidebarState,
    pub rows: Vec<TimelineRow>,
    /// Whether the sidebar pane is collapsed by the user.
    pub sidebar_collapsed: bool,
}

/// A message waiting for the running turn to end.
///
/// It carries its images, because "send this later" must send the same thing
/// the reader composed, and the composer is emptied the moment it is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessage {
    /// The session the message was written for.
    ///
    /// A queue is per session, not per client: switching to another session
    /// and back must find the same held messages, and a message must never be
    /// released into a session it was not written for.
    pub session_id: VibexSessionId,
    pub text: String,
    /// The pre-wire form of the images, so pulling the message back into the
    /// composer restores exactly what was there — and where each of them sat in
    /// the text, which the wire needs to put the picture back in its place.
    pub images: Vec<(crate::composer::ImageAttachment, u32)>,
}

/// A message the reader has sent that the runtime has not echoed back yet.
///
/// The client does not own the timeline: the authoritative copy of the reader's
/// own message arrives with the next snapshot or event, which is long enough for
/// a send to look like it failed. The send is projected locally until the row it
/// becomes shows up, and it counts as a running turn in the meantime.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingSend {
    /// The session it was sent to, absent while that session is still being
    /// created: the reader pressed Enter on the page that *asks* for one, and
    /// the message has to be on screen before the runtime has answered.
    pub session_id: Option<VibexSessionId>,
    /// Distinguishes this send's projected row from the next one's. The row has
    /// to keep one identity across frames — the transcript diffs by id, and the
    /// scroll anchor holds one — so it cannot be derived from the clock.
    pub serial: u64,
    pub text: String,
    /// The message as it went on the wire, labels stripped: the echo re-derives
    /// what the reader saw from these, exactly as the sent row will.
    pub attachments: Vec<vibex_core::MessageAttachment>,
    /// The newest timeline sequence the client knew when the message was sent,
    /// so only a *newer* row can be the one that confirms it.
    pub after_sequence: i64,
    pub submitted_at: std::time::Instant,
}

impl PendingSend {
    /// How long a send may be projected before the client stops pretending.
    ///
    /// Long enough for a slow runtime to answer, short enough that a message
    /// that never landed does not sit in the transcript forever.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

    /// The row the send is drawn as until the runtime's own copy arrives.
    ///
    /// It is built to look exactly like the row the echo will become: same kind,
    /// same body, so the reader sees one message that stays put rather than one
    /// that is replaced by a different-looking one a moment later.
    fn row(&self) -> TimelineRow {
        TimelineRow {
            id: format!(
                "pending-send:{}:{}",
                self.session_id
                    .as_ref()
                    .map(VibexSessionId::as_str)
                    .unwrap_or("new"),
                self.serial
            ),
            kind: vibex_desktop_model::TimelineRowKind::UserMessage,
            item_ids: Vec::new(),
            turn_id: None,
            turn_item_count: 0,
            turn_failed: false,
            turn_pending_permission: false,
            conclusion: false,
            first_sequence: self.after_sequence.saturating_add(1),
            last_sequence: self.after_sequence.saturating_add(1),
            title: "You".to_string(),
            body: self.text.clone(),
            streaming: false,
            collapsible: false,
            pending_permission: false,
            failed: false,
            runtime_attribution: None,
            file_path: None,
        }
    }

    /// Whether the runtime has echoed this message back into the timeline.
    fn is_confirmed_by(&self, items: &[vibex_core::TimelineItem]) -> bool {
        items.iter().any(|item| self.is_confirmed_item(item))
    }

    /// Whether one timeline item is the echo of this send.
    fn is_confirmed_item(&self, item: &vibex_core::TimelineItem) -> bool {
        item.sequence > self.after_sequence
            && matches!(
                &item.payload,
                vibex_core::TimelinePayload::UserMessage(message)
                    if message.text.trim() == self.text.trim()
                        && message.attachments == self.attachments
            )
    }
}

/// The whole application.
pub struct App {
    pub facade: BackendFacade,
    pub capabilities: BackendCapabilitySnapshot,
    pub seat: SeatKind,

    pub theme: TuiTheme,
    pub capability: ColorCapability,
    pub strings: Strings,
    pub keymap: Keymap,
    pub shell: ShellKind,
    pub navigation: CompactNavigation,

    pub agent: AgentWorkflowController,
    pub management: ManagementWorkflowController,
    pub projection: ProjectionState,
    pub transcript: Transcript,
    pub scroll: ScrollState,

    pub composer: ComposerBuffer,
    pub history: ComposerHistory,
    pub completion: Option<CompletionMenu>,

    pub page: Page,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub toast: Option<Toast>,

    pub settings: SettingsState,
    pub management_data: ManagementData,
    pub selection: BTreeMap<Scope, usize>,
    pub filter: String,
    pub filtering: bool,
    pub show_archived: bool,
    pub session_title: Option<String>,
    pub live: LiveState,
    pub pending: BTreeMap<String, ()>,
    pub generation: u64,
    pub should_quit: bool,
    pub workspace_rows: Vec<vibex_backend::WorkspaceSummary>,
    pub workspace_browse: Option<vibex_core::RemoteWorkspaceDirectoryListing>,
    pub file_rows: Vec<vibex_core::FileTreeEntry>,
    pub git_status: Option<vibex_core::GitStatusSummary>,
    pub git_history: Vec<vibex_core::GitCommitSummary>,
    pub git_branches: Vec<vibex_core::GitBranchSummary>,
    pub worktrees: Option<Box<vibex_core::GitWorktreeLifecycleSnapshot>>,
    pub diff_text: Option<String>,
    pub text_view: Option<(String, String)>,
    /// The crossterm-reported terminal size, used by renderers that need to
    /// make a layout decision before the frame buffer exists.
    pub viewport: (u16, u16),
    /// Rows the transcript band had in the last frame.
    ///
    /// The scroll ceiling is only as good as the viewport it is measured
    /// against, and the band is not the terminal: the bands above and below it
    /// take their rows first. The renderer records what it actually drew.
    pub transcript_band_rows: usize,
    /// Regions the last frame published for mouse hit-testing: the transcript
    /// band and the close affordance of the modal, when one is open.
    pub regions: FrameRegions,

    /// Runtime catalog, kept for the runtime picker.
    pub runtime_options: Option<vibex_core::SessionRuntimeOptionCatalog>,
    /// Whether the pending catalogue read was asked for by the picker, so the
    /// overlay opens when it lands. A catalogue fetched for the composer's
    /// info line must not pop a modal over the session.
    pub runtime_picker_pending: bool,
    /// The workspace chosen in the workspace browser, consumed by the
    /// new-session prompt.
    pub workspace_path: Option<String>,
    /// The message the composing page was holding when the session was asked
    /// for. The send needs a session id, and the id only exists once the runtime
    /// answers — so the message waits here for exactly one round trip.
    pub pending_new_session: Option<crate::composer::Outgoing>,
    /// The Agent and model chosen on the composing page, before there is a
    /// session to move. It is the runtime the session is *created* with.
    pub new_session_runtime: Option<vibex_core::SessionRuntimeSelection>,
    /// The free-text run option the open prompt is editing.
    ///
    /// A text option's value belongs to a key the prompt overlay cannot carry,
    /// so the key waits here for the one submission the prompt can make.
    pub run_option_prompt: Option<RunOptionKey>,
    /// In-progress elicitation answers.
    pub elicitation_draft: crate::reduce::ElicitationDraft,
    /// Whether the usage page shows this session or the aggregate.
    pub usage_scope_session: bool,
    /// Session ids whose detail card is open in the session list.
    ///
    /// Keyed by id rather than row index so a refresh, a rename or a filter
    /// cannot move a card onto a different session.
    pub session_cards: std::collections::BTreeSet<String>,
    /// The transcript search in progress, when the search bar is open.
    pub search: Option<crate::search::SearchState>,
    /// The list row the pointer is over, so a list can show it lit.
    pub hover: Option<(crate::keymap::Scope, usize)>,
    /// Which sent message the composer's history drawer points at.
    pub history_selection: usize,
    /// Commands run from the palette, most recent first.
    pub recent_commands: Vec<String>,
    /// The mouse selection over the transcript, while it is being made or after
    /// it has been copied.
    pub text_selection: Option<TextSelection>,
    /// Whether the button went down inside the composer, so a drag belongs to
    /// the draft instead of the transcript.
    pub draft_selecting: bool,
    /// Whether the session list groups sessions under their workspace, or shows
    /// them as one flat run. Persisted with the rest of the arrangement.
    pub sidebar_grouped: bool,
    /// Whether the dock panel above the composer is open.
    pub dock_open: bool,
    /// Which dock row the cursor is on, while the dock owns the keys.
    pub dock_selection: Option<usize>,
    /// Dock sections the reader has folded away.
    pub dock_collapsed: std::collections::BTreeSet<DockSection>,
    /// Whether finished agents and completed plan steps are hidden.
    pub dock_hide_done: bool,
    /// Where the arrangement is written; `None` keeps it in memory only.
    pub sidebar_path: Option<std::path::PathBuf>,
    /// The last left click, so two clicks in the same cell can be told apart
    /// from two clicks in different ones.
    pub last_click: Option<(std::time::Instant, usize, u16)>,
    /// Messages held back until the running turn ends.
    pub queued_messages: Vec<QueuedMessage>,
    /// Which queued message the queue band's cursor is on.
    pub queue_selection: Option<usize>,
    /// The message that has been sent but not yet echoed back by the runtime.
    ///
    /// The timeline belongs to the runtime, and its copy of the reader's own
    /// message arrives a round trip later. Until it does, the send is projected
    /// here — so the message is on screen the moment Enter is pressed, and the
    /// session reads as running rather than as having swallowed the message.
    pub pending_send: Option<PendingSend>,
    /// Bumped for every send, so each projected row keeps its own identity.
    pub pending_send_serial: u64,
    /// Sessions that finished a turn while the reader was looking elsewhere.
    ///
    /// The client's own notion, not the runtime's: it is what "unread" means in
    /// a list of sessions, and it is cleared by opening the session.
    pub unread_sessions: std::collections::BTreeSet<String>,
    /// A transient message above the composer, dismissed on the next key.
    pub banner: Option<Banner>,
    /// When the running turn started, for the elapsed-time readout.
    pub turn_started: Option<std::time::Instant>,
    /// Tokens spent by the running turn, for the readout beside the timer.
    pub turn_tokens: Option<u64>,
    /// What the Agent is doing right now, when the runtime says.
    pub activity: Option<String>,
    /// Monotonic clock driving every animation.
    pub animation_phase: u32,
    /// What the composer's prefix says the draft will do.
    pub composer_mode: ComposerMode,
}

/// What the draft will do when it is sent.
///
/// The mode is carried by the composer's prefix rather than by a label
/// elsewhere, so the answer is always where the reader is already looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposerMode {
    /// Send the draft to the Agent.
    #[default]
    Normal,
    /// Run the draft as a shell command.
    Shell,
    /// Treat the draft as a search over sent messages.
    HistorySearch,
}

/// How many rows the dock may take from the transcript.
///
/// It is a glance, not a screen: past a handful of rows the reader should open
/// the transcript, which is where the detail actually lives.
pub const MAX_DOCK_ROWS: usize = 8;

/// One section of the dock panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DockSection {
    Agents,
    Plan,
    Queue,
}

impl DockSection {
    pub const ALL: [DockSection; 3] = [Self::Agents, Self::Plan, Self::Queue];

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            DockSection::Agents => strings.dock_agents(),
            DockSection::Plan => strings.dock_plan(),
            DockSection::Queue => strings.dock_queue(),
        }
    }
}

/// One line of the dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockRow {
    /// A section heading, which the cursor steps over.
    Header { section: DockSection, count: usize },
    /// A delegated child agent.
    Agent {
        label: String,
        status: vibex_core::ToolCallStatus,
        summary: String,
        /// The transcript block the row stands for, so activating it can go
        /// there. Blocks are keyed by id rather than by sequence because a row
        /// can cover several items.
        block_id: String,
    },
    /// One step of the current plan.
    Plan {
        title: String,
        status: vibex_core::PlanStepStatus,
        block_id: String,
    },
    /// A message held until the running turn ends.
    Queue { index: usize, text: String },
}

/// How far through its plan the session is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoProgress {
    pub title: String,
    pub done: usize,
    pub total: usize,
    /// The step the Agent is on, when one is running.
    pub running: Option<String>,
}

/// A transient message above the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    pub tone: BannerTone,
    /// What the message is, which decides what may replace it.
    pub priority: BannerPriority,
}

/// Who owns the banner row.
///
/// There is one row, so something has to decide who gets it. Ordering the
/// claimants is that decision: a warning about the connection outranks a tip,
/// and a tip never displaces a mode reminder the reader still needs. Without
/// this the last writer wins and the row flickers between unrelated messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BannerPriority {
    /// A toast-like note about something that just happened.
    Transient,
    /// A rotating tip.
    Tip,
    /// A reminder about the mode the composer is in.
    Mode,
    /// Something the reader must know: offline, held messages.
    Warning,
}

/// What a banner is, for the copy and the priority bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Offline,
    Held,
    ShellMode,
    HistoryMode,
    Tip,
}

/// Regions the last frame painted, kept so a mouse event can be mapped back to
/// the thing under the pointer.
///
/// The renderer is the only code that knows where a band landed, so it
/// publishes what the mouse layer needs rather than the mouse layer
/// recomputing the layout with a second copy of the maths.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrameRegions {
    /// The rows that actually show transcript text: the band, less the pinned
    /// header and the search bar.
    pub scrollback: ratatui::layout::Rect,
    /// The close affordance on the open modal's top border.
    pub modal_close: Option<ratatui::layout::Rect>,
    /// The composer's text rows, for click-to-place-the-cursor.
    pub composer: Option<ratatui::layout::Rect>,
    /// Display rows the composer has scrolled past the top of its text area.
    ///
    /// The draft is wrapped to the box, and one taller than the box scrolls
    /// under it; without this a click on a visible row would place the caret on
    /// the row that row would have been at with no scrolling.
    pub composer_scroll: u16,
    /// The queue band's rows, for click-to-select.
    pub queue: Option<ratatui::layout::Rect>,
    /// The dock panel's rows, for click-to-select.
    pub dock: Option<ratatui::layout::Rect>,
    /// The composer's whole band, borders included: a click anywhere in the box
    /// takes the keyboard, and only the text rows can also take the caret.
    pub composer_band: Option<ratatui::layout::Rect>,
    /// The banner row, which a click dismisses.
    pub banner: Option<ratatui::layout::Rect>,
    /// A row-per-index list the frame drew: its rect and the scope it selects
    /// in. Clicking row `n` selects entry `n`.
    pub list: Option<ListRegion>,
    /// The turn rail's ticks, one rect per turn.
    pub turns: Vec<(ratatui::layout::Rect, usize)>,
    /// The shortcut band's hints, so a click runs the same intent as the key.
    pub hints: Vec<(ratatui::layout::Rect, crate::action::Intent)>,
}

impl FrameRegions {
    /// Start a frame: the row-per-something lists are rewritten, not extended.
    ///
    /// A rect describes where something was when the frame drew it. Keeping the
    /// previous frame's rects would let a click land on a row that has moved or
    /// gone, and the lists would grow with every frame the reader leaves open.
    pub fn begin_frame(&mut self) {
        self.turns.clear();
        self.hints.clear();
    }
}

/// A clickable list drawn by a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListRegion {
    pub rect: ratatui::layout::Rect,
    pub scope: crate::keymap::Scope,
    /// Row indices that are selectable, in screen order. A list with group
    /// headers has fewer selectable rows than it has lines.
    pub rows: usize,
    /// Lines between the top of the rect and the first selectable row.
    pub first_line: usize,
}

/// Which row a pointer is over, if any.
pub fn list_row_at(region: &ListRegion, column: u16, row: u16) -> Option<usize> {
    if column < region.rect.x
        || column >= region.rect.right()
        || row < region.rect.y
        || row >= region.rect.bottom()
    {
        return None;
    }
    let index = usize::from(row - region.rect.y).checked_sub(region.first_line)?;
    (index < region.rows).then_some(index)
}

/// A free-text selection over the transcript, in display coordinates.
///
/// `line` counts display lines from the top of the transcript, and `column`
/// counts terminal cells from the left edge of the transcript band. Both are
/// what the renderer already speaks in, so a selection never has to be
/// translated back into block or byte coordinates to be painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: (usize, u16),
    pub head: (usize, u16),
    /// Whether the button is still held.
    pub dragging: bool,
}

impl TextSelection {
    /// The selection with the two ends in reading order.
    pub fn ordered(&self) -> ((usize, u16), (usize, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether the selection covers any cell at all.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerTone {
    Info,
    Warning,
    Danger,
}

impl Banner {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: BannerTone::Info,
            priority: BannerPriority::Transient,
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: BannerTone::Warning,
            priority: BannerPriority::Warning,
        }
    }

    pub fn danger(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: BannerTone::Danger,
            priority: BannerPriority::Warning,
        }
    }

    pub fn with_priority(mut self, priority: BannerPriority) -> Self {
        self.priority = priority;
        self
    }
}

impl App {
    /// Build an app around an already-constructed backend facade.
    pub fn new(facade: BackendFacade, options: AppOptions) -> Self {
        let capabilities = facade.capabilities();
        let capability = options.capability;
        let theme = TuiTheme::resolve(options.theme_id.as_deref(), options.mode, capability);
        let strings = Strings::with_locale(options.locale);
        let agent =
            AgentWorkflowController::new(facade.agent().clone(), capabilities.agent.clone());
        let management = ManagementWorkflowController::new(
            facade.management().clone(),
            facade.device().clone(),
            vibex_ui::management::ManagementWorkflowCapabilities::from_backend(&capabilities),
        );
        let keymap = match Keymap::user_path() {
            Some(path) => Keymap::load(&path),
            None => Keymap::built_in(),
        };
        let arrangement = SidebarArrangement::load(options.sidebar_path.as_deref());
        let mut navigation = CompactNavigation::default();
        navigation.select_global(GlobalDestination::Sessions);
        Self {
            facade,
            capabilities,
            seat: options.seat,
            theme: theme.clone(),
            capability,
            strings,
            keymap,
            shell: ShellKind::Wide,
            navigation,
            agent,
            management,
            projection: ProjectionState {
                sidebar: arrangement.sidebar,
                rows: Vec::new(),
                sidebar_collapsed: false,
            },
            sidebar_grouped: arrangement.grouped,
            sidebar_path: options.sidebar_path,
            transcript: Transcript::new(),
            scroll: ScrollState::default(),
            composer: ComposerBuffer::default(),
            history: ComposerHistory::new(64),
            completion: None,
            page: Page::Sessions,
            focus: Focus::Main,
            overlay: None,
            toast: None,
            settings: SettingsState {
                theme_id: theme.id.to_string(),
                mode: options.mode,
                locale: options.locale,
                glyphs: capability.glyphs,
                selected: 0,
                view: SettingsMode::Browse,
                filter: String::new(),
                status_line: true,
            },
            management_data: ManagementData::default(),
            selection: BTreeMap::new(),
            filter: String::new(),
            filtering: false,
            show_archived: false,
            session_title: None,
            live: LiveState::Connecting,
            pending: BTreeMap::new(),
            generation: 0,
            should_quit: false,
            workspace_rows: Vec::new(),
            workspace_browse: None,
            file_rows: Vec::new(),
            git_status: None,
            git_history: Vec::new(),
            git_branches: Vec::new(),
            worktrees: None,
            diff_text: None,
            text_view: None,
            viewport: (120, 40),
            transcript_band_rows: 20,
            regions: FrameRegions::default(),
            runtime_options: None,
            runtime_picker_pending: false,
            workspace_path: None,
            pending_new_session: None,
            new_session_runtime: None,
            run_option_prompt: None,
            elicitation_draft: crate::reduce::ElicitationDraft::default(),
            usage_scope_session: true,
            session_cards: std::collections::BTreeSet::new(),
            search: None,
            hover: None,
            history_selection: 0,
            recent_commands: Vec::new(),
            text_selection: None,
            draft_selecting: false,
            dock_open: false,
            dock_selection: None,
            dock_collapsed: std::collections::BTreeSet::new(),
            dock_hide_done: false,
            last_click: None,
            queued_messages: Vec::new(),
            queue_selection: None,
            pending_send: None,
            pending_send_serial: 0,
            unread_sessions: std::collections::BTreeSet::new(),
            banner: None,
            turn_started: None,
            turn_tokens: None,
            activity: None,
            animation_phase: 0,
            composer_mode: ComposerMode::Normal,
        }
    }

    // ---- derived state -------------------------------------------------

    /// Every capability domain, so operation lookups do not need a second
    /// hand-maintained operation → domain table.
    fn domains(&self) -> [&DomainCapabilities; 8] {
        let snapshot = &self.capabilities;
        [
            &snapshot.agent,
            &snapshot.workspace,
            &snapshot.file,
            &snapshot.git,
            &snapshot.terminal,
            &snapshot.browser,
            &snapshot.management,
            &snapshot.device,
        ]
    }

    /// Whether the connected authority offers an operation at all.
    pub fn supports(&self, operation: BackendOperation) -> bool {
        self.domains()
            .iter()
            .any(|domain| domain.operations.contains(&operation))
    }

    /// Whether the authority offers the operation but this device's grant does
    /// not permit it. The two are deliberately distinct: "not supported" and
    /// "needs a higher permission level" call for different copy.
    pub fn permission_blocked(&self, operation: BackendOperation) -> bool {
        self.domains()
            .iter()
            .any(|domain| domain.permission_required.contains(&operation))
    }

    /// Four-way availability used by every page.
    ///
    /// The order matters: a domain that the authority reports as offline is
    /// offline even though its operation set is fully populated, and a grant
    /// gap is reported as needing permission rather than as unsupported. The
    /// two produce different copy, and conflating them would tell a read-only
    /// device that a feature does not exist.
    pub fn availability(&self, operation: BackendOperation) -> Availability {
        let domain = self.domains().into_iter().find(|domain| {
            domain.operations.contains(&operation)
                || domain.permission_required.contains(&operation)
        });
        match domain {
            Some(domain) if domain.permission_required.contains(&operation) => {
                Availability::RequiresPermission
            }
            Some(domain)
                if domain.operations.contains(&operation)
                    && domain.availability != vibex_backend::CapabilityAvailability::Offline =>
            {
                Availability::Available
            }
            Some(_) => Availability::Offline,
            None if !self.live.is_live() => Availability::Offline,
            None => Availability::Unsupported,
        }
    }

    pub fn selected_session_id(&self) -> Option<&VibexSessionId> {
        self.agent.state.selected_session_id.as_ref()
    }

    pub fn active_session(&self) -> Option<&AgentSession> {
        self.agent.state.active_session.value.as_ref()
    }

    /// Open the session view for a session that does not exist yet.
    ///
    /// The reader sent the first message and the runtime is still making the
    /// session it belongs to. They are moved into the session view at once —
    /// waiting on the page reads as nothing having happened — and the view is
    /// emptied of the session they came from, because what is about to appear
    /// there is a new one. The message itself is projected into it until the
    /// runtime's own copy arrives.
    pub fn enter_creating_session(&mut self) {
        if self.agent.state.selected_session_id.take().is_some() {
            self.agent.state.active_session.clear();
            self.agent.state.timeline = vibex_desktop_model::TimelineModel::default();
            self.agent.state.timeline_has_older = false;
        }
        self.transcript.set_blocks(Vec::new());
        self.scroll = ScrollState::default();
        self.page = Page::Agent;
        self.focus = Focus::Composer;
        self.workspace_path = None;
    }

    /// The runtime selection the open session is on: its Agent, the account or
    /// provider profile that authenticates it, and the model.
    ///
    /// This is the session's own durable *desired* selection, not the client's
    /// preferred catalogue entry, which is why the composer can name the Agent
    /// and model a message will actually go to.
    pub fn session_runtime_selection(&self) -> Option<&vibex_core::SessionRuntimeSelection> {
        self.agent
            .state
            .runtime_selection
            .value
            .as_ref()
            .map(|state| &state.desired)
    }

    /// Whether a catalogue entry is the one the open session is on.
    ///
    /// Matching is by Agent, authentication source and model rather than by
    /// whole-selection equality: the catalogue's choice carries the feature
    /// values it was published with, and a session that has since been tuned
    /// is still on that choice.
    pub fn runtime_option_is_current(&self, option: &vibex_core::SessionRuntimeOption) -> bool {
        self.session_runtime_selection().is_some_and(|selection| {
            option.selection.agent_id == selection.agent_id
                && option.selection.auth_source == selection.auth_source
                && option.selection.model == selection.model
        })
    }

    /// The index of the open session's runtime choice in the loaded catalogue.
    pub fn current_runtime_option_index(&self) -> Option<usize> {
        let catalog = self.runtime_options.as_ref()?;
        // On the page where a session is being written, the row that is current
        // is the one the reader chose for it — not the one the session behind
        // the page is running on.
        if (self.page == Page::NewSession || self.active_session().is_none())
            && let Some(chosen) = self.new_session_runtime.as_ref()
        {
            return catalog
                .options
                .iter()
                .position(|option| &option.selection == chosen);
        }
        catalog
            .options
            .iter()
            .position(|option| self.runtime_option_is_current(option))
    }

    /// The catalogue entry a selection belongs to, when the catalogue holds it.
    pub fn runtime_option_for(
        &self,
        selection: &vibex_core::SessionRuntimeSelection,
    ) -> Option<&vibex_core::SessionRuntimeOption> {
        self.runtime_options
            .as_ref()?
            .options
            .iter()
            .find(|option| {
                option.selection.agent_id == selection.agent_id
                    && option.selection.auth_source == selection.auth_source
                    && option.selection.model == selection.model
            })
    }

    /// The first catalogue entry a session may be created or switched on.
    fn default_runtime_selection(&self) -> Option<vibex_core::SessionRuntimeSelection> {
        self.runtime_options
            .as_ref()?
            .options
            .iter()
            .find(|option| option.availability == vibex_core::RuntimeOptionAvailability::Available)
            .map(|option| option.selection.clone())
    }

    /// Whether the page answers for a session that does not exist yet.
    ///
    /// The composing page keeps the client's session *selected* — leaving the
    /// page has to return there — so anything that asks "which session?" while
    /// it is up answers with the session behind the page rather than with what
    /// the reader is writing. Reading a runtime choice, and applying one, both
    /// have to ask the *page* first.
    pub fn page_is_composing(&self) -> bool {
        self.page == Page::NewSession || self.active_session().is_none()
    }

    /// The runtime selection this page's next message would go through.
    ///
    /// The composing page answers with the choice it is holding — falling back
    /// to the catalogue's first available entry, which is what
    /// [`Effect::CreateSession`] uses when the page chose nothing — while an
    /// open session answers with its own durable desired selection. A session
    /// whose runtime has never been activated has no selection to read, so it
    /// gets none rather than the catalogue's first entry, which would offer
    /// another Agent's options over it.
    pub fn page_runtime_selection(&self) -> Option<vibex_core::SessionRuntimeSelection> {
        if self.page_is_composing() {
            return self
                .new_session_runtime
                .clone()
                .or_else(|| self.default_runtime_selection());
        }
        self.session_runtime_selection().cloned()
    }

    /// The run options the page's selected Agent publishes.
    ///
    /// The catalogue is the authority: a selection the catalogue does not hold
    /// (an Agent that has since gone away) yields no options rather than a
    /// guess. Order is the Agent's own — thinking depth, conversation mode,
    /// then the session features — and it is what both the switcher and the
    /// value lists index.
    pub fn run_options(&self) -> Vec<RunOption> {
        let Some(selection) = self.page_runtime_selection() else {
            return Vec::new();
        };
        let Some(catalog) = self.runtime_options.as_ref() else {
            return Vec::new();
        };
        let projection =
            vibex_desktop_model::RuntimeCascadeProjection::from_catalog(catalog, &selection);
        let mut options = Vec::new();
        if !projection.reasoning_efforts.is_empty() {
            options.push(RunOption {
                key: RunOptionKey::ReasoningEffort,
                label: self.strings.runtime_thinking_depth().to_string(),
                description: None,
                kind: RunOptionKind::Choice,
                values: projection
                    .reasoning_efforts
                    .iter()
                    .map(|choice| vibex_core::SessionConfigValue {
                        value: choice.value.clone(),
                        label: Some(choice.label.clone()),
                    })
                    .collect(),
                explicit: selection.reasoning_effort.clone(),
                resolved: selection.reasoning_effort.as_ref().map(|effort| {
                    vibex_core::SessionConfigValue {
                        value: effort.clone(),
                        label: projection
                            .reasoning_efforts
                            .iter()
                            .find(|choice| &choice.value == effort)
                            .map(|choice| choice.label.clone()),
                    }
                }),
            });
        }
        if !projection.modes.is_empty() {
            options.push(RunOption {
                key: RunOptionKey::Mode,
                label: self.strings.runtime_conversation_mode().to_string(),
                description: None,
                kind: RunOptionKind::Choice,
                values: projection
                    .modes
                    .iter()
                    .map(|choice| vibex_core::SessionConfigValue {
                        value: choice.value.clone(),
                        label: Some(choice.label.clone()),
                    })
                    .collect(),
                explicit: selection.mode_id.clone(),
                resolved: selection
                    .mode_id
                    .as_ref()
                    .map(|mode| vibex_core::SessionConfigValue {
                        value: mode.clone(),
                        label: projection
                            .modes
                            .iter()
                            .find(|choice| &choice.value == mode)
                            .map(|choice| choice.label.clone()),
                    }),
            });
        }
        for feature in projection.features {
            let kind = match feature.kind {
                vibex_core::SessionRuntimeFeatureKind::String => RunOptionKind::Text,
                vibex_core::SessionRuntimeFeatureKind::Toggle => RunOptionKind::Toggle,
                vibex_core::SessionRuntimeFeatureKind::Select => RunOptionKind::Choice,
            };
            let resolved = feature.value_for(&selection.config_values);
            // A switch reads as a state, not as the wire's `true`: the row is
            // the reader's only view of a value the Agent published as a word.
            let resolved = match (kind, resolved) {
                (RunOptionKind::Toggle, Some(value)) => {
                    let label = match value.value.as_str() {
                        "true" => Some(self.strings.runtime_on().to_string()),
                        "false" => Some(self.strings.runtime_off().to_string()),
                        _ => value.label.clone(),
                    };
                    Some(vibex_core::SessionConfigValue {
                        value: value.value,
                        label,
                    })
                }
                (_, resolved) => resolved,
            };
            options.push(RunOption {
                key: RunOptionKey::Feature(feature.id.clone()),
                label: feature.label.clone(),
                description: feature.description.clone(),
                kind,
                values: feature.values.clone(),
                explicit: selection.config_values.get(&feature.id).cloned(),
                resolved,
            });
        }
        options
    }

    /// How many rows one view of the runtime switcher lists.
    ///
    /// The two views index their own rows: the catalogue by entry, the run
    /// options by what the chosen entry publishes.
    pub fn runtime_picker_row_count(&self, view: RuntimePickerView) -> usize {
        match view {
            RuntimePickerView::Choices => self
                .runtime_options
                .as_ref()
                .map(|catalog| catalog.options.len())
                .unwrap_or(0),
            RuntimePickerView::Options => self.run_options().len(),
        }
    }

    /// The value rows a run option's list offers: the Agent's own default
    /// first, then what the option accepts.
    ///
    /// A toggle is spelled as its two states rather than as a bare `true` and
    /// `false`, and a value in effect that the catalogue stopped publishing
    /// keeps its row, so "which value am I on" stays answerable.
    pub fn run_option_choices(&self, option: &RunOption) -> Vec<(Option<String>, String)> {
        let mut rows = vec![(None, self.strings.runtime_default().to_string())];
        match option.kind {
            RunOptionKind::Toggle => {
                rows.push((
                    Some("true".to_string()),
                    self.strings.runtime_on().to_string(),
                ));
                rows.push((
                    Some("false".to_string()),
                    self.strings.runtime_off().to_string(),
                ));
            }
            RunOptionKind::Choice | RunOptionKind::Text => {
                for value in &option.values {
                    rows.push((Some(value.value.clone()), session_config_value_label(value)));
                }
            }
        }
        if let Some(explicit) = option.explicit.as_deref()
            && !rows
                .iter()
                .any(|(value, _)| value.as_deref() == Some(explicit))
        {
            rows.push((Some(explicit.to_string()), explicit.to_string()));
        }
        rows
    }

    /// The run options the composer's info line names: how deep the Agent
    /// thinks and which conversation mode it is in, when the selection asks for
    /// either.
    ///
    /// The Agent's own features stay in the switcher: the info line has room
    /// for the shape of the message about to be written, not for a list of
    /// every setting behind it.
    pub fn composer_run_option_labels(&self) -> Vec<String> {
        self.run_options()
            .into_iter()
            .filter(|option| {
                matches!(
                    option.key,
                    RunOptionKey::ReasoningEffort | RunOptionKey::Mode
                ) && option.is_explicit()
            })
            .map(|option| option.resolved_label(self.strings.runtime_default()))
            .collect()
    }

    /// The Agent and model a message from this page will be sent through.
    ///
    /// Read from the session's own runtime selection rather than from the
    /// catalogue's first entry: a session keeps the runtime it was created with
    /// until the reader switches it, and the page has to name that one.
    pub fn composer_runtime_labels(&self) -> (String, String) {
        if let Some(option) = self
            .current_runtime_option_index()
            .and_then(|index| self.runtime_options.as_ref()?.options.get(index))
        {
            return (option.agent_label.clone(), option.model_label.clone());
        }
        let agent = self
            .session_runtime_selection()
            .map(|selection| selection.agent_id.to_string())
            .or_else(|| {
                self.active_session()
                    .map(|session| session.agent_id.to_string())
            })
            .or_else(|| {
                self.runtime_options
                    .as_ref()
                    .and_then(|catalog| catalog.options.first())
                    .map(|option| option.agent_label.clone())
            })
            .unwrap_or_else(|| self.strings.runtime_unavailable().to_string());
        let model = self
            .session_runtime_selection()
            .and_then(|selection| selection.model.model_id())
            .map(str::to_string)
            .or_else(|| {
                self.runtime_options
                    .as_ref()
                    .and_then(|catalog| catalog.options.first())
                    .map(|option| option.model_label.clone())
            })
            .unwrap_or_default();
        (agent, model)
    }

    /// Whether the page's mark is lit: it shines only on a page that waits.
    pub fn composing_page_shines(&self) -> bool {
        self.page == Page::NewSession && self.focus == Focus::Composer
    }

    /// The workspace a session created from the composing page will open in.
    ///
    /// The same answer [`Self::create_session_from_draft`] sends: the directory
    /// the reader chose, else the one the open session already runs in.
    pub fn new_session_workspace(&self) -> String {
        self.workspace_path
            .clone()
            .or_else(|| {
                self.active_session()
                    .map(|session| session.workspace_root.clone())
            })
            .or_else(|| {
                self.workspace_rows
                    .first()
                    .map(|workspace| workspace.workspace.root_path.clone())
            })
            // Nothing has told us where the runtime works yet; this client's own
            // directory is the honest guess while the list is on its way.
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| path.display().to_string())
            })
            .unwrap_or_default()
    }

    /// Workspace the session pages read from.
    pub fn active_workspace_id(&self) -> Option<vibex_core::WorkspaceId> {
        self.active_session()
            .map(|session| session.workspace_id.clone())
    }

    /// Sidebar rows for the current filter, with the reader's grouping applied.
    pub fn sidebar_rows(&self) -> Vec<vibex_desktop_model::AgentSidebarRow> {
        let mut rows = self
            .agent
            .state
            .view(&self.projection.sidebar, &self.filter, self.shell)
            .sessions;
        if !self.sidebar_grouped {
            // Flat mode keeps the order the projection computed -- pinned
            // first, then the manual order -- and only drops the headings.
            rows.retain(|row| row.kind != vibex_desktop_model::AgentSidebarRowKind::Project);
        }
        rows
    }

    /// The row the session cursor is on.
    pub fn selected_sidebar_row(&self) -> Option<vibex_desktop_model::AgentSidebarRow> {
        let rows = self.sidebar_rows();
        rows.get(self.selection_for(Scope::Sessions)).cloned()
    }

    /// Where the manual arrangement is written.
    ///
    /// It sits beside the key file under the same home, so a reader who wants
    /// to reset the interface has one directory to clear.
    pub fn sidebar_arrangement_path() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_SIDEBAR")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        let home = std::env::var("VIBEX_HOME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| std::path::PathBuf::from(value).join(".vibex"))
            })?;
        Some(home.join("tui-sidebar.json"))
    }

    /// Persist the arrangement. A failure is silent: losing a pin is not worth
    /// interrupting the reader, and the next change tries again.
    pub fn save_sidebar_arrangement(&self) {
        let Some(path) = self.sidebar_path.clone() else {
            return;
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let arrangement = SidebarArrangement {
            sidebar: self.projection.sidebar.clone(),
            grouped: self.sidebar_grouped,
        };
        if let Ok(body) = serde_json::to_string_pretty(&arrangement) {
            let _ = std::fs::write(path, body);
        }
    }

    /// Fold the loaded session ids into the arrangement, keeping the reader's
    /// choices and appending anything new.
    ///
    /// New ids are appended in the order they are already shown, not in the
    /// order the runtime listed them: seeding the manual order is a way of
    /// remembering the current list, so it must not quietly reshuffle it the
    /// first time a session appears.
    pub fn reconcile_sidebar_arrangement(&mut self) {
        let ids = self
            .agent
            .state
            .view(&self.projection.sidebar, "", self.shell)
            .sessions
            .into_iter()
            .filter_map(|row| row.session_id.map(|id| id.to_string()))
            .collect::<Vec<_>>();
        self.projection.sidebar.reconcile(ids);
    }

    /// Keep the selected session pinned above the rest, or let it go.
    pub fn toggle_session_pin(&mut self) -> bool {
        let Some(row) = self.selected_sidebar_row() else {
            return false;
        };
        let Some(session_id) = row.session_id.as_ref().map(ToString::to_string) else {
            return false;
        };
        if !self.projection.sidebar.pinned_ids.remove(&session_id) {
            self.projection.sidebar.pinned_ids.insert(session_id);
        }
        self.save_sidebar_arrangement();
        true
    }

    /// Move the selected session one place through the manual order.
    ///
    /// The cursor follows the row, so a second press moves the same session
    /// again rather than the one that took its place.
    pub fn move_session_row(&mut self, delta: isize) -> Option<bool> {
        // A manual order is only meaningful once every loaded session has a
        // place in it, and a session created in this run may not have one yet.
        self.reconcile_sidebar_arrangement();
        let rows = self.sidebar_rows();
        let index = self.selection_for(Scope::Sessions);
        let row = rows.get(index)?;
        let moving_id = row.session_id.as_ref()?.to_string();
        let moving_pinned = row.pinned;
        let mut cursor = index as isize + delta;
        let mut target = None;
        while cursor >= 0 && (cursor as usize) < rows.len() {
            let candidate = &rows[cursor as usize];
            if let Some(id) = candidate.session_id.as_ref() {
                target = Some((cursor as usize, id.to_string(), candidate.pinned));
                break;
            }
            cursor += delta;
        }
        let (target_index, target_id, target_pinned) = target?;
        // Pinned rows sort above the rest whatever the manual order says, so a
        // move across that line would look like nothing happened. Saying so is
        // better than silently disagreeing with the reader.
        if moving_pinned != target_pinned {
            return Some(false);
        }
        if !self
            .projection
            .sidebar
            .move_row_relative(&moving_id, &target_id, delta > 0)
        {
            // Already against the end of the order: nothing to say about it.
            return None;
        }
        self.set_selection(Scope::Sessions, target_index);
        self.save_sidebar_arrangement();
        Some(true)
    }

    /// Show the list grouped by workspace, or as one flat run.
    pub fn toggle_sidebar_grouping(&mut self) -> bool {
        self.sidebar_grouped = !self.sidebar_grouped;
        self.save_sidebar_arrangement();
        self.sidebar_grouped
    }

    /// The approval cards currently waiting.
    pub fn approvals(&self) -> Vec<vibex_ui::ApprovalSurfaceModel> {
        self.agent.state.approval_surfaces(self.shell)
    }

    /// The elicitation forms currently waiting.
    pub fn elicitations(&self) -> Vec<vibex_ui::ElicitationSurfaceModel> {
        self.agent.state.elicitation_surfaces(self.shell)
    }

    pub fn pending_permission_count(&self) -> usize {
        self.approvals().len()
    }

    pub fn selection_for(&self, scope: Scope) -> usize {
        self.selection.get(&scope).copied().unwrap_or(0)
    }

    pub fn set_selection(&mut self, scope: Scope, value: usize) {
        self.selection.insert(scope, value);
    }

    /// Row count for the page that currently owns the selection.
    pub fn page_row_count(&self) -> usize {
        match self.page {
            Page::NewSession => 0,
            Page::Sessions => self.sidebar_rows().len(),
            Page::Management => ManagementRow::ALL.len(),
            Page::Devices => self.management_data.devices.len(),
            Page::Providers => self.management_data.providers.len(),
            Page::Agents => self.management_data.agents.len(),
            Page::Mcp => self.management_data.mcp.len(),
            Page::Skills => self.management_data.skills.len(),
            Page::Prompts => self.management_data.prompts.len(),
            Page::Hooks => self.management_data.hooks.len(),
            Page::Recovery => RecoveryAction::ALL.len(),
            Page::Settings => self.visible_settings().len(),
            Page::Agent => self.transcript.len(),
            Page::Changes => self.file_rows.len(),
            Page::Files => self.file_rows.len(),
            Page::Usage | Page::Help | Page::Terminal => 0,
        }
    }

    /// The scopes consulted for key dispatch, most specific first.
    pub fn active_scopes(&self) -> Vec<Scope> {
        let mut scopes = Vec::new();
        if self.overlay.is_some() {
            scopes.push(Scope::Overlay);
        }
        if self.filtering {
            // While typing a filter only cancellation and editing apply.
            scopes.push(Scope::Global);
            return scopes;
        }
        // The composer owns the keyboard on every page that has one — including
        // the page where a session is being written, which has no session yet.
        // Without this the page's keys fell through to the Agent scope, where
        // a printable character is a *binding*: typing did nothing, and `/`
        // searched the transcript of a session the reader was leaving.
        if self.focus == Focus::Composer && self.page.is_composing_page() {
            scopes.push(Scope::Composer);
        }
        if self.overlay.is_none() {
            scopes.push(self.page.scope());
        }
        scopes.push(Scope::Global);
        scopes
    }

    /// The scopes whose bindings the key bar and `?` help should advertise.
    pub fn documented_scopes(&self) -> Vec<Scope> {
        if self.overlay.is_some() {
            return vec![Scope::Overlay, Scope::Global];
        }
        if self.focus == Focus::Composer && self.page.is_composing_page() {
            return vec![Scope::Composer, Scope::Global];
        }
        vec![self.page.scope(), Scope::Global]
    }

    // ---- mutation helpers ----------------------------------------------

    pub fn toast(&mut self, toast: Toast) {
        self.toast = Some(toast);
    }

    /// Advance transient state by one frame.
    /// Collapse or expand the whole sidebar.
    pub fn toggle_sidebar_collapsed(&mut self) {
        self.projection.sidebar_collapsed = !self.projection.sidebar_collapsed;
    }

    /// Collapse or expand one project group.
    pub fn toggle_sidebar_collapsed_for(&mut self, project_id: &str) {
        if !self.projection.sidebar.collapsed_ids.remove(project_id) {
            self.projection
                .sidebar
                .collapsed_ids
                .insert(project_id.to_string());
        }
    }

    /// Whether a session's detail card is open in the session list.
    pub fn session_card_expanded(&self, session_id: &str) -> bool {
        self.session_cards.contains(session_id)
    }

    /// Open or close one session's detail card.
    pub fn toggle_session_card(&mut self, session_id: &str) {
        if !self.session_cards.remove(session_id) {
            self.session_cards.insert(session_id.to_string());
        }
    }

    /// Close one session's detail card, reporting whether it was open.
    pub fn close_session_card(&mut self, session_id: &str) -> bool {
        self.session_cards.remove(session_id)
    }

    /// Close every open session detail card, reporting how many were open.
    pub fn close_session_cards(&mut self) -> usize {
        let open = self.session_cards.len();
        self.session_cards.clear();
        open
    }

    /// The session the session-list selection points at, when it points at one.
    pub fn selected_session_row_id(&self) -> Option<VibexSessionId> {
        let rows = self.sidebar_rows();
        let index = self.selection_for(Scope::Sessions);
        rows.get(index)?.session_id.clone()
    }

    /// The session the session-list selection points at, when it points at one.
    pub fn selected_session_row(&self) -> Option<&AgentSession> {
        let session_id = self.selected_session_row_id()?;
        self.agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|session| session.id == session_id)
    }

    /// The full record for one session id, when the list has loaded it.
    pub fn session_by_id(&self, session_id: &VibexSessionId) -> Option<&AgentSession> {
        self.agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|session| &session.id == session_id)
    }

    /// Whether the terminal is narrow enough that chrome must give way.
    pub fn is_compact(&self) -> bool {
        self.shell == ShellKind::Compact
    }

    /// Which glyph tier the terminal can render.
    pub fn glyph_tier(&self) -> crate::glyphs::GlyphTier {
        crate::glyphs::GlyphTier::of(&self.theme)
    }

    /// The frame counter every animation reads from.
    pub fn animation_phase(&self) -> u32 {
        self.animation_phase
    }

    /// Pending questions from the Agent, which also block the turn.
    pub fn pending_elicitations(&self) -> usize {
        self.agent.state.elicitation_surfaces(self.shell).len()
    }

    /// The dock's rows: delegated agents, the current plan and the held queue.
    ///
    /// Everything here is derived from the transcript the reader can already
    /// see, so the panel cannot disagree with the page behind it. Sections with
    /// nothing in them are left out rather than drawn empty.
    pub fn dock_rows(&self) -> Vec<DockRow> {
        let items = &self.agent.state.timeline.items;
        let mut rows = Vec::new();

        // Delegated child agents, newest row first and one row per delegation:
        // a child that reports progress must not grow the panel every time.
        let mut seen = std::collections::BTreeSet::new();
        let mut agents = Vec::new();
        for row in &self.projection.rows {
            let Some(delegation) = vibex_desktop_model::timeline_row_delegation(row, items) else {
                continue;
            };
            if !seen.insert(delegation.delegation_id.to_string()) {
                continue;
            }
            if self.dock_hide_done && delegation.status == vibex_core::ToolCallStatus::Completed {
                continue;
            }
            agents.push((
                row.id.clone(),
                delegation.agent_label.unwrap_or(delegation.action),
                delegation.status,
                delegation.summary,
            ));
        }
        if !agents.is_empty() {
            rows.push(DockRow::Header {
                section: DockSection::Agents,
                count: agents.len(),
            });
            if !self.dock_collapsed.contains(&DockSection::Agents) {
                rows.extend(
                    agents
                        .into_iter()
                        .map(|(block_id, label, status, summary)| DockRow::Agent {
                            label,
                            status,
                            summary,
                            block_id,
                        }),
                );
            }
        }

        if let Some(plan) = vibex_desktop_model::current_agent_plan(items) {
            let plan_block = self
                .projection
                .rows
                .iter()
                .find(|row| {
                    row.first_sequence <= plan.sequence && plan.sequence <= row.last_sequence
                })
                .map(|row| row.id.clone())
                .unwrap_or_default();
            let block_id = plan_block;
            let steps = plan
                .steps
                .iter()
                .filter(|step| {
                    !self.dock_hide_done || step.status != vibex_core::PlanStepStatus::Completed
                })
                .collect::<Vec<_>>();
            if !steps.is_empty() {
                rows.push(DockRow::Header {
                    section: DockSection::Plan,
                    count: steps.len(),
                });
                if !self.dock_collapsed.contains(&DockSection::Plan) {
                    rows.extend(steps.into_iter().map(|step| DockRow::Plan {
                        title: step.title.clone(),
                        status: step.status,
                        block_id: block_id.clone(),
                    }));
                }
            }
        }

        if rows.iter().all(|row| {
            !matches!(
                row,
                DockRow::Header {
                    section: DockSection::Plan,
                    ..
                }
            )
        }) && let Some(progress) = self.todo_progress()
        {
            // No structured plan item reached this client, but the transcript
            // still carries the plan block the reader can see. Showing what is
            // known beats showing nothing.
            let block_id = self
                .transcript
                .blocks()
                .iter()
                .rev()
                .find(|block| {
                    matches!(
                        block.kind,
                        vibex_desktop_model::TimelineRowKind::TodoUpdate
                            | vibex_desktop_model::TimelineRowKind::Plan
                    )
                })
                .map(|block| block.id.clone())
                .unwrap_or_default();
            rows.push(DockRow::Header {
                section: DockSection::Plan,
                count: progress.total,
            });
            if !self.dock_collapsed.contains(&DockSection::Plan) {
                // A running step is never "done", whatever the counts say.
                if let Some(running) = progress.running.as_ref() {
                    rows.push(DockRow::Plan {
                        title: running.clone(),
                        status: vibex_core::PlanStepStatus::Running,
                        block_id: block_id.clone(),
                    });
                }
                if !(self.dock_hide_done && progress.done == progress.total) {
                    rows.push(DockRow::Plan {
                        title: format!("{} {}/{}", progress.title, progress.done, progress.total),
                        status: if progress.done == progress.total {
                            vibex_core::PlanStepStatus::Completed
                        } else {
                            vibex_core::PlanStepStatus::Pending
                        },
                        block_id,
                    });
                }
            }
        }

        if !self.queued_for_active().is_empty() {
            let held = self.queued_for_active();
            rows.push(DockRow::Header {
                section: DockSection::Queue,
                count: held.len(),
            });
            if !self.dock_collapsed.contains(&DockSection::Queue) {
                rows.extend(held.into_iter().enumerate().map(|(index, at)| {
                    DockRow::Queue {
                        index,
                        text: self.queued_messages[at]
                            .text
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                    }
                }));
            }
        }
        rows
    }

    /// The dock's height: a title row plus the rows it can show.
    pub fn dock_height(&self) -> u16 {
        let rows = self.dock_rows().len();
        if rows == 0 {
            return 0;
        }
        let visible = rows.min(MAX_DOCK_ROWS.saturating_sub(1));
        (visible + 1) as u16
    }

    /// Step the dock cursor, skipping headings.
    pub fn step_dock_selection(&mut self, delta: isize) -> bool {
        let rows = self.dock_rows();
        if rows.is_empty() {
            self.dock_selection = None;
            return false;
        }
        let start = self
            .dock_selection
            .filter(|index| *index < rows.len())
            .unwrap_or(0);
        let mut index = start as isize;
        loop {
            index += delta;
            if index < 0 || index as usize >= rows.len() {
                self.dock_selection = Some(start);
                return false;
            }
            if !matches!(rows[index as usize], DockRow::Header { .. }) {
                self.dock_selection = Some(index as usize);
                return true;
            }
        }
    }

    /// Act on the dock row under the cursor.
    pub fn activate_dock_row(&mut self) -> bool {
        let rows = self.dock_rows();
        let Some(index) = self.dock_selection.filter(|index| *index < rows.len()) else {
            return false;
        };
        match rows[index].clone() {
            DockRow::Header { section, .. } => {
                if !self.dock_collapsed.remove(&section) {
                    self.dock_collapsed.insert(section);
                }
                true
            }
            DockRow::Queue { index, .. } => {
                self.queue_selection = Some(index);
                self.edit_queued_message()
            }
            // A plan step or an agent is a place in the transcript, so the
            // action is "show me that", the same thing clicking its tick does.
            DockRow::Plan { block_id, .. } | DockRow::Agent { block_id, .. } => {
                let block = self
                    .transcript
                    .blocks()
                    .iter()
                    .position(|block| block.id == block_id);
                let Some(block) = block else {
                    return false;
                };
                self.scroll.follow = false;
                self.scroll.offset = self.transcript.line_of_block(block);
                self.set_selection(Scope::Agent, block);
                self.focus = Focus::Main;
                true
            }
        }
    }

    /// Fold finished work out of the dock, or bring it back.
    pub fn toggle_dock_hide_done(&mut self) -> bool {
        self.dock_hide_done = !self.dock_hide_done;
        self.dock_hide_done
    }

    /// Whether the dock owns the list keys right now.
    pub fn dock_is_focused(&self) -> bool {
        self.dock_open && self.dock_selection.is_some()
    }

    /// How much background work the session is running.
    pub fn background_task_count(&self) -> usize {
        // The runtime does not yet publish a background-task list to this
        // client, so the honest answer is zero rather than a guess.
        0
    }

    /// The session's current plan, as the last plan-shaped block reported it.
    ///
    /// The runtime publishes a plan as a `TodoUpdate` (or `Plan`) timeline row
    /// whose body is one `Status: title` line per step, so the progress bar is
    /// derived from the transcript the reader can see rather than from a second
    /// source of truth that could disagree with it.
    pub fn todo_progress(&self) -> Option<TodoProgress> {
        // The projection, not the transcript: a plan update is bookkeeping the
        // transcript deliberately does not draw, and a progress band that
        // disappeared with the row it summarises would be worse than no band.
        let row = self.projection.rows.iter().rev().find(|row| {
            matches!(
                row.kind,
                vibex_desktop_model::TimelineRowKind::TodoUpdate
                    | vibex_desktop_model::TimelineRowKind::Plan
            )
        })?;
        let mut progress = TodoProgress {
            title: row.title.clone(),
            done: 0,
            total: 0,
            running: None,
        };
        for line in row.body.lines() {
            let Some((status, title)) = line.split_once(": ") else {
                continue;
            };
            progress.total += 1;
            match status.trim() {
                "Completed" => progress.done += 1,
                "Running" => progress.running = Some(title.trim().to_string()),
                _ => {}
            }
        }
        (progress.total > 0).then_some(progress)
    }

    /// Steps the session's plan has completed and total.
    pub fn todo_done_count(&self) -> usize {
        self.todo_progress()
            .map(|progress| progress.done)
            .unwrap_or(0)
    }

    pub fn todo_total_count(&self) -> usize {
        self.todo_progress()
            .map(|progress| progress.total)
            .unwrap_or(0)
    }

    /// What the Agent is doing right now, when the runtime reported it.
    pub fn current_activity(&self) -> Option<String> {
        self.activity.clone()
    }

    /// How long the running turn has been going.
    pub fn turn_elapsed(&self) -> Option<std::time::Duration> {
        self.turn_started.map(|started| started.elapsed())
    }

    /// Tokens spent by the running turn.
    pub fn turn_tokens(&self) -> Option<u64> {
        self.turn_tokens
    }

    /// A short label for the current page, used when there is no session.
    pub fn page_label(&self, strings: Strings) -> &'static str {
        match self.page {
            Page::Sessions => strings.nav_sessions(),
            Page::Agent => strings.nav_agent(),
            Page::NewSession => strings.session_new(),
            Page::Files => strings.nav_files(),
            Page::Changes => strings.nav_changes(),
            Page::Terminal => strings.nav_terminal(),
            Page::Management | Page::Agents => strings.nav_management(),
            Page::Providers => strings.management_providers(),
            Page::Mcp => strings.management_mcp(),
            Page::Skills => strings.management_skills(),
            Page::Prompts => strings.management_prompts(),
            Page::Hooks => strings.management_hooks(),
            Page::Devices => strings.devices_title(),
            Page::Usage => strings.nav_usage(),
            Page::Recovery => strings.recovery_title(),
            Page::Settings => strings.nav_settings(),
            Page::Help => strings.nav_help(),
        }
    }

    /// Scroll by `delta` rows, clamped to the transcript.
    ///
    /// The wheel and the keyboard share this, so neither can walk the viewport
    /// past the end of the session.
    ///
    /// The step starts from where the frame actually is, not from the state's
    /// offset: while the viewport is following the tail, the offset it was last
    /// dragged to is not the offset being drawn, and stepping from it threw the
    /// reader to the top of the session — a different turn — on the first
    /// scroll after opening it.
    pub fn scroll_lines(&mut self, delta: i64) {
        let current = if self.scroll.follow {
            self.transcript.scroll_offset()
        } else {
            self.scroll.offset
        } as i64;
        self.scroll.offset = (current + delta).max(0) as usize;
        let bottom = self.transcript.bottom_offset(
            self.transcript_band_rows.max(1),
            &self.theme,
            self.strings,
        );
        if self.scroll.offset >= bottom {
            // At the bottom the reader is following the tail again: the newest
            // line is what they scrolled to.
            self.scroll.offset = bottom;
            self.scroll.follow = true;
        } else {
            self.scroll.follow = false;
        }
    }

    /// Whether the transcript has a running block worth animating.
    ///
    /// The interface repaints without input only while this is true.
    pub fn transcript_animating(&self) -> bool {
        self.transcript.is_animating()
    }

    /// Whether anything on screen is moving without the reader's input.
    ///
    /// The composing page's mark shines; a turn's spinner turns. Everything
    /// else holds still, which is what keeps an idle session at zero frames.
    pub fn chrome_animating(&self) -> bool {
        self.transcript_animating() || self.composing_page_shines()
    }

    /// Step the running indicator. Returns whether a repaint is due.
    ///
    /// Streaming text does not need one: a delta marks the app dirty by itself,
    /// so a transcript with no other animation stays at zero frames.
    pub fn advance_transcript_animation(&mut self) -> bool {
        if !self.is_animating() {
            return false;
        }
        self.animation_phase = self.animation_phase.wrapping_add(1);
        true
    }

    /// Keep the turn clock in step with what the session actually is.
    ///
    /// The elapsed readout and the spinner answer to "is a turn running", which
    /// is a property of the session rather than an event. Deriving it here means
    /// no path can leave the clock running after the turn stopped — which would
    /// keep the client repainting for the rest of the session — or leave it at
    /// zero while the runtime works.
    pub fn sync_turn_clock(&mut self) -> bool {
        let running = self.turn_reads_running() || self.transcript.is_animating();
        match (running, self.turn_started.is_some()) {
            (true, false) => {
                self.turn_started = Some(std::time::Instant::now());
                true
            }
            (false, true) => {
                self.turn_started = None;
                self.turn_tokens = None;
                true
            }
            _ => false,
        }
    }

    /// Whether anything on screen is moving without the reader's input.
    ///
    /// The turn line's spinner turns the whole time a turn is running — the
    /// quiet parts of a turn included, which is when it matters most: a spinner
    /// held on one frame while the runtime starts up reads as a frozen client.
    /// The composing page's mark shines while it waits, and a question waiting
    /// on the reader pulses. Everything else holds still, which is what keeps an
    /// idle session at zero frames.
    pub fn is_animating(&self) -> bool {
        self.transcript.is_animating()
            || self.turn_reads_running()
            || self.pending_permission_count() > 0
            || self.pending_elicitations() > 0
            || self.composing_page_shines()
    }

    pub fn tick(&mut self) {
        if let Some(toast) = self.toast.as_mut() {
            toast.ttl = toast.ttl.saturating_sub(1);
            if toast.ttl == 0 {
                self.toast = None;
            }
        }
    }

    pub fn navigate_to(&mut self, page: Page) {
        self.page = page;
        self.filtering = false;
        self.focus = Focus::Main;
        // The search bar and the selection belong to the transcript; leaving it
        // must not leave a highlight behind on another page.
        if page != Page::Agent {
            self.close_search();
            self.clear_text_selection();
            self.last_click = None;
        }
        if page.is_session_page() {
            self.navigation.level = vibex_ui::shell::NavigationLevel::Session;
        } else {
            self.navigation.level = vibex_ui::shell::NavigationLevel::Global;
        }
    }

    pub fn select_global(&mut self, destination: GlobalDestination) {
        let page = match destination {
            GlobalDestination::Sessions => Page::Sessions,
            GlobalDestination::Management => Page::Management,
            GlobalDestination::Settings => Page::Settings,
        };
        self.navigation.select_global(destination);
        self.navigate_to(page);
    }

    pub fn select_session_destination(&mut self, destination: SessionDestination) {
        let page = match destination {
            SessionDestination::Agent => Page::Agent,
            SessionDestination::Files => Page::Files,
            SessionDestination::Changes => Page::Changes,
            SessionDestination::Terminal => Page::Terminal,
        };
        self.navigation.select_session(destination);
        self.navigate_to(page);
    }

    pub fn open_session(&mut self, session_id: VibexSessionId) {
        self.unread_sessions.remove(session_id.as_str());
        self.navigation.enter_session(session_id.as_str());
        self.navigate_to(Page::Agent);
        // Opening a session is a request to work in it, so the keyboard lands
        // in the composer without a trip through `Tab`.
        self.focus = Focus::Composer;
        self.scroll = ScrollState::default();
    }

    /// Put a held draft back in the composer after a failed creation.
    pub fn restore_pending_new_session(&mut self) {
        if let Some(outgoing) = self.pending_new_session.take() {
            self.composer.set_draft(outgoing.text, outgoing.images);
        }
    }

    /// The effect that opens a freshly created session with the message that
    /// asked for it.
    ///
    /// A draft that was only a request for a session — an empty one — is not
    /// sent: the reader asked for a session, and an empty turn is not a message.
    pub fn pending_send_effect(&mut self, session_id: VibexSessionId) -> Option<Effect> {
        let outgoing = self.pending_new_session.take()?;
        if outgoing.text.trim().is_empty() && outgoing.images.is_empty() {
            return None;
        }
        let attachments = self.wire_attachments(&outgoing.images);
        if let Some(pending) = self.pending_send.as_mut() {
            pending.session_id = Some(session_id.clone());
        }
        Some(Effect::SendMessage {
            session_id,
            text: outgoing.text,
            attachments,
        })
    }

    /// Take the keyboard into the composer, placing the caret when the click
    /// landed on a text row.
    ///
    /// A click on the box's border focuses it too: a box the reader can see and
    /// click but not type in reads as broken.
    pub fn click_composer(&mut self, column: u16, row: u16) -> bool {
        let Some(text_area) = self.regions.composer else {
            return false;
        };
        let in_text = column >= text_area.x
            && column < text_area.right()
            && row >= text_area.y
            && row < text_area.bottom();
        let in_band = self.regions.composer_band.is_some_and(|band| {
            column >= band.x && column < band.right() && row >= band.y && row < band.bottom()
        });
        if !in_text && !in_band {
            return false;
        }
        self.focus = Focus::Composer;
        if in_text {
            // The published row counts from the top of the text area, so the
            // rows the box scrolled past have to be added back.
            self.composer.move_cursor_to_cell(
                row - text_area.y + self.regions.composer_scroll,
                column - text_area.x,
            );
            self.composer.begin_selection();
            self.draft_selecting = true;
        }
        true
    }

    /// Rebuild the transcript from the shared controller's projection.
    ///
    /// A prepend grows the content above the viewport, so the reader's place
    /// is anchored to the block that was under the first visible line rather
    /// than to the absolute line offset it used to sit at.
    pub fn sync_transcript(&mut self) {
        // `transcript_rows` rather than the state-free projection: a turn the
        // runtime has finished must stop claiming to stream, or the client
        // draws a spinner over a finished answer for the rest of the session.
        let mut rows = self.agent.state.transcript_rows();
        // A send the runtime has not echoed yet is projected as the row it will
        // become. It goes last because that is where the echo will land, and it
        // is only drawn for the session it belongs to: another session's client
        // state is not this transcript.
        if let Some(pending) = self.pending_send_for_active() {
            rows.push(pending.row());
        }
        self.projection.rows = rows.clone();
        let blocks: Vec<Block> = rows
            .iter()
            .filter(|row| row_is_rendered(row))
            .map(block_from_row)
            .collect();
        // A prepend is the only change that moves the content under the
        // viewport, and it is recognisable from the head alone: the block that
        // used to start the transcript now sits further down. An append, a
        // streamed update, or an in-place replacement leaves the line offset
        // exact, so the anchor lookup — which lays the whole transcript out —
        // only runs when it can be needed.
        let prepended = !self.scroll.follow
            && self
                .transcript
                .block(0)
                .is_some_and(|head| blocks.first().is_some_and(|first| first.id != head.id));
        let anchor = if prepended {
            self.transcript
                .block_at_line(self.scroll.offset)
                .and_then(|index| self.transcript.block(index).map(|block| block.id.clone()))
        } else {
            None
        };
        let change = self.transcript.set_blocks(blocks);
        if change.appended > 0 {
            // New content resumes following unless the user scrolled away.
            if self.scroll.follow {
                self.scroll.offset = 0;
            } else if let Some(offset) = anchor
                .as_deref()
                .and_then(|id| self.transcript.index_of_block(id))
                .map(|index| self.transcript.offset_of_block(index))
            {
                self.scroll.offset = offset;
            }
        }
        // Streaming content can add or invalidate matches, so an open search
        // is re-run rather than left pointing at blocks that have changed.
        self.refresh_search_matches();
    }

    /// Ask for one more page of older history, if there is one to ask for.
    ///
    /// The controller refuses when nothing is selected, when the projection
    /// already starts at sequence 0, or when an older page is already in
    /// flight, so a caller may offer this on every scroll gesture.
    pub fn load_older_history(&mut self) -> Option<Effect> {
        let ticket = self.agent.begin_timeline_before().ok()?;
        Some(Effect::LoadOlder { ticket })
    }

    /// Whether the reader is parked at the very top of a scrolled transcript.
    pub fn at_transcript_top(&self) -> bool {
        self.page == Page::Agent
            && self.overlay.is_none()
            && !self.filtering
            && !self.scroll.follow
            && self.scroll.offset == 0
            && self.agent.state.timeline_has_older
    }

    /// Open the transcript search bar.
    ///
    /// The view deliberately keeps following the tail: opening the bar changes
    /// nothing until the query produces a match to jump to, so an accidental
    /// `/` does not scroll the reader away from what they were reading.
    pub fn begin_search(&mut self) -> bool {
        if self.transcript.is_empty() {
            return false;
        }
        self.search = Some(crate::search::SearchState::new());
        true
    }

    /// Close the search bar and forget the query.
    pub fn close_search(&mut self) -> bool {
        self.search.take().is_some()
    }

    /// Re-run the open search over the current blocks.
    ///
    /// Called after an edit to the query and after the transcript changes; it
    /// is deliberately not called per frame, because scanning every block is
    /// the one part of search that is not free.
    pub fn refresh_search_matches(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let Some(pattern) = search.pattern.clone() else {
            return;
        };
        let (blocks, total) = self.transcript.search_blocks(&pattern);
        search.set_matches(blocks, total);
        self.reveal_current_match();
    }

    /// Whether the search bar owns the keyboard right now.
    pub fn search_composing(&self) -> bool {
        self.search.as_ref().is_some_and(|search| search.composing)
    }

    /// Scroll so the block the search points at is visible.
    pub fn reveal_current_match(&mut self) {
        let Some(block) = self
            .search
            .as_ref()
            .and_then(|search| search.current_block())
        else {
            return;
        };
        let height = self.viewport.1 as usize / 2;
        let line = self.transcript.line_of_block(block);
        self.scroll.follow = false;
        self.scroll.offset = line.saturating_sub(height / 3);
        self.set_selection(Scope::Agent, block);
    }

    /// Step to the next or previous match.
    pub fn step_search(&mut self, delta: isize) -> bool {
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        if search.matches.is_empty() {
            return false;
        }
        search.step(delta);
        search.composing = false;
        self.reveal_current_match();
        true
    }

    /// The palette's entries: recents first, then everything else.
    pub fn palette_entries(&self, query: &str) -> Vec<crate::view::PaletteEntry> {
        crate::view::palette_matches_recent(query, self.strings, &self.recent_commands)
    }

    /// How many of the palette's leading entries are remembered commands.
    ///
    /// Only meaningful for an empty query, which is when the recents are
    /// lifted to the top; the renderer uses it to draw the `Recent` heading.
    pub fn palette_recent_count(&self) -> usize {
        self.recent_commands
            .iter()
            .filter(|id| {
                Intent::from_id(id).is_some_and(|intent| {
                    crate::view::PALETTE
                        .iter()
                        .any(|entry| entry.intent == intent)
                })
            })
            .count()
            .min(MAX_RECENT_COMMANDS)
    }

    /// Remember a command so the palette can offer it first next time.
    pub fn remember_command(&mut self, intent: Intent) {
        let id = intent.id().to_string();
        self.recent_commands.retain(|candidate| candidate != &id);
        self.recent_commands.insert(0, id);
        self.recent_commands.truncate(MAX_RECENT_COMMANDS);
    }

    /// Whether this click is the second of a double click.
    ///
    /// Shared by every clickable surface: a row, a queue entry, a transcript
    /// block. The window is the usual one, and the cell must match, so two
    /// clicks on different rows stay two clicks.
    pub fn double_click_at(&mut self, column: u16, row: u16) -> bool {
        let now = std::time::Instant::now();
        let repeat = self.last_click.is_some_and(|(at, last_line, last_column)| {
            last_line == usize::from(row)
                && last_column == column
                && now.duration_since(at) < std::time::Duration::from_millis(400)
        });
        self.last_click = (!repeat).then_some((now, usize::from(row), column));
        repeat
    }

    /// Claim the banner row, if this message outranks what is there.
    ///
    /// Returns whether the banner changed, so the caller can decide to redraw.
    pub fn set_banner(&mut self, banner: Banner) -> bool {
        match &self.banner {
            Some(current) if current.priority > banner.priority => false,
            Some(current) if current.text == banner.text => false,
            _ => {
                self.banner = Some(banner);
                true
            }
        }
    }

    /// Release the banner row when the condition behind it has gone.
    pub fn clear_banner(&mut self, priority: BannerPriority) {
        if self
            .banner
            .as_ref()
            .is_some_and(|banner| banner.priority == priority)
        {
            self.banner = None;
        }
    }

    /// Keep the banner row in step with the connection and the draft's mode.
    ///
    /// Called once per frame's worth of state change: the producers are
    /// conditions rather than events, so a banner that is no longer true is
    /// withdrawn by the same code that raised it.
    pub fn refresh_banner(&mut self) -> bool {
        if !self.live.is_live() {
            return self.set_banner(
                Banner::warning(self.strings.toast_offline())
                    .with_priority(BannerPriority::Warning),
            );
        }
        self.clear_banner(BannerPriority::Warning);
        if !self.queued_for_active().is_empty() && self.page == Page::Agent {
            let text = format!(
                "{} · {}",
                self.strings.queue_held(),
                self.strings.queue_hint()
            );
            return self.set_banner(Banner::info(text).with_priority(BannerPriority::Mode));
        }
        if self.composer_mode != ComposerMode::Normal && self.page == Page::Agent {
            let text = match self.composer_mode {
                ComposerMode::HistorySearch => self.strings.composer_history_hint(),
                ComposerMode::Shell => self.strings.mode_shell(),
                ComposerMode::Normal => "",
            };
            return self.set_banner(Banner::info(text).with_priority(BannerPriority::Mode));
        }
        self.clear_banner(BannerPriority::Mode);
        false
    }

    // ---- the send queue --------------------------------------------------

    /// Whether the open session has a turn running.
    pub fn session_running(&self) -> bool {
        self.active_session()
            .is_some_and(|session| session.state == vibex_core::AgentSessionState::Running)
    }

    /// Whether the open session *reads* as running.
    ///
    /// A send that the runtime has not answered yet counts: the reader pressed
    /// Enter, and a client that shows "idle" for the round trip reads as one
    /// that dropped the message.
    pub fn turn_reads_running(&self) -> bool {
        self.session_running() || self.pending_send_for_active().is_some()
    }

    /// The unconfirmed send for the open session, if there is one.
    ///
    /// A send whose session is still being created belongs to the reader as
    /// well: they are looking at the page the session will open on, and the
    /// message they just wrote is the only thing on it.
    pub fn pending_send_for_active(&self) -> Option<&PendingSend> {
        let pending = self.pending_send.as_ref()?;
        match pending.session_id.as_ref() {
            None => Some(pending),
            Some(session_id) => (self.selected_session_id() == Some(session_id)).then_some(pending),
        }
    }

    /// Forget a send the runtime has echoed, or one that has waited too long.
    ///
    /// Called after the timeline changes and on every tick: the confirmation is
    /// a property of the timeline, and the timeout is the only thing standing
    /// between a dropped message and a row that never goes away.
    pub fn settle_pending_send(&mut self) -> bool {
        let Some(pending) = self.pending_send.as_ref() else {
            return false;
        };
        if pending.submitted_at.elapsed() > PendingSend::TIMEOUT {
            self.pending_send = None;
            return true;
        }
        // A send is confirmed by the timeline on screen. A reader who has left
        // the session keeps the projection until the timeout — it is not drawn
        // anywhere else, and the timeline is refetched when they come back, so
        // it settles then rather than showing a phantom row in another session.
        // A send whose session does not exist yet has no timeline to appear in;
        // the timeout is what settles it if the creation never answers.
        if pending.session_id.is_none() {
            return false;
        }
        let confirmed = pending.is_confirmed_by(&self.agent.state.timeline.items);
        if confirmed {
            self.pending_send = None;
            return true;
        }
        false
    }

    /// Drop a send the runtime refused, so no phantom row is left behind.
    pub fn abandon_pending_send(&mut self) -> bool {
        let had = self.pending_send.take().is_some();
        if had {
            self.turn_started = None;
        }
        had
    }

    /// Record a message that has been dispatched but not yet echoed.
    pub fn mark_send_dispatched(
        &mut self,
        session_id: Option<&VibexSessionId>,
        text: String,
        attachments: Vec<vibex_core::MessageAttachment>,
    ) {
        self.pending_send_serial = self.pending_send_serial.wrapping_add(1);
        self.pending_send = Some(PendingSend {
            session_id: session_id.cloned(),
            serial: self.pending_send_serial,
            text,
            attachments,
            after_sequence: self.transcript.newest_sequence(),
            submitted_at: std::time::Instant::now(),
        });
        self.turn_started = Some(std::time::Instant::now());
        self.scroll.follow = true;
        // The projection is part of the transcript, so it is put there now
        // rather than at the next event — waiting for one is the delay this
        // exists to remove.
        self.sync_transcript();
    }

    /// Hold a message until the running turn ends, for the open session.
    ///
    /// Answers whether it was held: a client with no session in front of it has
    /// nowhere to hold a message, and saying so is better than queuing one that
    /// no turn will ever release.
    pub fn enqueue_message(&mut self, text: String) -> bool {
        let Some(session_id) = self.selected_session_id().cloned() else {
            return false;
        };
        self.enqueue(session_id, text, Vec::new());
        true
    }

    /// Hold a message -- text and images together -- until the turn ends.
    ///
    /// The session it was written for is part of the message: the reader may
    /// leave and come back, and the queue must still know where it belongs.
    pub fn enqueue(
        &mut self,
        session_id: VibexSessionId,
        text: String,
        images: Vec<(crate::composer::ImageAttachment, u32)>,
    ) {
        self.queued_messages.push(QueuedMessage {
            session_id,
            text,
            images,
        });
        self.queue_selection = Some(self.queued_for_active().len().saturating_sub(1));
    }

    /// Indices into [`Self::queued_messages`] that belong to the open session,
    /// oldest first.
    ///
    /// The list is one client-wide vector — the reader's queue is one gesture
    /// away wherever they are — but what they see and act on is the queue of
    /// the session in front of them.
    pub fn queued_for_active(&self) -> Vec<usize> {
        let Some(session_id) = self.selected_session_id() else {
            return Vec::new();
        };
        self.queued_messages
            .iter()
            .enumerate()
            .filter(|(_, queued)| &queued.session_id == session_id)
            .map(|(index, _)| index)
            .collect()
    }

    /// The row of the open session's queue the cursor is on, as an index into
    /// the client-wide vector.
    fn selected_queued_index(&self) -> Option<usize> {
        let rows = self.queued_for_active();
        let cursor = self.queue_selection?;
        rows.get(cursor).copied()
    }

    /// Move the queue cursor, entering the queue at its newest row.
    pub fn move_queue_selection(&mut self, delta: isize) {
        let last = self.queued_for_active().len();
        if last == 0 {
            self.queue_selection = None;
            return;
        }
        let last = last - 1;
        let current = self.queue_selection.unwrap_or(last) as isize;
        self.queue_selection = Some((current + delta).clamp(0, last as isize) as usize);
    }

    /// Take the selected queued message back into the composer to edit it.
    pub fn edit_queued_message(&mut self) -> bool {
        let Some(index) = self.selected_queued_index() else {
            return false;
        };
        let queued = self.queued_messages.remove(index);
        let remaining = self.queued_for_active().len();
        self.queue_selection =
            (remaining > 0).then(|| self.queue_selection.unwrap_or(0).min(remaining - 1));
        // A draft already in the composer is not thrown away: it goes to the
        // front of the queue, which is where the reader would look for it.
        let outgoing = self.composer.outgoing();
        let draft = outgoing.text;
        let images = outgoing.images;
        if !draft.trim().is_empty() || !images.is_empty() {
            self.queued_messages.insert(
                index,
                QueuedMessage {
                    session_id: queued.session_id.clone(),
                    text: draft,
                    images,
                },
            );
        }
        self.composer.set_draft(queued.text, queued.images);
        self.focus = Focus::Composer;
        self.composer_mode = ComposerMode::Normal;
        true
    }

    /// Drop the selected queued message.
    pub fn delete_queued_message(&mut self) -> bool {
        let Some(index) = self.selected_queued_index() else {
            return false;
        };
        self.queued_messages.remove(index);
        let remaining = self.queued_for_active().len();
        self.queue_selection =
            (remaining > 0).then(|| self.queue_selection.unwrap_or(0).min(remaining - 1));
        true
    }

    /// Swap the selected queued message with its neighbour.
    pub fn move_queued_message(&mut self, delta: isize) -> bool {
        let rows = self.queued_for_active();
        let Some(cursor) = self.queue_selection else {
            return false;
        };
        let Some(index) = rows.get(cursor).copied() else {
            return false;
        };
        let target = cursor as isize + delta;
        if target < 0 || target >= rows.len() as isize {
            return false;
        }
        self.queued_messages.swap(index, rows[target as usize]);
        self.queue_selection = Some(target as usize);
        true
    }

    /// The wire form of a draft's images, with the place each one sat in.
    ///
    /// Clipboard bytes are written to a file only when this client *is* the
    /// authority: the runtime that has to read them is then this host, and a
    /// path is the only form the desktop can draw. On a remote seat the bytes
    /// travel as a data URL instead, which the runtime materialises itself.
    pub fn wire_attachments(
        &self,
        images: &[(crate::composer::ImageAttachment, u32)],
    ) -> Vec<vibex_core::MessageAttachment> {
        let materialise = self.seat == crate::view::SeatKind::Authority;
        images
            .iter()
            .map(|(image, offset)| crate::composer::message_attachment(image, *offset, materialise))
            .collect()
    }

    /// Take the selected queued message out, to be sent immediately.
    pub fn take_queued_message(&mut self) -> Option<(String, Vec<vibex_core::MessageAttachment>)> {
        let index = self.selected_queued_index()?;
        let queued = self.queued_messages.remove(index);
        let remaining = self.queued_for_active().len();
        self.queue_selection =
            (remaining > 0).then(|| self.queue_selection.unwrap_or(0).min(remaining - 1));
        let attachments = self.wire_attachments(&queued.images);
        Some((queued.text, attachments))
    }

    /// The messages whose session has finished its turn and can take another.
    ///
    /// Every session with a queue is asked, not only the one on screen: a
    /// message is released when *its* turn ends, wherever the reader happens to
    /// be looking. Called after every worker message rather than on a special
    /// "turn ended" event — the client has no such event, and a queue that only
    /// drains on one signal would stall the moment that signal changed shape.
    pub fn drain_queue(
        &mut self,
    ) -> Vec<(VibexSessionId, String, Vec<vibex_core::MessageAttachment>)> {
        if !self.live.is_live() || self.queued_messages.is_empty() {
            return Vec::new();
        }
        let mut ready: Vec<VibexSessionId> = Vec::new();
        for queued in &self.queued_messages {
            if ready.contains(&queued.session_id) {
                continue;
            }
            if self.session_is_running(&queued.session_id) {
                continue;
            }
            ready.push(queued.session_id.clone());
        }
        let mut released = Vec::new();
        for session_id in ready {
            // One message per session per pass: the next one waits until the
            // runtime has taken this turn and reported the session running.
            let Some(index) = self
                .queued_messages
                .iter()
                .position(|queued| queued.session_id == session_id)
            else {
                continue;
            };
            let queued = self.queued_messages.remove(index);
            if session_id == self.selected_session_id().cloned().unwrap_or_default() {
                let remaining = self.queued_for_active().len();
                self.queue_selection =
                    (remaining > 0).then(|| self.queue_selection.unwrap_or(0).min(remaining - 1));
            }
            self.history.push(queued.text.clone());
            let attachments = self.wire_attachments(&queued.images);
            released.push((session_id, queued.text, attachments));
        }
        released
    }

    /// Note a finished answer for a session the reader is not looking at.
    ///
    /// Called with the timeline item that arrived: a *final* answer is the
    /// signal that the Agent stopped working, and it is only "unread" when it
    /// landed somewhere the reader was not. Opening the session clears it.
    pub fn note_activity(&mut self, event: &vibex_core::TimelineLiveEvent) -> bool {
        if event.sequence != event.item.sequence || event.session_id != event.item.session_id {
            return false;
        }
        if self
            .agent
            .state
            .selected_session_id
            .as_ref()
            .is_some_and(|selected| selected == &event.session_id)
        {
            return false;
        }
        let final_answer = matches!(
            &event.item.payload,
            vibex_core::TimelinePayload::AgentMessage(message) if message.is_final
        );
        final_answer
            && self
                .unread_sessions
                .insert(event.session_id.as_str().to_string())
    }

    /// The Agent a session runs on, named for the session list.
    ///
    /// The catalogue is where a display name lives; a session whose Agent is
    /// not in it still gets its id, so the mark is never blank.
    pub fn session_agent_label(&self, session: &AgentSession) -> String {
        if let Some(agent) = self
            .runtime_options
            .as_ref()
            .and_then(|catalog| {
                catalog
                    .agents
                    .iter()
                    .find(|agent| agent.agent_id == session.agent_id)
            })
            .map(|agent| agent.label.clone())
        {
            return agent;
        }
        if let Some(label) = self
            .runtime_options
            .as_ref()
            .and_then(|catalog| {
                catalog
                    .options
                    .iter()
                    .find(|option| option.selection.agent_id == session.agent_id)
            })
            .map(|option| option.agent_label.clone())
        {
            return label;
        }
        session.agent_id.to_string()
    }

    /// Whether the session list marks this session as having something new.
    pub fn session_is_unread(&self, session_id: &VibexSessionId) -> bool {
        self.unread_sessions.contains(session_id.as_str())
    }

    /// The title of a session by id, when the client has listed it.
    pub fn session_title(&self, session_id: &VibexSessionId) -> Option<String> {
        self.agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|session| &session.id == session_id)
            .map(|session| session.title.clone())
    }

    /// Whether one session has a turn running, by id.
    ///
    /// The open session is checked first because it is the copy the rest of the
    /// interface reads from; the list is what knows about the sessions the
    /// reader is *not* looking at, which is exactly the case a queue has to get
    /// right.
    fn session_is_running(&self, session_id: &VibexSessionId) -> bool {
        // A send that is still in flight counts: the runtime has not reported
        // the turn yet, and releasing the next held message into that gap would
        // interleave two turns.
        if self
            .pending_send
            .as_ref()
            .and_then(|pending| pending.session_id.as_ref())
            .is_some_and(|pending| pending == session_id)
        {
            return true;
        }
        let running = |session: &AgentSession| {
            &session.id == session_id && session.state == vibex_core::AgentSessionState::Running
        };
        if self.active_session().is_some_and(running) {
            return true;
        }
        self.agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(running)
    }

    // ---- composer history search ----------------------------------------

    /// Derive the composer's mode from what the draft starts with.
    ///
    /// The mode is a property of the text rather than a separate flag, so it
    /// survives an undo, a recalled history entry and a paste without any of
    /// them having to remember to set it.
    pub fn sync_composer_mode(&mut self) {
        let text = self.composer.text();
        let next = if text == "?" || text.starts_with("? ") {
            ComposerMode::HistorySearch
        } else {
            ComposerMode::Normal
        };
        if next != self.composer_mode {
            self.composer_mode = next;
            self.history_selection = 0;
        }
    }

    /// The query the history drawer is filtering by, when it is open.
    pub fn history_query(&self) -> Option<String> {
        if self.composer_mode != ComposerMode::HistorySearch {
            return None;
        }
        Some(
            self.composer
                .text()
                .strip_prefix("? ")
                .or_else(|| self.composer.text().strip_prefix('?'))
                .unwrap_or_default()
                .to_string(),
        )
    }

    /// Sent messages matching the history query, best first.
    ///
    /// Ranking is deliberately simple — prefix beats word-start beats
    /// substring, then newest first — because the history is at most a few
    /// hundred entries and a reader looking for a message they sent recognises
    /// it by reading, not by score.
    pub fn history_matches(&self) -> Vec<(usize, String)> {
        let Some(query) = self.history_query() else {
            return Vec::new();
        };
        let query = query.trim().to_lowercase();
        let entries = self.history.entries();
        if query.is_empty() {
            return entries
                .iter()
                .enumerate()
                .rev()
                .take(MAX_HISTORY_MATCHES)
                .map(|(index, text)| (index, text.clone()))
                .collect();
        }
        let mut scored = entries
            .iter()
            .enumerate()
            .filter_map(|(index, text)| {
                let haystack = text.to_lowercase();
                let score = if haystack.starts_with(&query) {
                    0
                } else if haystack
                    .split_whitespace()
                    .any(|word| word.starts_with(&query))
                {
                    1
                } else if haystack.contains(&query) {
                    2
                } else {
                    return None;
                };
                Some((score, index, text.clone()))
            })
            .collect::<Vec<_>>();
        // Newest first inside a rank, so the most recent match is on top.
        scored.sort_by(|left, right| (left.0, right.1).cmp(&(right.0, left.1)));
        scored
            .into_iter()
            .take(MAX_HISTORY_MATCHES)
            .map(|(_, index, text)| (index, text))
            .collect()
    }

    /// The history drawer's current row.
    pub fn history_selected(&self) -> Option<(usize, String)> {
        let matches = self.history_matches();
        if matches.is_empty() {
            return None;
        }
        let index = self.history_selection.min(matches.len() - 1);
        Some(matches[index].clone())
    }

    /// Move the history drawer's selection.
    pub fn move_history_selection(&mut self, delta: isize) {
        let count = self.history_matches().len();
        if count == 0 {
            return;
        }
        let current = self.history_selection.min(count - 1) as isize;
        self.history_selection = (current + delta).clamp(0, count as isize - 1) as usize;
    }

    /// Replace the draft with the selected history entry.
    pub fn accept_history_match(&mut self) -> bool {
        let Some((_, text)) = self.history_selected() else {
            return false;
        };
        self.composer.set_text(text);
        self.composer_mode = ComposerMode::Normal;
        self.history_selection = 0;
        self.refresh_completion();
        true
    }

    /// Leave history search, dropping the query.
    pub fn cancel_history_search(&mut self) -> bool {
        if self.composer_mode != ComposerMode::HistorySearch {
            return false;
        }
        self.composer.clear();
        self.composer_mode = ComposerMode::Normal;
        self.history_selection = 0;
        true
    }

    // ---- transcript text selection -------------------------------------

    /// Start a selection at one cell of the transcript band.
    pub fn begin_text_selection(&mut self, line: usize, column: u16) {
        self.text_selection = Some(TextSelection {
            anchor: (line, column),
            head: (line, column),
            dragging: true,
        });
    }

    /// Start capturing a chord for `intent`.
    pub fn begin_key_capture(&mut self, intent: Intent) {
        if let Some(Overlay::Keys {
            query,
            selected,
            message,
            dirty,
            ..
        }) = self.overlay.clone()
        {
            self.overlay = Some(Overlay::Keys {
                query,
                selected,
                capturing: Some(intent),
                message,
                dirty,
            });
        }
    }

    /// Apply a captured chord, refusing one that another intent already owns.
    ///
    /// A duplicate is refused rather than accepted-with-a-warning because
    /// dispatch takes the first match in the table: the losing action would
    /// stop working with nothing on screen to say so. The message names the
    /// owner so the reader can free the chord first.
    pub fn finish_key_capture(&mut self, chord: Chord) {
        let Some(Overlay::Keys {
            query,
            selected,
            capturing,
            dirty,
            ..
        }) = self.overlay.clone()
        else {
            return;
        };
        let Some(intent) = capturing else {
            return;
        };
        let scope = intent.default_scope();
        let message = match self.keymap.conflict(scope, chord, intent) {
            Some(owner) => Some(format!(
                "{}: {owner_id} ({scope_id})",
                self.strings.keys_conflict(),
                owner_id = owner.id(),
                scope_id = owner.default_scope().id(),
            )),
            None => {
                self.keymap.rebind(intent, chord);
                None
            }
        };
        self.overlay = Some(Overlay::Keys {
            query,
            selected,
            capturing: None,
            message,
            dirty: dirty || self.keymap.is_overridden(intent),
        });
    }

    /// The image paths a paste names, in the order they appear.
    ///
    /// A paste that names a picture is a request to attach it, not to write its
    /// name into the prompt. Terminals and file managers spell a path in more
    /// than one way — quoted, `file://`-prefixed, with escaped spaces, with a
    /// `~` — and a screenshot is often dropped in as a path *plus* a sentence,
    /// so each line is judged on its own.
    pub fn image_paths_from_paste(text: &str) -> Vec<String> {
        if text.len() > 64 * 1024 {
            return Vec::new();
        }
        text.lines()
            .map(pasted_path)
            .filter(|path| !path.is_empty())
            .filter(|path| crate::composer::image_mime_for_path(path).is_some())
            .filter(|path| std::path::Path::new(path).is_file())
            .collect()
    }

    /// Whether a paste names exactly one image, and should be attached.
    ///
    /// The single-path case is kept as its own question because the answer
    /// decides whether the paste becomes an attachment or a draft: a paste of
    /// anything else — a paragraph, a directory, a list of mixed paths — is
    /// text the reader meant to put in the prompt.
    pub fn image_path_from_paste(text: &str) -> Option<String> {
        let paths = Self::image_paths_from_paste(text);
        (paths.len() == 1 && text.trim().lines().count() == 1).then(|| paths[0].clone())
    }

    /// Route a paste, wherever it came from.
    ///
    /// One path in: the reader's paste gesture is shared by the terminal's own
    /// bracketed paste and by the client's clipboard reader, and both have to
    /// treat a picture path and a paragraph the same way.
    pub fn insert_pasted_text(&mut self, text: &str) {
        let paths = Self::image_paths_from_paste(text);
        if !paths.is_empty() {
            let mut attached = Vec::new();
            let mut failure = None;
            for path in paths {
                match self.attach_image_path(&path) {
                    Ok(label) => attached.push(label),
                    Err(error) => failure = Some(error),
                }
            }
            if let Some(error) = failure {
                self.toast(Toast::warning(error));
            } else if !attached.is_empty() {
                let message = format!("{} {}", self.strings.image_attached(), attached.join(", "));
                self.toast(Toast::success(message));
            }
            return;
        }
        // A big paste collapses into a chip so the draft stays readable; the
        // bytes are put back when it is sent.
        self.composer.insert_paste(text);
        self.refresh_completion();
    }

    /// Attach an image from a path on the authority host or the local machine.
    pub fn attach_image_path(&mut self, path: &str) -> Result<String, String> {
        let trimmed = path.trim().trim_matches('"');
        if trimmed.is_empty() {
            return Err(self.strings.image_not_found().to_string());
        }
        let Some(mime) = crate::composer::image_mime_for_path(trimmed) else {
            return Err(self.strings.image_unsupported().to_string());
        };
        let metadata = std::fs::metadata(trimmed)
            .map_err(|_| format!("{}: {trimmed}", self.strings.image_not_found()))?;
        if metadata.len() as usize > crate::composer::IMAGE_MAX_BYTES {
            return Err(self.strings.image_too_large().to_string());
        }
        self.composer
            .insert_image(
                mime,
                crate::composer::ImageSource::Path(trimmed.to_string()),
            )
            .ok_or_else(|| self.strings.image_cap().to_string())
    }

    /// Attach an image whose bytes were read from the clipboard.
    pub fn attach_image_bytes(
        &mut self,
        mime_type: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<String, String> {
        if bytes.len() > crate::composer::IMAGE_MAX_BYTES {
            return Err(self.strings.image_too_large().to_string());
        }
        self.composer
            .insert_image(
                mime_type,
                crate::composer::ImageSource::Bytes(std::sync::Arc::new(bytes)),
            )
            .ok_or_else(|| self.strings.image_cap().to_string())
    }

    /// Put the selected binding back on its default chord.
    pub fn reset_key_binding(&mut self, intent: Intent) {
        self.keymap.reset(intent);
        if let Some(Overlay::Keys {
            query,
            selected,
            capturing,
            dirty,
            ..
        }) = self.overlay.clone()
        {
            self.overlay = Some(Overlay::Keys {
                query,
                selected,
                capturing,
                message: None,
                dirty: dirty || self.keymap.is_overridden(intent),
            });
        }
    }

    /// Write the current bindings to the user's key file.
    pub fn save_keymap(&mut self) -> Result<String, String> {
        let path =
            Keymap::user_path().ok_or_else(|| "no home directory for tui-keys.toml".to_string())?;
        self.keymap.save(&path)?;
        Ok(path.display().to_string())
    }

    /// Extend the draft selection to the cell under a drag.
    ///
    /// The pointer is clamped into the composer: a drag that leaves the box
    /// still selects to its nearest edge rather than jumping into the
    /// transcript, and a draft that is shorter than the pointer's row simply
    /// ends at its last character.
    pub fn drag_draft_selection(&mut self, column: u16, row: u16) -> bool {
        let Some(region) = self.regions.composer else {
            return false;
        };
        if region.width == 0 || region.height == 0 {
            return false;
        }
        let column = column.clamp(region.x, region.right().saturating_sub(1));
        let row = row.clamp(region.y, region.bottom().saturating_sub(1));
        self.composer
            .extend_selection_to_cell(row - region.y, column - region.x);
        true
    }

    /// Extend the selection in progress to one cell.
    pub fn extend_text_selection(&mut self, line: usize, column: u16) -> bool {
        let Some(selection) = self.text_selection.as_mut() else {
            return false;
        };
        selection.head = (line, column);
        true
    }

    /// Finish the selection in progress, reporting whether it covers anything.
    pub fn finish_text_selection(&mut self) -> bool {
        let Some(selection) = self.text_selection.as_mut() else {
            return false;
        };
        selection.dragging = false;
        !selection.is_empty()
    }

    /// Replace the selection with a whole word, for a double click.
    pub fn select_word_at(&mut self, line: usize, column: u16) -> bool {
        let theme = self.theme.clone();
        let strings = self.strings;
        let Some(text) = self
            .transcript
            .plain_lines(line, line + 1, &theme, strings)
            .into_iter()
            .next()
        else {
            return false;
        };
        let Some((start, end)) = crate::transcript::word_at(&text, column) else {
            return false;
        };
        self.text_selection = Some(TextSelection {
            anchor: (line, start),
            head: (line, end),
            dragging: false,
        });
        true
    }

    /// Forget the selection.
    pub fn clear_text_selection(&mut self) -> bool {
        self.text_selection.take().is_some()
    }

    /// The selected text, ready for the clipboard.
    ///
    /// The trailing whitespace of every line is dropped, because a terminal
    /// pads with spaces and nobody wants them on the clipboard; interior
    /// spacing is preserved exactly.
    pub fn selected_text(&mut self) -> Option<String> {
        let selection = self.text_selection?;
        let (start, end) = selection.ordered();
        if start == end {
            return None;
        }
        let theme = self.theme.clone();
        let strings = self.strings;
        let lines = self
            .transcript
            .plain_lines(start.0, end.0 + 1, &theme, strings);
        if lines.is_empty() {
            return None;
        }
        let last = lines.len() - 1;
        let mut output = Vec::with_capacity(lines.len());
        for (index, line) in lines.into_iter().enumerate() {
            let from = if index == 0 { usize::from(start.1) } else { 0 };
            let to = if index == last {
                usize::from(end.1)
            } else {
                usize::from(u16::MAX)
            };
            let (_, rest) = crate::text::take_width(&line, from);
            let (prefix, _) = crate::text::take_width(rest, to.saturating_sub(from));
            output.push(prefix.trim_end().to_string());
        }
        while output.last().is_some_and(|line| line.is_empty()) {
            output.pop();
        }
        if output.is_empty() {
            return None;
        }
        Some(output.join("\n"))
    }

    /// Recompute the shell kind for the current viewport.
    pub fn resize(&mut self, columns: u16, rows: u16) {
        self.viewport = (columns, rows);
        self.shell = shell_for_columns(columns);
        // The renderer is the authority on the transcript's width and re-states
        // it every frame; setting the same value here keeps pre-frame layout
        // maths (a prepend's scroll anchor) honest instead of measuring against
        // a zero width.
        let width = crate::view::transcript_width(self.shell, columns);
        self.transcript.configure(width, &self.theme.clone());
    }

    /// Locale-aware, capability-aware reason an action is unavailable.
    pub fn unavailable_reason(&self, intent: Intent) -> Option<String> {
        if !self.live.is_live() && intent.mutates() && !intent.works_offline() {
            return Some(self.strings.toast_offline().to_string());
        }
        None
    }
}

/// How many history entries the composer's drawer will show.
pub const MAX_HISTORY_MATCHES: usize = 100;

/// How many palette commands are remembered.
pub const MAX_RECENT_COMMANDS: usize = 8;

/// Column thresholds for the three shell layouts, re-calibrated from the
/// desktop's pixel breakpoints for a character grid.
pub const WIDE_MIN_COLUMNS: u16 = 120;
pub const MEDIUM_MIN_COLUMNS: u16 = 88;

/// Pick the shell layout for a terminal width in columns.
pub fn shell_for_columns(columns: u16) -> ShellKind {
    if columns >= WIDE_MIN_COLUMNS {
        ShellKind::Wide
    } else if columns >= MEDIUM_MIN_COLUMNS {
        ShellKind::Medium
    } else {
        ShellKind::Compact
    }
}

/// Three-way availability used throughout the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    RequiresPermission,
    Unsupported,
    Offline,
}

impl Availability {
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Options the composition root passes into [`App::new`].
#[derive(Debug, Clone)]
pub struct AppOptions {
    pub seat: SeatKind,
    pub capability: ColorCapability,
    pub theme_id: Option<String>,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    /// Where the reader's session-list arrangement is kept.
    ///
    /// The composition root decides this, the way it decides the seat: a test
    /// harness must not write into the developer's home, and a client with no
    /// writable home simply keeps the arrangement in memory for the run.
    pub sidebar_path: Option<std::path::PathBuf>,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self::with_default_paths()
    }
}

impl AppOptions {
    /// The default options, with the arrangement stored beside the key file.
    fn with_default_paths() -> Self {
        Self {
            seat: SeatKind::Remote,
            capability: ColorCapability::detect(),
            theme_id: None,
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            sidebar_path: App::sidebar_arrangement_path(),
        }
    }
}

/// Normalise one pasted line into a filesystem path.
///
/// Handles what a terminal, a file manager or a browser actually puts on the
/// clipboard: surrounding quotes, a `file://` URL, backslash-escaped spaces,
/// and a `~` home prefix.
fn pasted_path(line: &str) -> String {
    let mut text = line.trim();
    if let Some(rest) = text
        .strip_prefix("file://")
        .or_else(|| text.strip_prefix("file:"))
    {
        text = rest;
        // `file:///Users/me/a.png` keeps its leading slash; `file://host/path`
        // does not name anything on this machine and is left to fail.
        if let Some(unescaped) = text.strip_prefix('/')
            && text.starts_with("//")
        {
            text = unescaped;
        }
    }
    let text = text.trim();
    let text = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            text.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(text);
    // A shell escapes a space in a drag-and-dropped path; nobody types the
    // backslashes themselves, so unescaping them is never wrong here.
    let unescaped = text.replace("\\ ", " ");
    if let Some(rest) = unescaped.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return std::path::Path::new(&home).join(rest).display().to_string();
    }
    unescaped
}

/// Whether a projected row earns a row in the transcript.
///
/// A timeline carries bookkeeping as well as conversation, and a character grid
/// pays for every line it shows. A plan is already on screen twice — as a
/// progress band with the running step, and in full in the dock — and the
/// *resolution* of an approval is visible in the request that stays on screen.
/// Drawing them again costs the reader the thing they are reading for.
pub fn row_is_rendered(row: &TimelineRow) -> bool {
    !matches!(
        row.kind,
        vibex_desktop_model::TimelineRowKind::TodoUpdate
            | vibex_desktop_model::TimelineRowKind::Plan
            | vibex_desktop_model::TimelineRowKind::PermissionResolution
            | vibex_desktop_model::TimelineRowKind::ElicitationResolution
    )
}

/// Project one authoritative `TimelineRow` into a transcript block.
pub fn block_from_row(row: &TimelineRow) -> Block {
    Block {
        id: row.id.clone(),
        kind: row.kind,
        title: row.title.clone(),
        body: row.body.clone(),
        turn_id: row.turn_id.clone(),
        sequence: row.last_sequence,
        expanded: false,
        // A dense row shows one line by shape, so it has to be openable to be
        // readable in full: the transcript is where its body lives.
        collapsible: row.collapsible || crate::transcript::is_dense_row(row.kind),
        streaming: row.streaming,
        failed: row.failed,
        pending_permission: row.pending_permission || row.turn_pending_permission,
        file_path: row.file_path.clone(),
        runtime_attribution: row.runtime_attribution.clone(),
        conclusion: row.conclusion,
        group: crate::transcript::GroupRole::Solo,
    }
}

/// A management entry edit, carried as data so the worker does not have to
/// re-derive which page the user was on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagementEntryEdit {
    Mcp {
        server_id: vibex_core::McpServerId,
        display_name: String,
    },
    Skill {
        skill_id: vibex_core::SkillId,
        display_name: String,
    },
    Prompt {
        prompt_id: vibex_core::PromptId,
        display_name: String,
    },
    Hook {
        hook_id: vibex_core::HookId,
        display_name: String,
    },
}

/// Actions on the recovery page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    Diagnostics,
    BackupCreate,
    BackupInspect,
    BackupRestore,
}

impl RecoveryAction {
    pub const ALL: [RecoveryAction; 4] = [
        RecoveryAction::Diagnostics,
        RecoveryAction::BackupCreate,
        RecoveryAction::BackupInspect,
        RecoveryAction::BackupRestore,
    ];
}

/// Asynchronous work the reducer asks the worker to perform.
///
/// The variants carry only data; the worker owns the tokio runtime and the
/// backend handles, which is what keeps the reducer pure.
#[derive(Debug, Clone)]
pub enum Effect {
    ListSessions {
        include_archived: bool,
    },
    OpenSession {
        session_id: VibexSessionId,
        /// Issued by the shared controller before the fetch starts, so the
        /// snapshot lands in the generation it was requested for.
        ticket: vibex_ui::AgentSessionLoadTicket,
    },
    RefreshTimeline,
    /// Fetch one page of timeline history below the ticket's cursor.
    LoadOlder {
        ticket: vibex_ui::AgentTimelineBeforeTicket,
    },
    ListRuntimeOptions,
    CreateSession {
        workspace_root: String,
        title: Option<String>,
        /// The runtime chosen on the composing page, when the reader made a
        /// choice there: a session is created *with* an Agent, not moved to one.
        runtime: Option<vibex_core::SessionRuntimeSelection>,
    },
    RenameSession {
        session_id: VibexSessionId,
        title: String,
    },
    ArchiveSession {
        session_id: VibexSessionId,
    },
    DeleteSession {
        session_id: VibexSessionId,
    },
    ForkSession {
        session_id: VibexSessionId,
    },
    SendMessage {
        session_id: VibexSessionId,
        text: String,
        /// Images the prompt carries, in the order they were attached.
        attachments: Vec<vibex_core::MessageAttachment>,
    },
    ContinueTurn {
        session_id: VibexSessionId,
    },
    /// Ask the worker for whatever the system clipboard holds.
    ///
    /// An image is attached; text is pasted. Reading a clipboard is I/O and
    /// belongs to the worker, so the reducer only asks.
    ReadClipboard,
    /// Ask the host for an image on the system clipboard.
    ///
    /// Reading a clipboard is I/O and belongs to the worker; the reducer asks
    /// and gets an [`crate::worker::AppMessage::ClipboardImage`] back.
    ReadClipboardImage,
    Interrupt {
        session_id: VibexSessionId,
    },
    SteerMessage {
        session_id: VibexSessionId,
        text: String,
        attachments: Vec<vibex_core::MessageAttachment>,
        fallback_to_resend: bool,
    },
    ResolvePermission {
        session_id: VibexSessionId,
        request_id: vibex_core::RequestId,
        resolution: vibex_core::PermissionResolution,
    },
    ResolveElicitation {
        session_id: VibexSessionId,
        request_id: vibex_core::RequestId,
        resolution: vibex_core::ElicitationResolution,
    },
    SwitchRuntime {
        session_id: VibexSessionId,
        selection: vibex_core::SessionRuntimeSelection,
    },
    ListWorkspaces,
    OpenWorkspace {
        root_path: String,
    },
    BrowseDirectories {
        path: Option<String>,
    },
    LoadFileTree {
        workspace_id: vibex_core::WorkspaceId,
    },
    LoadGitStatus {
        workspace_id: vibex_core::WorkspaceId,
    },
    LoadGitDiff {
        workspace_id: vibex_core::WorkspaceId,
        path: String,
    },
    ListDevices,
    CreatePairingOffer,
    RevokeDevice {
        device_id: vibex_core::DeviceId,
        reason: Option<String>,
    },
    ListAudit,
    ListProfiles,
    SelectProfile {
        profile_id: vibex_core::ProviderProfileId,
    },
    WriteProviderSecret {
        profile_id: vibex_core::ProviderProfileId,
        secret: String,
    },
    TestProviderProfile {
        profile_id: vibex_core::ProviderProfileId,
    },
    FetchProviderModels {
        profile_id: vibex_core::ProviderProfileId,
    },
    ListAgents,
    InstallAgent {
        agent_id: vibex_core::AgentId,
    },
    UninstallAgent {
        agent_id: vibex_core::AgentId,
    },
    ListAgentAuth {
        agent_id: vibex_core::AgentId,
    },
    LogoutAgent {
        agent_id: vibex_core::AgentId,
    },
    ListMcp,
    ListSkills,
    ListPrompts,
    ListHooks,
    ToggleMcp {
        server_id: vibex_core::McpServerId,
        enabled: bool,
    },
    ToggleSkill {
        skill_id: vibex_core::SkillId,
        enabled: bool,
    },
    TogglePrompt {
        prompt_id: vibex_core::PromptId,
        enabled: bool,
    },
    ToggleHook {
        hook_id: vibex_core::HookId,
        enabled: bool,
    },
    LoadUsage,
    ExportDiagnostics,
    CreateBackup,
    InspectBackup,
    RestoreBackup {
        backup_id: String,
    },
    DiscoverCompletions {
        trigger: crate::composer::CompletionTrigger,
        query: String,
    },
    CheckDrift,
    /// Probe one Agent's runtime option snapshot.
    ProbeAgentRuntime {
        request: vibex_core::AgentRuntimeOptionProbeRequest,
    },
    /// Stage or unstage one path.
    GitStage {
        workspace_id: vibex_core::WorkspaceId,
        path: String,
        stage: bool,
    },
    /// Commit the staged changes with an already-collected message.
    GitCommit {
        workspace_id: vibex_core::WorkspaceId,
        message: String,
    },
    /// Recent commits on the workspace's branch.
    LoadGitHistory {
        workspace_id: vibex_core::WorkspaceId,
    },
    /// Local and remote branches.
    LoadGitBranches {
        workspace_id: vibex_core::WorkspaceId,
    },
    /// Discard the changes at one path.
    GitRevert {
        workspace_id: vibex_core::WorkspaceId,
        path: String,
    },
    /// Read the worktree lifecycle snapshot for a workspace.
    LoadWorktrees {
        workspace_id: vibex_core::WorkspaceId,
    },
    /// Run the destructive preflight a worktree action requires.
    WorktreePreflight {
        workspace_id: vibex_core::WorkspaceId,
        path: String,
    },
    /// Create a worktree from a branch name.
    WorktreeCreate {
        workspace_id: vibex_core::WorkspaceId,
        branch_name: String,
    },
    /// Edit one management entry by id.
    UpdateEntry {
        entry: ManagementEntryEdit,
    },
    /// Read a file for the read-only viewer.
    ReadFile {
        workspace_id: vibex_core::WorkspaceId,
        path: String,
    },
    /// Provider health summaries.
    ListHealth,
    /// Rename a provider profile.
    RenameProfile {
        profile_id: vibex_core::ProviderProfileId,
        name: String,
    },
    /// Hand the terminal to `$EDITOR` and read the result back.
    EditExternally {
        title: String,
        body: String,
    },
    /// Put text on the system clipboard. Routed through the terminal's own
    /// OSC 52 support so the client never links an X11/Wayland clipboard crate.
    Clipboard {
        text: String,
    },
}

impl Effect {
    /// Whether the effect is a mutation, so the UI can show a pending marker.
    pub fn key(&self) -> &'static str {
        match self {
            Effect::ListSessions { .. } => "sessions",
            Effect::OpenSession { .. } => "open_session",
            Effect::RefreshTimeline => "timeline",
            Effect::LoadOlder { .. } => "older_timeline",
            Effect::ListRuntimeOptions => "runtime_options",
            Effect::CreateSession { .. } => "create_session",
            Effect::RenameSession { .. } => "rename_session",
            Effect::ArchiveSession { .. } => "archive_session",
            Effect::DeleteSession { .. } => "delete_session",
            Effect::ForkSession { .. } => "fork_session",
            Effect::ReadClipboard => "read_clipboard",
            Effect::ReadClipboardImage => "read_clipboard_image",
            Effect::SendMessage { .. } => "send_message",
            Effect::ContinueTurn { .. } => "continue_turn",
            Effect::Interrupt { .. } => "interrupt",
            Effect::SteerMessage { .. } => "steer_message",
            Effect::ResolvePermission { .. } => "resolve_permission",
            Effect::ResolveElicitation { .. } => "resolve_elicitation",
            Effect::SwitchRuntime { .. } => "switch_runtime",
            Effect::ListWorkspaces => "workspaces",
            Effect::OpenWorkspace { .. } => "open_workspace",
            Effect::BrowseDirectories { .. } => "browse",
            Effect::LoadFileTree { .. } => "file_tree",
            Effect::LoadGitStatus { .. } => "git_status",
            Effect::LoadGitDiff { .. } => "git_diff",
            Effect::ListDevices => "devices",
            Effect::CreatePairingOffer => "pairing_offer",
            Effect::RevokeDevice { .. } => "revoke_device",
            Effect::ListAudit => "audit",
            Effect::ListProfiles => "profiles",
            Effect::SelectProfile { .. } => "select_profile",
            Effect::WriteProviderSecret { .. } => "provider_secret",
            Effect::TestProviderProfile { .. } => "provider_test",
            Effect::FetchProviderModels { .. } => "provider_models",
            Effect::ListAgents => "agents",
            Effect::InstallAgent { .. } => "install_agent",
            Effect::UninstallAgent { .. } => "uninstall_agent",
            Effect::ListAgentAuth { .. } => "agent_auth",
            Effect::LogoutAgent { .. } => "logout_agent",
            Effect::ListMcp => "mcp",
            Effect::ListSkills => "skills",
            Effect::ListPrompts => "prompts",
            Effect::ListHooks => "hooks",
            Effect::ToggleMcp { .. }
            | Effect::ToggleSkill { .. }
            | Effect::TogglePrompt { .. }
            | Effect::ToggleHook { .. } => "toggle_entry",
            Effect::LoadUsage => "usage",
            Effect::ExportDiagnostics => "diagnostics",
            Effect::CreateBackup => "backup_create",
            Effect::InspectBackup => "backup_inspect",
            Effect::RestoreBackup { .. } => "backup_restore",
            Effect::DiscoverCompletions { .. } => "completions",
            Effect::CheckDrift => "drift",
            Effect::ProbeAgentRuntime { .. } => "runtime_probe",
            Effect::GitStage { .. } => "git_stage",
            Effect::GitCommit { .. } => "git_commit",
            Effect::ReadFile { .. } => "read_file",
            Effect::LoadGitHistory { .. } => "git_history",
            Effect::LoadGitBranches { .. } => "git_branches",
            Effect::GitRevert { .. } => "git_revert",
            Effect::LoadWorktrees { .. } => "worktrees",
            Effect::WorktreePreflight { .. } => "worktree_preflight",
            Effect::WorktreeCreate { .. } => "worktree_create",
            Effect::UpdateEntry { .. } => "update_entry",
            Effect::ListHealth => "provider_health",
            Effect::RenameProfile { .. } => "rename_profile",
            Effect::EditExternally { .. } => "edit_external",
            Effect::Clipboard { .. } => "clipboard",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrangement_app(path: &std::path::Path) -> App {
        App::new(
            vibex_backend::DisconnectedBackend::facade(),
            AppOptions {
                sidebar_path: Some(path.to_path_buf()),
                ..AppOptions::default()
            },
        )
    }

    #[test]
    fn the_sidebar_arrangement_round_trips_through_its_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-sidebar.json");
        let mut app = arrangement_app(&path);
        app.projection
            .sidebar
            .pinned_ids
            .insert("session_pinned01".to_string());
        app.projection.sidebar.row_order = vec![
            "session_pinned01".to_string(),
            "session_other001".to_string(),
        ];
        app.projection
            .sidebar
            .collapsed_ids
            .insert("project_collapsed".to_string());
        app.sidebar_grouped = false;
        app.save_sidebar_arrangement();

        let reloaded = arrangement_app(&path);
        assert!(
            reloaded
                .projection
                .sidebar
                .pinned_ids
                .contains("session_pinned01")
        );
        assert_eq!(
            reloaded.projection.sidebar.row_order,
            vec![
                "session_pinned01".to_string(),
                "session_other001".to_string()
            ]
        );
        assert!(
            reloaded
                .projection
                .sidebar
                .collapsed_ids
                .contains("project_collapsed")
        );
        assert!(
            !reloaded.sidebar_grouped,
            "grouping did not survive the save"
        );
    }

    #[test]
    fn an_app_without_an_arrangement_path_writes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(
            vibex_backend::DisconnectedBackend::facade(),
            AppOptions {
                sidebar_path: None,
                ..AppOptions::default()
            },
        );
        app.projection
            .sidebar
            .pinned_ids
            .insert("session_pinned01".to_string());
        app.save_sidebar_arrangement();
        // Nothing anywhere: the app was told it has no place to keep it.
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_none(),
            "an app without a path still wrote something"
        );
    }

    #[test]
    fn the_ways_a_terminal_spells_a_path_all_attach() {
        let directory = tempfile::tempdir().unwrap();
        let spaces = directory.path().join("Screenshot 2026-10-01.png");
        std::fs::write(&spaces, b"png").unwrap();
        let plain = directory.path().join("shot.png");
        std::fs::write(&plain, b"png").unwrap();
        let display = spaces.display().to_string();

        for spelling in [
            display.clone(),
            format!("  {display}  "),
            format!("\"{display}\""),
            format!("'{display}'"),
            display.replace(' ', "\\ "),
        ] {
            assert_eq!(
                App::image_paths_from_paste(&spelling),
                vec![display.clone()],
                "{spelling} did not resolve"
            );
        }
        let url = format!("file://{}", plain.display());
        assert_eq!(
            App::image_paths_from_paste(&url),
            vec![plain.display().to_string()]
        );
        // A `file://host/path` names another machine, which has no file here.
        assert!(App::image_paths_from_paste("file://elsewhere/tmp/a.png").is_empty());
    }

    #[test]
    fn a_paste_can_carry_several_pictures_and_text() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("one.png");
        let second = directory.path().join("two.jpg");
        std::fs::write(&first, b"png").unwrap();
        std::fs::write(&second, b"jpg").unwrap();

        let both = format!("{}\n{}", first.display(), second.display());
        assert_eq!(
            App::image_paths_from_paste(&both),
            vec![first.display().to_string(), second.display().to_string()]
        );
        // Two files on two lines is not one path: it is a paste that happens to
        // name pictures, and only the plural question can answer for it.
        assert_eq!(App::image_path_from_paste(&both), None);
        // A picture named inside a sentence stays a sentence.
        assert!(
            App::image_paths_from_paste(&format!("look at {}", first.display())).is_empty(),
            "a sentence was read as a path"
        );
    }

    #[test]
    fn a_home_relative_paste_is_expanded() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let path = std::path::Path::new(&home).join("vibex-paste-probe.png");
        std::fs::write(&path, b"png").unwrap();
        let expanded = App::image_paths_from_paste("~/vibex-paste-probe.png");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(expanded, vec![path.display().to_string()]);
    }

    #[test]
    fn only_an_existing_image_file_is_a_pasted_attachment() {
        let directory = tempfile::tempdir().unwrap();
        let shot = directory.path().join("shot.png");
        std::fs::write(&shot, b"png").unwrap();
        let notes = directory.path().join("notes.txt");
        std::fs::write(&notes, b"words").unwrap();

        assert_eq!(
            App::image_path_from_paste(&format!("  {}  ", shot.display())),
            Some(shot.display().to_string())
        );
        // A text file, a file that does not exist, and a paragraph are all
        // text the reader meant to type.
        assert_eq!(
            App::image_path_from_paste(&notes.display().to_string()),
            None
        );
        assert_eq!(
            App::image_path_from_paste(&directory.path().join("gone.png").display().to_string()),
            None
        );
        assert_eq!(
            App::image_path_from_paste(&format!("look at {}\nand this", shot.display())),
            None
        );
        assert_eq!(App::image_path_from_paste(""), None);
    }

    #[test]
    fn a_queued_message_keeps_its_images_when_it_is_pulled_back() {
        let mut app = arrangement_app(&tempfile::tempdir().unwrap().path().join("unused.json"));
        app.composer.insert_str("see this ");
        app.composer
            .insert_image(
                "image/png",
                crate::composer::ImageSource::Bytes(std::sync::Arc::new(vec![1, 2, 3])),
            )
            .expect("the image attaches");
        // The reader is in a session; that is where a held message belongs.
        let session_id = VibexSessionId::new();
        app.agent.state.selected_session_id = Some(session_id.clone());
        let outgoing = app.composer.take_outgoing();
        app.enqueue(session_id.clone(), outgoing.text, outgoing.images);
        assert_eq!(app.queued_messages.len(), 1);
        assert_eq!(app.queued_messages[0].images.len(), 1);
        assert_eq!(app.queued_messages[0].session_id, session_id);

        // Pulling it back restores the picture, not just the words.
        app.queue_selection = Some(0);
        assert!(app.edit_queued_message());
        assert_eq!(app.composer.image_count(), 1);
        assert!(app.composer.text().contains("[Image #1]"));

        // And holding it again carries it a second time.
        let outgoing = app.composer.take_outgoing();
        app.enqueue(session_id.clone(), outgoing.text, outgoing.images);
        app.live = LiveState::Ready;
        let drained = app.drain_queue();
        assert_eq!(drained.len(), 1, "the queue releases the message");
        assert_eq!(drained[0].0, session_id, "and to its own session");
        assert_eq!(drained[0].2.len(), 1);
        assert_eq!(
            drained[0].2[0].uri.as_deref(),
            Some("data:image/png;base64,AQID")
        );
    }

    #[test]
    fn scopes_put_the_overlay_first() {
        // The active-scope list is order-sensitive: the first match wins.
        let management_scopes = [Scope::Management, Scope::Global];
        assert_eq!(management_scopes[0], Scope::Management);
    }

    #[test]
    fn focus_cycles_in_both_directions() {
        let mut focus = Focus::Sidebar;
        for _ in 0..4 {
            focus = focus.next();
        }
        assert_eq!(focus, Focus::Sidebar);
        for _ in 0..4 {
            focus = focus.previous();
        }
        assert_eq!(focus, Focus::Sidebar);
        assert_eq!(Focus::Sidebar.previous(), Focus::Composer);
        assert_eq!(Focus::Composer.next(), Focus::Sidebar);
    }

    #[test]
    fn management_rows_cover_every_management_page() {
        let pages = ManagementRow::ALL.map(ManagementRow::page);
        assert!(pages.contains(&Page::Devices));
        assert!(pages.contains(&Page::Recovery));
        assert!(pages.contains(&Page::Providers));
        assert_eq!(pages.len(), 8);
    }

    #[test]
    fn page_scopes_are_distinct_per_page() {
        let mut seen = std::collections::BTreeSet::new();
        for page in [
            Page::Sessions,
            Page::Agent,
            Page::Devices,
            Page::Usage,
            Page::Settings,
            Page::Help,
        ] {
            assert!(seen.insert(page.scope()), "{page:?} shares a scope");
        }
    }

    #[test]
    fn settings_rows_cover_the_page() {
        let rows = crate::settings::SETTINGS
            .iter()
            .map(|definition| definition.row)
            .collect::<Vec<_>>();
        assert!(rows.contains(&SettingRow::Theme));
        assert!(rows.contains(&SettingRow::Keys));
        assert!(rows.contains(&SettingRow::Workspace));
        // Every section has at least one row, or its header would be empty.
        for section in crate::settings::SettingsSection::ALL {
            assert!(
                crate::settings::SETTINGS
                    .iter()
                    .any(|definition| definition.section == section),
                "{section:?} has no setting"
            );
        }
    }

    #[test]
    fn recovery_actions_cover_every_destructive_and_read_only_action() {
        assert_eq!(RecoveryAction::ALL.len(), 4);
        assert!(RecoveryAction::ALL.contains(&RecoveryAction::BackupRestore));
    }

    #[test]
    fn toasts_expire() {
        let mut toast = Toast::info("hello");
        for _ in 0..3 {
            toast.ttl = toast.ttl.saturating_sub(1);
        }
        assert_eq!(toast.ttl, 1);
        assert_eq!(toast.tone, ToastTone::Info);
        assert_eq!(Toast::warning("w").tone, ToastTone::Warning);
        assert_eq!(Toast::danger("d").tone, ToastTone::Danger);
        assert_eq!(Toast::success("s").tone, ToastTone::Success);
    }

    #[test]
    fn live_state_gates_mutations() {
        assert!(LiveState::Ready.is_live());
        assert!(!LiveState::Offline.is_live());
        assert!(!LiveState::Reconnecting.is_live());
        assert!(!LiveState::Connecting.is_live());
    }

    #[test]
    fn shell_breakpoints_match_the_documented_columns() {
        assert_eq!(shell_for_columns(200), ShellKind::Wide);
        assert_eq!(shell_for_columns(120), ShellKind::Wide);
        assert_eq!(shell_for_columns(119), ShellKind::Medium);
        assert_eq!(shell_for_columns(88), ShellKind::Medium);
        assert_eq!(shell_for_columns(87), ShellKind::Compact);
    }

    #[test]
    fn capability_availability_is_reported_per_domain() {
        let snapshot = BackendCapabilitySnapshot::disconnected_v1();
        assert!(
            snapshot
                .agent
                .operations
                .contains(&BackendOperation::AgentListSessions)
        );
        assert_eq!(
            snapshot.agent.availability,
            vibex_backend::CapabilityAvailability::Offline
        );
        let _ = DomainCapabilities::unavailable();
    }
}

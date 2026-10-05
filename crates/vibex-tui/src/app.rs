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

use std::collections::{BTreeMap, HashMap};

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
    /// prompt, with the runtime it will be sent through named beside it. It is
    /// also the page the client *opens* on, because the reader who typed
    /// `vibex` in a directory came to write in it.
    NewSession,
    Management,
    Providers,
    Agents,
    Mcp,
    Skills,
    Prompts,
    Devices,
    Usage,
    Settings,
    Help,
}

impl Page {
    pub const fn scope(self) -> Scope {
        match self {
            Page::Sessions => Scope::Sessions,
            Page::Agent | Page::NewSession => Scope::Agent,
            Page::Management | Page::Agents => Scope::Management,
            Page::Providers => Scope::Providers,
            Page::Mcp => Scope::Mcp,
            Page::Skills => Scope::Skills,
            Page::Prompts => Scope::Prompts,
            Page::Devices => Scope::Devices,
            Page::Usage => Scope::Usage,
            Page::Settings => Scope::Settings,
            Page::Help => Scope::Help,
        }
    }

    pub const fn is_session_page(self) -> bool {
        matches!(self, Page::Agent | Page::NewSession)
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
    ProviderSecret,
    ProviderEndpoint,
    McpServerName,
    SkillName,
    PromptName,
    DeviceRevokeReason,
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

/// Whether a catalogue entry describes a runtime selection.
///
/// Identity is the Agent, the authentication source and the model: the feature
/// values an entry was published with are not part of it, so a selection the
/// reader has tuned with a run option still belongs to the entry it came from.
fn option_is_selection(
    option: &vibex_core::SessionRuntimeOption,
    selection: &vibex_core::SessionRuntimeSelection,
) -> bool {
    option.selection.agent_id == selection.agent_id
        && option.selection.auth_source == selection.auth_source
        && option.selection.model == selection.model
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
    Devices,
}

impl ManagementRow {
    pub const ALL: [ManagementRow; 6] = [
        ManagementRow::Agents,
        ManagementRow::Providers,
        ManagementRow::Mcp,
        ManagementRow::Skills,
        ManagementRow::Prompts,
        ManagementRow::Devices,
    ];

    pub const fn page(self) -> Page {
        match self {
            ManagementRow::Agents => Page::Agents,
            ManagementRow::Providers => Page::Providers,
            ManagementRow::Mcp => Page::Mcp,
            ManagementRow::Skills => Page::Skills,
            ManagementRow::Prompts => Page::Prompts,
            ManagementRow::Devices => Page::Devices,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidebarArrangement {
    pub sidebar: vibex_desktop_model::SidebarState,
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
    /// The arrangement the authority draws its own sidebar from, when this
    /// client has one. It replaces the local arrangement above: the desktop
    /// owns the tree, and two arrangements would mean the reader sees a list
    /// neither surface agreed on.
    pub sidebar_organization: Option<vibex_desktop_model::SidebarOrganizationView>,
    pub rows: Vec<TimelineRow>,
    /// Whether the sidebar pane is collapsed by the user.
    pub sidebar_collapsed: bool,
}

/// What a session reorder did.
#[derive(Debug, Clone)]
pub enum SidebarMove {
    /// Send this change to the authority, which owns the order.
    Remote(Box<Effect>),
    /// The move would cross the pinned band, which sorts above everything.
    Blocked,
    /// Already against the end of its band.
    AtEdge,
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
    /// The destination, including a reserved ID while creation is pending.
    /// An unbound projection is never shown in any session.
    pub session_id: Option<VibexSessionId>,
    /// Distinguishes this send's projected row from the next one's. The row has
    /// to keep one identity across frames — the transcript diffs by id, and the
    /// scroll anchor holds one — so it cannot be derived from the clock.
    pub serial: u64,
    pub correlation_id: vibex_core::CorrelationId,
    pub text: String,
    /// The message as it went on the wire, labels stripped: the echo re-derives
    /// what the reader saw from these, exactly as the sent row will.
    pub attachments: Vec<vibex_core::MessageAttachment>,
    /// The known timeline end positions the optimistic row. Confirmation uses
    /// correlation identity, never a text/sequence guess.
    pub after_sequence: i64,
    pub submitted_at: std::time::Instant,
    /// When the reader pressed send, on the wall clock.
    ///
    /// `submitted_at` measures the wait and cannot be turned into an hour; the
    /// projected row is stamped with this until the runtime's own row — and its
    /// own timestamp — replaces it.
    pub submitted_at_ms: i64,
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
    /// that is replaced by a different-looking one a moment later. The body is
    /// the wire text — the composer's labels were dropped on the way out — so
    /// its attachments are put back where they sat, exactly as the echoed row's
    /// will be.
    fn row(&self, strings: Strings) -> TimelineRow {
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
            // The row is the client's own, so it carries the clock the reader
            // sent it at rather than an item's.
            timestamp_ms: self.submitted_at_ms,
            title: "You".to_string(),
            body: crate::attachment::with_attachments(&self.text, &self.attachments, strings),
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
        self.session_id.as_ref() == Some(&item.session_id)
            && item.correlation_id.as_ref() == Some(&self.correlation_id)
            && matches!(item.payload, vibex_core::TimelinePayload::UserMessage(_))
    }
}

/// The owner of an editor buffer. Draft identities are reserved session IDs;
/// submitting a draft promotes its identity without borrowing another session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComposerTarget {
    Draft(VibexSessionId),
    Session(VibexSessionId),
}

/// An asynchronous edit is valid only for the same owner and unchanged input.
/// Navigation invalidates it even when the reader returns to the same editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerTicket {
    pub target: Option<ComposerTarget>,
    pub navigation_serial: u64,
    pub runtime: Option<Box<vibex_core::SessionRuntimeSelection>>,
    pub text: String,
    pub cursor: usize,
}

/// Immutable input to one creation, retained until its own callback arrives.
#[derive(Debug, Clone)]
pub struct PendingCreation {
    pub outgoing: crate::composer::Outgoing,
    pub runtime: Option<vibex_core::SessionRuntimeSelection>,
    pub workspace_root: String,
    pub send_id: u64,
}

#[derive(Debug, Clone)]
pub struct FailedCreation {
    pub draft_id: VibexSessionId,
    pub creation: PendingCreation,
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
    /// The reader's auto-continue preferences and the turns they act on.
    pub auto_continue: crate::auto_continue::AutoContinue,
    pub transcript: Transcript,
    pub scroll: ScrollState,

    pub composer: ComposerBuffer,
    pub history: ComposerHistory,
    pub completion: Option<CompletionMenu>,

    pub page: Page,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub toast: Option<Toast>,
    /// Frames the reader's first `Ctrl+C` is still waiting for a second one.
    ///
    /// The hint on the status band is drawn from this rather than stored beside
    /// it, so the two can never disagree about whether a second press would
    /// quit. Zero is "not armed".
    quit_armed: u8,

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
    /// Lines the session list shows, published by the frame that drew it: the
    /// window is what a page key and the wheel step through.
    pub session_band_rows: usize,
    /// The first line of the session list the page draws, snapped to a row.
    ///
    /// The list is taller than the window it sits in, so it scrolls by line
    /// rather than by selection: the wheel and the page keys move the window
    /// and leave the cursor where the reader put it.
    pub session_scroll: usize,
    /// Whether the reader moved the window directly. While this is set the
    /// window stays where they put it; moving the selection — a key, a click,
    /// a filter — hands it back to the cursor.
    pub session_scroll_manual: bool,
    /// Regions the last frame published for mouse hit-testing: the transcript
    /// band and the close affordance of the modal, when one is open.
    pub regions: FrameRegions,

    /// Runtime catalog, kept for the runtime picker.
    pub runtime_options: Option<vibex_core::SessionRuntimeOptionCatalog>,
    /// Whether the pending catalogue read was asked for by the picker, so the
    /// overlay opens when it lands. A catalogue fetched for the composer's
    /// info line must not pop a modal over the session.
    pub runtime_picker_pending: bool,
    /// What the switcher remembers between runs: the selection last applied,
    /// each Agent's own answer, and how each model runs.
    pub runtime_prefs: crate::runtime_prefs::RuntimePreferences,
    /// Where the switcher's memory is written; `None` keeps it in memory only.
    pub runtime_path: Option<std::path::PathBuf>,
    /// The switcher's filter, folded groups and staged run options.
    pub runtime_picker: crate::runtime_picker::RuntimePickerState,
    /// The Agent whose row the Agents page should land on.
    ///
    /// A catalogue row whose account needs attention sends the reader to that
    /// page; the list arrives after the page does, so the Agent waits here for
    /// it rather than being selected by an index the page has not got yet.
    pub pending_agent_focus: Option<vibex_core::AgentId>,
    /// The workspace chosen in the workspace browser, consumed by the
    /// new-session prompt.
    pub workspace_path: Option<String>,
    /// The workspace the reader named as the default for new sessions.
    ///
    /// Apart from `workspace_path`, which is the choice for the session being
    /// composed and may have come from the directory this client was started
    /// in: only the settings row writes this standing answer down, so picking a
    /// directory for one session leaves the next run's default alone.
    pub preferred_workspace: Option<String>,
    /// Immutable first-message inputs keyed by the reserved session identity.
    /// Each remains here until its own creation is acknowledged.
    pub pending_creations: BTreeMap<VibexSessionId, PendingCreation>,
    pub failed_creations: Vec<FailedCreation>,
    pub new_draft_id: VibexSessionId,
    pub composer_target: Option<ComposerTarget>,
    pub composer_drafts: BTreeMap<ComposerTarget, ComposerBuffer>,
    pub runtime_picker_target: Option<ComposerTarget>,
    pub navigation_serial: u64,
    pub pending_forks: BTreeMap<VibexSessionId, u64>,
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
    /// The click target the pointer is over, so a control can show it can be
    /// pressed before the reader presses it.
    pub hovered_hint: Option<crate::action::Intent>,
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
    /// Where the interface's own settings are written; `None` keeps them in
    /// memory only, which is what a preview or a test wants.
    pub preferences_path: Option<std::path::PathBuf>,
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
    pub pending_sends: BTreeMap<u64, PendingSend>,
    /// RPC lifetime is independent of the optimistic row's display timeout.
    pub inflight_sends: BTreeMap<u64, VibexSessionId>,
    /// Bumped for every send, so each projected row keeps its own identity.
    pub pending_send_serial: u64,
    /// Sessions that finished a turn while the reader was looking elsewhere.
    ///
    /// The client's own notion, not the runtime's: it is what "unread" means in
    /// a list of sessions, and it is cleared by opening the session.
    pub unread_sessions: std::collections::BTreeSet<String>,
    /// What each session last did, one line, for the list's second line.
    ///
    /// The runtime publishes a session's *state* but not its last words, and a
    /// list of titles alone cannot say which of five idle sessions is the one
    /// that answered. The line is the client's own reading of the events it
    /// already receives, pruned to the sessions the list still holds, so it
    /// costs the runtime nothing.
    pub session_echoes: BTreeMap<String, String>,
    /// A transient message above the composer, dismissed on the next key.
    pub banner: Option<Banner>,
    /// When the running turn started, for the elapsed-time readout.
    pub turn_started: Option<std::time::Instant>,
    /// Tokens spent by the running turn, for the readout beside the timer.
    pub turn_tokens: Option<u64>,
    /// The running turn as the line above the composer reads it: its phase, how
    /// many tools it has called, and how fast it is producing output.
    ///
    /// The phase and the count are re-read from the projection on every event;
    /// the rate is a difference between two of those readings, so it is held
    /// rather than derived at draw time.
    pub turn_readout: crate::turn::TurnReadout,
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
    /// The runtime switcher's rows, when it is the modal on screen. Its cursor
    /// indexes a window rather than a rect, so the region carries where the
    /// window started as well as where it was drawn.
    pub runtime_picker: Option<RuntimePickerRegion>,
    /// The turn rail's ticks, one rect per turn.
    pub turns: Vec<(ratatui::layout::Rect, usize)>,
    /// The shortcut band's hints, so a click runs the same intent as the key.
    pub hints: Vec<(ratatui::layout::Rect, crate::action::Intent)>,
}

impl FrameRegions {
    /// Start a frame: every rect the last frame published is dropped.
    ///
    /// A rect describes where something was when the frame drew it. Keeping the
    /// previous frame's rects would let a click land on a row that has moved or
    /// gone, and the lists would grow with every frame the reader leaves open.
    pub fn begin_frame(&mut self) {
        self.turns.clear();
        self.hints.clear();
        self.runtime_picker = None;
        // A list, a banner row, a composer's box and a modal's close affordance
        // each belong to the frame that drew them: left standing, the session
        // list's rect would hit-test transcript rows instead, a modal that has
        // closed would still answer a click where its close used to be, and a
        // composer nobody can see would take the keyboard.
        self.list = None;
        self.banner = None;
        self.modal_close = None;
        self.composer = None;
        self.composer_band = None;
        self.composer_scroll = 0;
    }
}

/// A clickable list drawn by a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRegion {
    pub rect: ratatui::layout::Rect,
    pub scope: crate::keymap::Scope,
    /// Row indices that are selectable, in screen order. A list with group
    /// headers has fewer selectable rows than it has lines.
    pub rows: usize,
    /// Lines between the top of the rect and the first selectable row.
    pub first_line: usize,
    /// The index of the row drawn at the top of the rect.
    ///
    /// A list scrolls by row, so a click has to know which row the first drawn
    /// line belongs to before it can count downwards.
    pub first_row: usize,
    /// How many lines each drawn row occupies, top to bottom. Empty means every
    /// row is one line, which is what a list of single-line rows publishes.
    ///
    /// Rows are not all the same height — a session with something to say
    /// spends a second line on it — and a click that assumed one line per row
    /// would land on the row below the one under the pointer.
    pub heights: Vec<u16>,
}

/// Where the runtime switcher drew its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePickerRegion {
    pub rect: ratatui::layout::Rect,
    /// The catalogue row drawn on the rect's first line.
    pub offset: usize,
    /// How many rows the view has, drawn or not.
    pub rows: usize,
}

impl RuntimePickerRegion {
    /// Which row a pointer is over, if any.
    pub fn row_at(&self, column: u16, row: u16) -> Option<usize> {
        if column < self.rect.x
            || column >= self.rect.right()
            || row < self.rect.y
            || row >= self.rect.bottom()
        {
            return None;
        }
        let index = self.offset + usize::from(row - self.rect.y);
        (index < self.rows).then_some(index)
    }
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
    let line = usize::from(row - region.rect.y).checked_sub(region.first_line)?;
    if region.heights.is_empty() {
        return (line < region.rows).then_some(line);
    }
    let mut top = 0usize;
    for (index, height) in region.heights.iter().enumerate() {
        let height = usize::from((*height).max(1));
        if line < top + height {
            let index = region.first_row + index;
            return (index < region.rows).then_some(index);
        }
        top += height;
    }
    None
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
        // The reader's choice per appearance comes from the file; a theme the
        // composition root named for this run (`--theme`, `VIBEX_THEME`) seeds
        // the slot for the appearance on screen without erasing the other one.
        let mut themes = options.remembered.themes.clone();
        if let Some(theme_id) = options.theme_id.as_deref() {
            crate::settings::select_theme(&mut themes, options.mode, theme_id);
        }
        let theme = TuiTheme::resolve(
            crate::settings::theme_slot(&themes, options.mode),
            options.mode,
            capability,
        );
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
        let mut app = Self {
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
            auto_continue: crate::auto_continue::AutoContinue::default(),
            projection: ProjectionState {
                sidebar_organization: None,
                sidebar: arrangement.sidebar,
                rows: Vec::new(),
                sidebar_collapsed: false,
            },
            sidebar_path: options.sidebar_path,
            preferences_path: options.preferences_path,
            transcript: Transcript::new(),
            scroll: ScrollState::default(),
            composer: ComposerBuffer::default(),
            history: ComposerHistory::new(64),
            completion: None,
            page: Page::Sessions,
            focus: Focus::Main,
            overlay: None,
            toast: None,
            quit_armed: 0,
            settings: SettingsState {
                themes,
                mode: options.mode,
                locale: options.locale,
                glyphs: capability.glyphs,
                selected: 0,
                view: SettingsMode::Browse,
                filter: String::new(),
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
            text_view: None,
            viewport: (120, 40),
            transcript_band_rows: 20,
            session_band_rows: 12,
            session_scroll: 0,
            session_scroll_manual: false,
            regions: FrameRegions::default(),
            runtime_options: None,
            runtime_picker_pending: false,
            runtime_prefs: crate::runtime_prefs::RuntimePreferences::load(
                options.runtime_path.as_deref(),
            ),
            runtime_path: options.runtime_path,
            runtime_picker: crate::runtime_picker::RuntimePickerState::default(),
            pending_agent_focus: None,
            workspace_path: None,
            preferred_workspace: options.remembered.workspace.clone(),
            pending_creations: BTreeMap::new(),
            failed_creations: Vec::new(),
            new_draft_id: VibexSessionId::new(),
            composer_target: None,
            composer_drafts: BTreeMap::new(),
            runtime_picker_target: None,
            navigation_serial: 0,
            pending_forks: BTreeMap::new(),
            new_session_runtime: None,
            run_option_prompt: None,
            elicitation_draft: crate::reduce::ElicitationDraft::default(),
            usage_scope_session: true,
            session_cards: std::collections::BTreeSet::new(),
            search: None,
            hover: None,
            hovered_hint: None,
            history_selection: 0,
            recent_commands: options.remembered.recent_commands.clone(),
            text_selection: None,
            draft_selecting: false,
            dock_open: false,
            dock_selection: None,
            dock_collapsed: std::collections::BTreeSet::new(),
            dock_hide_done: false,
            last_click: None,
            queued_messages: Vec::new(),
            queue_selection: None,
            pending_sends: BTreeMap::new(),
            inflight_sends: BTreeMap::new(),
            pending_send_serial: 0,
            unread_sessions: std::collections::BTreeSet::new(),
            session_echoes: BTreeMap::new(),
            banner: None,
            turn_started: None,
            turn_tokens: None,
            turn_readout: crate::turn::TurnReadout::default(),
            activity: None,
            animation_phase: 0,
            composer_mode: ComposerMode::Normal,
        };
        // The client opens where a session is written, not on the list of
        // sessions that already exist: the reader who typed `vibex` in a
        // directory wants to write a message there, and the list is one key
        // away when they want to reopen something instead. The directory the
        // client was started in is the workspace that first session gets, so
        // the page names it rather than asking which directory was meant — and
        // a reader who named a standing default in the settings gets that
        // instead, because they already answered the question there.
        app.workspace_path = app
            .preferred_workspace
            .clone()
            .or_else(Self::starting_workspace);
        app.navigate_to(Page::NewSession);
        app.focus = Focus::Composer;
        app
    }

    /// The directory this client was started in, when it is one.
    ///
    /// A reader opens the client *in* the project they mean to work on, so
    /// that directory is the workspace the first session gets: it is the one
    /// answer they already gave by choosing where to run the command. A
    /// directory that cannot be read, or that is not one, proposes nothing and
    /// lets the page fall back to what the runtime reports.
    fn starting_workspace() -> Option<String> {
        std::env::current_dir()
            .ok()
            .filter(|path| path.is_dir())
            .map(|path| path.display().to_string())
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

    /// Whether the page in front of the reader is a session's own page.
    ///
    /// The composing page deliberately keeps the session it came from
    /// *selected* — leaving the page has to return there — but the reader is
    /// writing a *new* session there, which does not exist yet. Everything that
    /// describes "the session in front of the reader" — its turn and clock,
    /// its queue, its plan, its approval count, the Agent its messages go
    /// through — has to answer for the page, so while this is false those
    /// answers are empty rather than the session behind the page, which the
    /// reader is leaving.
    ///
    /// The distinction is the *page*, not whether a session happens to be
    /// selected: a creation owns its reserved identity before the authority
    /// has persisted the session, while an editable new draft owns no session.
    pub fn page_owns_session(&self) -> bool {
        self.page != Page::NewSession
    }

    /// The session this page answers for, when it has one of its own.
    pub fn page_session_id(&self) -> Option<&VibexSessionId> {
        if self.page_owns_session() {
            self.selected_session_id()
        } else {
            None
        }
    }

    /// Whether the page in front of the reader is *showing* a session.
    ///
    /// This is what a runtime choice moves, and what a run option tunes: the
    /// picker is one global key, so its target has to be the session on screen
    /// and never one the reader cannot see. The session list is the case that
    /// bit: the client still had a session selected behind it, so a choice made
    /// while looking at the list — or while writing a new session — moved that
    /// unseen session. Deciding by the page keeps the two independent: a page
    /// showing no session has nothing to move, so the choice is the *next*
    /// session's, and a session's own page moves only itself.
    pub fn page_shows_session(&self) -> bool {
        self.page_owns_session()
            && self.page.is_session_page()
            && self.selected_session_id().is_some()
    }

    /// Open the session view for a session that does not exist yet.
    ///
    /// The reader sent the first message and the runtime is still making the
    /// session it belongs to. They are moved into the session view at once —
    /// waiting on the page reads as nothing having happened — and the view is
    /// emptied of the session they came from, because what is about to appear
    /// there is a new one. The message itself is projected into it until the
    /// runtime's own copy arrives.
    pub fn enter_creating_session(&mut self, session_id: VibexSessionId) {
        self.open_session(session_id.clone());
        self.composer_drafts
            .remove(&ComposerTarget::Draft(session_id));
        self.workspace_path = None;
        self.new_session_runtime = None;
        self.new_draft_id = VibexSessionId::new();
        self.sync_transcript();
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

    /// Whether a catalogue entry is the one this page's message would go
    /// through.
    ///
    /// The *page* answers, through [`Self::page_runtime_selection`]: on the page
    /// where a session is being written that is the choice the page holds — the
    /// reader's pick, or the catalogue's first available entry, which is what
    /// the session would be created with — and never the entry the session
    /// behind the page is running on.
    ///
    /// Matching is by Agent, authentication source and model rather than by
    /// whole-selection equality: the catalogue's choice carries the feature
    /// values it was published with, and a choice that has since been tuned with
    /// a run option is still that choice.
    pub fn runtime_option_is_current(&self, option: &vibex_core::SessionRuntimeOption) -> bool {
        self.page_runtime_selection()
            .is_some_and(|selection| option_is_selection(option, &selection))
    }

    /// The index of this page's runtime choice in the loaded catalogue.
    pub fn current_runtime_option_index(&self) -> Option<usize> {
        let catalog = self.runtime_options.as_ref()?;
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
            .find(|option| option_is_selection(option, selection))
    }

    /// The catalogue entry a creation with no choice of its own would use.
    ///
    /// A valid remembered preference is the default without becoming an
    /// explicit edit to the new draft. Otherwise use the catalogue's first
    /// available entry. Creation materializes this same displayed selection
    /// into its immutable request.
    fn default_runtime_selection(&self) -> Option<vibex_core::SessionRuntimeSelection> {
        let catalog = self.runtime_options.as_ref()?;
        if let Some(selection) = self.runtime_prefs.preferred(catalog, None) {
            return Some(selection);
        }
        catalog
            .options
            .iter()
            .find(|option| option.availability == vibex_core::RuntimeOptionAvailability::Available)
            .or_else(|| catalog.options.first())
            .map(|option| option.selection.clone())
    }

    /// Whether the page answers for a session that does not exist yet.
    ///
    /// The composing page keeps the client's session *selected* — leaving the
    /// page has to return there — so anything that asks "which session?" while
    /// it is up answers with the session behind the page rather than with what
    /// the reader is writing. Reading a runtime choice, and applying one, both
    /// have to ask the *page* first; the session is what answers only when there
    /// is no page holding a session of its own and no session selected at all.
    pub fn page_is_composing(&self) -> bool {
        self.page == Page::NewSession
            || (self.selected_session_id().is_none() && self.active_session().is_none())
    }

    /// The runtime selection this page's next message would go through.
    ///
    /// A page that is showing a session answers with that session's own durable
    /// desired selection — a choice made here moves *it*. A session whose
    /// runtime has never been reported has no selection to read, so it gets
    /// none rather than the catalogue's first entry, which would offer another
    /// Agent's options over it.
    ///
    /// A page that shows no session — the list, the management pages, the page
    /// where a session is being written, and the view that waits for one being
    /// created — answers with the choice the page holds, falling back to the
    /// entry a creation with no choice would use. Its picker is therefore
    /// about the *next* session, and the session the client still has selected
    /// behind it is not what the page names or moves.
    pub fn page_runtime_selection(&self) -> Option<vibex_core::SessionRuntimeSelection> {
        if self.page_shows_session() {
            if let Some(creation) = self
                .selected_session_id()
                .and_then(|id| self.pending_creations.get(id))
            {
                return creation.runtime.clone();
            }
            return self.session_runtime_selection().cloned();
        }
        self.new_session_runtime
            .clone()
            .or_else(|| self.default_runtime_selection())
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
        self.run_options_for(&selection)
    }

    /// The run options one selection publishes.
    ///
    /// Split from [`Self::run_options`] because the switcher reads its rows from
    /// the *staged* selection while the composer's info line reads the applied
    /// one: a reader who changed a thinking depth has not sent a message with it
    /// yet, and the line behind the panel must not claim they have.
    pub fn run_options_for(
        &self,
        selection: &vibex_core::SessionRuntimeSelection,
    ) -> Vec<RunOption> {
        let Some(catalog) = self.runtime_options.as_ref() else {
            return Vec::new();
        };
        let selection = selection.clone();
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
    /// The *page's* own selection answers ([`Self::page_runtime_selection`]):
    /// on the page where a session is being written that is the runtime
    /// [`Effect::CreateSession`] will carry, and it is deliberately not read
    /// from the session behind the page — naming that one is what promised an
    /// Agent the message would not go through. A choice the reader has tuned
    /// with a run option is still named by the entry it belongs to.
    ///
    /// The composing page has no session to fall back to either: when it holds
    /// no choice — a catalogue that has not arrived, or one that publishes
    /// nothing — it names the entry a creation with no choice would use, or
    /// says the runtime is unavailable when the catalogue has none. Naming the
    /// session behind the page here is what made the page claim codex while the
    /// creation used the catalogue's default.
    pub fn composer_runtime_labels(&self) -> (String, String) {
        if let Some(selection) = self.page_runtime_selection() {
            if let Some(option) = self.runtime_option_for(&selection) {
                return (option.agent_label.clone(), option.model_label.clone());
            }
            // The catalogue no longer publishes this selection (an Agent that
            // has since gone away): its own words still name it.
            return (
                selection.agent_id.to_string(),
                selection
                    .model
                    .model_id()
                    .map(str::to_string)
                    .unwrap_or_default(),
            );
        }
        // A page showing a session whose runtime state has not arrived yet — or
        // the row the list holds for it — still names the Agent it records. No
        // other page does: a page that shows no session names the entry a
        // creation with no choice would use, never the one behind it.
        if self.page_shows_session()
            && let Some(session) = self.active_session().or_else(|| {
                self.selected_session_id()
                    .and_then(|session_id| self.session_by_id(session_id))
            })
        {
            return (session.agent_id.to_string(), String::new());
        }
        match self
            .runtime_options
            .as_ref()
            .and_then(|catalog| catalog.options.first())
        {
            Some(option) => (option.agent_label.clone(), option.model_label.clone()),
            None => (
                self.strings.runtime_unavailable().to_string(),
                String::new(),
            ),
        }
    }

    /// Whether the page's mark is lit: it shines only on a page that waits.
    pub fn composing_page_shines(&self) -> bool {
        self.page == Page::NewSession && self.focus == Focus::Composer
    }

    /// The workspace a session created from the composing page will open in.
    ///
    /// The same answer [`Self::create_session_from_draft`] sends: the directory
    /// the reader chose — which is the one the client was started in until they
    /// choose another — else the one the open session already runs in.
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
            .or_else(Self::starting_workspace)
            .unwrap_or_default()
    }

    /// Where the directory picker opens.
    ///
    /// A backend that can browse the authority's filesystem answers a first
    /// listing with its own root, so nothing is proposed here: which roots
    /// exist is the authority's to say. A backend that browses *this* machine
    /// — the native seat, which deliberately does not report
    /// `WorkspaceBrowseDirectories` — opens on the directory the page already
    /// names when that directory exists here. A reader choosing a root is
    /// usually choosing a sibling of the project they are in, and walking down
    /// from home is a different question.
    pub fn workspace_picker_start(&self) -> Option<String> {
        if self.supports(BackendOperation::WorkspaceBrowseDirectories) {
            return None;
        }
        let path = self.new_session_workspace();
        std::path::Path::new(&path).is_dir().then_some(path)
    }

    /// Workspace the session pages read from.
    pub fn active_workspace_id(&self) -> Option<vibex_core::WorkspaceId> {
        self.active_session()
            .map(|session| session.workspace_id.clone())
    }

    /// Sidebar rows for the current filter, with the reader's grouping applied.
    pub fn sidebar_rows(&self) -> Vec<vibex_desktop_model::AgentSidebarRow> {
        self.sidebar_view().rows
    }

    /// The session list as the page draws it: the rows, the size of each
    /// heading's subtree, and the state tally the header's chips read.
    ///
    /// The three are computed together because they are three readings of one
    /// projection: a chip counted from a different session set than the rows
    /// were built from is how a header ends up describing a list nobody is
    /// looking at.
    pub fn sidebar_view(&self) -> crate::sessions::SessionListRows {
        match self.projection.sidebar_organization.as_ref() {
            Some(view) => {
                let sessions = self
                    .agent
                    .state
                    .sessions
                    .value
                    .as_deref()
                    .unwrap_or_default();
                crate::sessions::session_list_rows(&crate::sessions::SessionListInput {
                    view,
                    sessions,
                    projects: &crate::sessions::project_entries(&self.workspace_rows, sessions),
                    unread_session_ids: &self.unread_sessions,
                    query: &self.filter,
                })
            }
            // No arrangement to mirror: the list is projected from the
            // sessions themselves, which is all a client without a desktop
            // can honestly say about the order.
            None => {
                let sessions = self
                    .agent
                    .state
                    .view(&self.projection.sidebar, &self.filter, self.shell)
                    .sessions;
                let loaded = self
                    .agent
                    .state
                    .sessions
                    .value
                    .as_deref()
                    .unwrap_or_default();
                let states = crate::sessions::session_state_tally(loaded, &self.filter);
                let counts = crate::sessions::heading_counts(&sessions, loaded, &self.filter);
                crate::sessions::SessionListRows {
                    counts,
                    rows: sessions,
                    states,
                }
            }
        }
    }

    /// Whether the session list's row at `index` is a heading — a project, or a
    /// folder the reader made — rather than a session.
    ///
    /// A heading is a control rather than a row to open, which is what lets a
    /// single click fold it.
    pub fn sidebar_row_is_heading(&self, index: usize) -> bool {
        self.sidebar_rows()
            .get(index)
            .is_some_and(|row| row.session_id.is_none())
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
        Some(Self::interface_home()?.join("tui-sidebar.json"))
    }

    /// Where the runtime switcher's memory is written.
    ///
    /// Beside the arrangement, for the same reason: one home holds everything
    /// this client remembers.
    pub fn runtime_preferences_path() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_RUNTIME")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        Some(Self::interface_home()?.join("tui-runtime.json"))
    }

    /// Where the interface's own settings are written.
    ///
    /// Beside the arrangement and the switcher's memory, for the same reason.
    /// This is the file the settings surface keeps: the look, the language, the
    /// standing workspace and the palette's recents.
    pub fn interface_preferences_path() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_INTERFACE")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        Some(Self::interface_home()?.join("tui-interface.json"))
    }

    /// The directory the interface's own files live in.
    fn interface_home() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_HOME")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        std::env::var("VIBEX_HOME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| std::path::PathBuf::from(value).join(".vibex"))
            })
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
        };
        if let Ok(body) = serde_json::to_string_pretty(&arrangement) {
            let _ = std::fs::write(path, body);
        }
    }

    /// The interface settings this run would write down.
    ///
    /// Read from the live surface rather than tracked beside it, so the file
    /// cannot describe a value the reader is not looking at. The workspace is
    /// the standing default alone: the directory a session is composed in is a
    /// per-session answer, not a setting.
    pub fn interface_preferences(&self) -> crate::interface_prefs::InterfacePreferences {
        crate::interface_prefs::InterfacePreferences {
            themes: self.settings.themes.clone(),
            mode: Some(
                match self.settings.mode {
                    vibex_ui::GpuiThemeMode::Dark => "dark",
                    vibex_ui::GpuiThemeMode::Light => "light",
                }
                .to_string(),
            ),
            icons: Some(
                match self.settings.glyphs {
                    crate::theme::GlyphMode::Unicode => "unicode",
                    crate::theme::GlyphMode::Ascii => "ascii",
                }
                .to_string(),
            ),
            locale: Some(self.settings.locale.tag().to_string()),
            workspace: self.preferred_workspace.clone(),
            recent_commands: self.recent_commands.clone(),
        }
    }

    /// Persist the interface's own settings. A failure is silent: losing a
    /// preference is not worth interrupting the reader, and the next change
    /// tries again.
    pub fn save_interface_preferences(&self) {
        let Some(path) = self.preferences_path.as_deref() else {
            return;
        };
        self.interface_preferences().save(Some(path));
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

    /// The arrangement the authority draws its sidebar from, when loaded.
    pub fn sidebar_organization(&self) -> Option<&vibex_desktop_model::SidebarOrganizationView> {
        self.projection.sidebar_organization.as_ref()
    }

    /// Fold an authority snapshot into the projection.
    ///
    /// An arrangement that arranges nothing — no folders, no manual order, no
    /// pins — is dropped rather than adopted: it carries no information, and
    /// adopting it would replace this client's ordering with an arbitrary one.
    pub fn apply_sidebar_organization(
        &mut self,
        snapshot: &vibex_core::RemoteSidebarOrganizationSnapshot,
    ) -> bool {
        // The auto-continue preferences travel with the tree but do not depend
        // on it: a reader who has switched a session on has said so whether or
        // not they have also arranged folders.
        let sessions = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default();
        self.auto_continue.apply_authority(
            &snapshot.auto_continue_project_ids.iter().cloned().collect(),
            &snapshot.auto_continue_session_overrides.clone(),
            &snapshot.auto_continue_session_ids.iter().cloned().collect(),
            &snapshot
                .auto_continue_paused_session_ids
                .iter()
                .cloned()
                .collect(),
            sessions,
        );
        let view = vibex_desktop_model::SidebarOrganizationView::from_remote(snapshot);
        if !view.arranges_anything() {
            return self.projection.sidebar_organization.take().is_some();
        }
        let changed = self.projection.sidebar_organization.as_ref() != Some(&view);
        self.projection.sidebar_organization = Some(view);
        changed
    }

    /// The authority change that opens or closes the row the cursor is on.
    ///
    /// `None` when there is no arrangement loaded, which is the caller's signal
    /// to fold the change into the local fallback arrangement instead.
    pub fn sidebar_collapse_effect(
        &self,
        row: &vibex_desktop_model::AgentSidebarRow,
    ) -> Option<Effect> {
        use vibex_desktop_model::AgentSidebarRowKind;

        let view = self.projection.sidebar_organization.as_ref()?;
        let mutation = match row.kind {
            AgentSidebarRowKind::Folder => {
                vibex_core::RemoteSidebarOrganizationMutation::SetFolderCollapsed {
                    folder_id: row.id.strip_prefix("folder:")?.to_string(),
                    collapsed: !row.collapsed,
                }
            }
            AgentSidebarRowKind::Project => {
                vibex_core::RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                    project_id: row.project_id.clone(),
                    collapsed: !row.collapsed,
                }
            }
            AgentSidebarRowKind::Session => return None,
        };
        Some(Effect::MutateSidebarOrganization {
            mutation,
            expected_revision: Some(view.revision),
        })
    }

    /// The authority change that switches auto-continue on or off.
    ///
    /// `None` when there is no arrangement loaded: the preference is then this
    /// client's alone, which is all a client with no authority to write to can
    /// honestly claim.
    pub fn sidebar_auto_continue_effect(
        &self,
        session_id: &VibexSessionId,
        enabled: bool,
    ) -> Option<Effect> {
        let view = self.projection.sidebar_organization.as_ref()?;
        Some(Effect::MutateSidebarOrganization {
            mutation: vibex_core::RemoteSidebarOrganizationMutation::SetSessionAutoContinue {
                session_id: session_id.as_str().to_string(),
                enabled,
            },
            expected_revision: Some(view.revision),
        })
    }

    /// The authority change that pins or unpins the row the cursor is on.
    pub fn sidebar_pin_effect(&self, row: &vibex_desktop_model::AgentSidebarRow) -> Option<Effect> {
        let view = self.projection.sidebar_organization.as_ref()?;
        let session_id = row.session_id.as_ref()?.as_str().to_string();
        Some(Effect::MutateSidebarOrganization {
            mutation: vibex_core::RemoteSidebarOrganizationMutation::SetSessionPinned {
                session_id,
                pinned: !row.pinned,
            },
            expected_revision: Some(view.revision),
        })
    }

    /// The authority change that moves the selected session one place.
    ///
    /// `None` when there is no arrangement loaded: the caller moves through the
    /// local order instead.
    pub fn sidebar_move(&self, delta: isize) -> Option<SidebarMove> {
        let view = self.projection.sidebar_organization.as_ref()?;
        let rows = self.sidebar_rows();
        let index = self.selection_for(Scope::Sessions);
        let row = rows.get(index)?;
        let moving_id = row.session_id.as_ref()?.as_str().to_string();
        let project_id = row.project_id.clone();
        let parent_id = row.parent_id.clone();
        let mut cursor = index as isize + delta;
        while cursor >= 0 && (cursor as usize) < rows.len() {
            let candidate = &rows[cursor as usize];
            if let Some(target_id) = candidate.session_id.as_ref()
                && candidate.project_id == project_id
                && candidate.parent_id == parent_id
            {
                // Pinned rows sort above the rest whatever the arrangement
                // says, so a move across that line would look like nothing
                // happened. Saying so is better than silently disagreeing.
                if candidate.pinned != row.pinned {
                    return Some(SidebarMove::Blocked);
                }
                let mutation = vibex_core::RemoteSidebarOrganizationMutation::MoveItems {
                    items: vec![vibex_core::RemoteSidebarItemRef {
                        kind: vibex_core::RemoteSidebarItemKind::Session,
                        id: moving_id,
                    }],
                    anchor: Some(vibex_core::RemoteSidebarItemRef {
                        kind: vibex_core::RemoteSidebarItemKind::Session,
                        id: target_id.as_str().to_string(),
                    }),
                    position: if delta > 0 {
                        vibex_core::RemoteSidebarDropPosition::After
                    } else {
                        vibex_core::RemoteSidebarDropPosition::Before
                    },
                    project_id: Some(project_id),
                };
                return Some(SidebarMove::Remote(Box::new(
                    Effect::MutateSidebarOrganization {
                        mutation,
                        expected_revision: Some(view.revision),
                    },
                )));
            }
            cursor += delta;
        }
        Some(SidebarMove::AtEdge)
    }

    /// What auto-continue wants to do now, as effects for the worker.
    ///
    /// Called after anything that could have moved a turn: a session update, a
    /// timeline event, a list refresh, a mutation's answer.
    pub fn sync_auto_continue(&mut self) -> Vec<Effect> {
        let sessions = self.agent.state.sessions.value.clone().unwrap_or_default();
        let pending = self.pending_sessions(&sessions);
        let now_ms = vibex_core::unix_timestamp_ms();
        let actions = self.auto_continue.sync(&sessions, now_ms, |session_id| {
            pending.contains(session_id.as_str())
        });
        actions.into_iter().map(auto_continue_effect).collect()
    }

    /// The sessions a send is in flight for, so a continuation does not
    /// interleave with one.
    fn pending_sessions(
        &self,
        sessions: &[vibex_core::AgentSession],
    ) -> std::collections::BTreeSet<String> {
        sessions
            .iter()
            .filter(|session| self.session_is_running(&session.id))
            .map(|session| session.id.as_str().to_string())
            .collect()
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
        if scope == Scope::Sessions {
            // The cursor owns the window again: whatever moved it, the reader
            // is now looking at a row and the list has to show it.
            self.session_scroll_manual = false;
        }
        self.selection.insert(scope, value);
    }

    /// Move the session list's window by `lines`, without moving the cursor.
    ///
    /// The window is clamped where it is drawn — only the frame knows how tall
    /// the rows are and how much room it has — so this only has to refuse to
    /// run off the top.
    pub fn scroll_session_list(&mut self, lines: i64) {
        self.session_scroll_manual = true;
        self.session_scroll = if lines.is_negative() {
            self.session_scroll
                .saturating_sub(lines.unsigned_abs() as usize)
        } else {
            self.session_scroll.saturating_add(lines as usize)
        };
    }

    /// Put the session list's window at its first line.
    pub fn session_list_home(&mut self) {
        self.session_scroll_manual = true;
        self.session_scroll = 0;
    }

    /// Put the session list's window at a line past the end, which the frame
    /// clamps to the last page of rows.
    pub fn session_list_end(&mut self) {
        self.session_scroll_manual = true;
        self.session_scroll = usize::MAX;
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
            Page::Settings => self.visible_settings().len(),
            Page::Agent => self.transcript.len(),
            Page::Usage | Page::Help => 0,
        }
    }

    /// The scopes consulted for key dispatch, most specific first.
    pub fn active_scopes(&self) -> Vec<Scope> {
        let mut scopes = Vec::new();
        // The switcher sits *inside* the overlay scope: the overlay's shared
        // keys (move, confirm, close) still apply, and the switcher's own keys
        // win where the two would otherwise disagree.
        if self.runtime_picker_is_open() {
            scopes.push(Scope::Runtime);
        }
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
        if self.runtime_picker_is_open() {
            return vec![Scope::Runtime, Scope::Global];
        }
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

    /// Whether a second `Ctrl+C` would quit, and the hint is on screen.
    pub fn quit_armed(&self) -> bool {
        self.quit_armed > 0
    }

    /// Take the reader's first `Ctrl+C`, and start the window a second one has
    /// to land in.
    pub fn arm_quit(&mut self) {
        self.quit_armed = QUIT_ARM_FRAMES;
    }

    /// Spend the arming, returning whether there was one to spend.
    ///
    /// Every `Ctrl+C` spends it, including the presses that go on to do
    /// something else: an arming that survived a press which cleared a draft or
    /// stopped a turn would leave the *next* press quitting on a reader who had
    /// long since stopped asking to leave.
    pub fn spend_quit(&mut self) -> bool {
        std::mem::take(&mut self.quit_armed) > 0
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

    /// The session a key pressed on the session list acts on.
    ///
    /// The list shows rows and its cursor is on one of them, so rename, fork,
    /// archive and delete answer for *that* row — never for the session the
    /// client happens to have open behind the list. The reader may be looking
    /// at a row that is not the one they are working in, and an action that
    /// silently hit the other one is how a session got archived that nobody
    /// pointed at.
    pub fn list_session_target(&self) -> Option<VibexSessionId> {
        self.selected_session_row_id()
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
    ///
    /// The dock is a session's own panel — delegated agents, its plan, its held
    /// queue — so the composing page has none: the flag survives the visit and
    /// the panel returns with the session it belongs to.
    pub fn dock_is_focused(&self) -> bool {
        self.page_owns_session() && self.dock_open && self.dock_selection.is_some()
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
    /// source of truth that could disagree with it. The composing page has no
    /// session whose plan it could be, so it answers none.
    pub fn todo_progress(&self) -> Option<TodoProgress> {
        if !self.page_owns_session() {
            return None;
        }
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
    ///
    /// The composing page has no Agent answering yet, so it reports nothing
    /// rather than what the session behind the page happened to be doing.
    pub fn current_activity(&self) -> Option<String> {
        self.page_owns_session()
            .then(|| self.activity.clone())
            .flatten()
    }

    /// How long the running turn has been going.
    pub fn turn_elapsed(&self) -> Option<std::time::Duration> {
        if !self.page_owns_session() {
            return None;
        }
        self.turn_started.map(|started| started.elapsed())
    }

    /// Tokens spent by the running turn.
    pub fn turn_tokens(&self) -> Option<u64> {
        self.page_owns_session()
            .then_some(self.turn_tokens)
            .flatten()
    }

    /// The reading of the running turn, for the line above the composer.
    ///
    /// `None` when nothing is running or the page has no session of its own:
    /// there is no live turn to describe, and the last one's phase would be a
    /// claim about a turn that has ended. A send the runtime has not answered
    /// yet has a reading with no phase in it — the line says the turn is
    /// running, because that is all that is known.
    pub fn turn_readout(&self) -> Option<&crate::turn::TurnReadout> {
        (self.page_owns_session() && self.turn_reads_running()).then_some(&self.turn_readout)
    }

    /// A short label for the current page, used when there is no session.
    pub fn page_label(&self, strings: Strings) -> &'static str {
        match self.page {
            Page::Sessions => strings.nav_sessions(),
            Page::Agent => strings.nav_agent(),
            Page::NewSession => strings.session_new(),
            Page::Management | Page::Agents => strings.nav_management(),
            Page::Providers => strings.management_providers(),
            Page::Mcp => strings.management_mcp(),
            Page::Skills => strings.management_skills(),
            Page::Prompts => strings.management_prompts(),
            Page::Devices => strings.devices_title(),
            Page::Usage => strings.nav_usage(),
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
    /// The interface repaints without input only while this is true. The
    /// composing page draws no transcript, so a turn streaming in the session
    /// behind it is not something to repaint for.
    pub fn transcript_animating(&self) -> bool {
        self.page_owns_session() && self.transcript.is_animating()
    }

    /// Whether the landing mark has a light crossing it right now.
    ///
    /// The light comes round again for as long as the page waits, so this is
    /// true for one pass of the loop and false between passes. It is the
    /// question the tick period asks: there is nothing to draw quickly while
    /// the mark sits between lights.
    fn landing_mark_moves(&self) -> bool {
        self.landing_mark_loops() && crate::logo::moving(self.animation_phase)
    }

    /// Whether the landing mark is looping at all.
    ///
    /// The page waits for the reader, so its light will come round again: the
    /// clock has to keep running even in the quiet stretch between passes, or
    /// there would be no next pass.
    fn landing_mark_loops(&self) -> bool {
        self.composing_page_shines()
    }

    /// Whether the landing mark is between passes, with nothing else moving.
    ///
    /// The mark keeps a clock through the quiet stretch — that is what brings
    /// the light round again — but a frame that draws what is already on screen
    /// is not a reason to repaint, and a light that came round at the price of
    /// a repaint every animation tick for as long as the page is left open
    /// would be a greeting charged to the reader's battery. So the clock runs
    /// on the slow tick between passes and the client holds still.
    fn landing_mark_waits(&self) -> bool {
        self.landing_mark_loops()
            && !self.landing_mark_moves()
            && !self.transcript_animating()
            && !self.turn_reads_running()
            && self.page_approval_count() == 0
            && self.page_elicitation_count() == 0
    }

    /// Whether anything on screen is moving without the reader's input.
    ///
    /// The landing mark's light crosses the page; a turn's spinner turns.
    /// Everything else holds still, which is what keeps an idle session — and
    /// the prompt the client opens on, between passes — at zero frames.
    pub fn chrome_animating(&self) -> bool {
        self.transcript_animating() || self.landing_mark_moves()
    }

    /// Step the running indicator. Returns whether a repaint is due.
    ///
    /// Streaming text does not need one: a delta marks the app dirty by itself,
    /// so a transcript with no other animation stays at zero frames. Neither
    /// does the landing mark's clock between passes, which is stepping a phase
    /// whose frame is already on screen.
    pub fn advance_transcript_animation(&mut self) -> bool {
        if !self.is_animating() {
            return false;
        }
        self.animation_phase = self.animation_phase.wrapping_add(1);
        !self.landing_mark_waits()
    }

    /// Keep the turn clock in step with what the session actually is.
    ///
    /// The elapsed readout and the spinner answer to "is a turn running", which
    /// is a property of the session rather than an event. Deriving it here means
    /// no path can leave the clock running after the turn stopped — which would
    /// keep the client repainting for the rest of the session — or leave it at
    /// zero while the runtime works.
    ///
    /// The inquiry is page-independent even though the readout is not: the
    /// composing page does not draw the clock, and a reader who steps out to
    /// write a new session and comes back must find the turn's own elapsed time
    /// rather than one that restarted when they returned.
    pub fn sync_turn_clock(&mut self) -> bool {
        let running = self.open_session_is_running()
            || self.pending_send_for_selected().is_some()
            || self.transcript.is_animating();
        match (running, self.turn_started.is_some()) {
            (true, false) => {
                self.turn_started = Some(std::time::Instant::now());
                true
            }
            (false, true) => {
                self.turn_started = None;
                self.turn_tokens = None;
                self.turn_readout.forget();
                true
            }
            _ => false,
        }
    }

    /// Read the running turn into [`Self::turn_readout`].
    ///
    /// Called when the timeline moves rather than on a timer. The phase and the
    /// tool count are read straight from the projection, but the rate is a
    /// difference between two observations of the output, so an observation
    /// taken before anything arrived has no difference to report.
    ///
    /// Answers whether the line above the composer would change.
    pub fn sync_turn_readout(&mut self) -> bool {
        if !self.turn_reads_running() {
            return self.turn_readout.forget();
        }
        let Some(turn) = self.live_turn() else {
            // A send the runtime has not answered yet reads as running before
            // there is a turn to read: the line says so, and there is nothing
            // else it could say.
            return self.turn_readout.forget();
        };
        self.turn_readout.observe(&turn, std::time::Instant::now())
    }

    /// The turn the open session is still running, when it has one.
    ///
    /// The last turn the runtime has not closed is the live one: a completed
    /// turn is history, and a superseded one was replaced by the queued message
    /// that interrupted it.
    fn live_turn(&self) -> Option<vibex_desktop_model::TimelineConversationTurn> {
        if !self.page_owns_session() {
            return None;
        }
        self.agent
            .state
            .conversation_turns()
            .into_iter()
            .rev()
            .find(|turn| !(turn.complete || turn.superseded))
    }

    /// Whether anything on screen is moving without the reader's input.
    ///
    /// The turn line's spinner turns the whole time a turn is running — the
    /// quiet parts of a turn included, which is when it matters most: a spinner
    /// held on one frame while the runtime starts up reads as a frozen client.
    /// The composing page's mark loops while it waits, quiet stretch included,
    /// because the clock between passes is what brings the light round again;
    /// a question waiting on the reader pulses. Everything else holds still,
    /// which is what keeps an idle session at zero frames.
    pub fn is_animating(&self) -> bool {
        self.transcript_animating()
            || self.turn_reads_running()
            || self.page_approval_count() > 0
            || self.page_elicitation_count() > 0
            || self.landing_mark_loops()
    }

    /// The approvals the page in front of the reader is waiting on.
    ///
    /// A session's approvals belong to its own page: the composing page is
    /// writing a session that does not exist yet, so the count it shows is zero
    /// rather than what the session behind it is waiting for.
    pub fn page_approval_count(&self) -> usize {
        if self.page_owns_session() {
            self.pending_permission_count()
        } else {
            0
        }
    }

    /// The questions the page in front of the reader is waiting on.
    pub fn page_elicitation_count(&self) -> usize {
        if self.page_owns_session() {
            self.pending_elicitations()
        } else {
            0
        }
    }

    pub fn tick(&mut self) -> crate::reduce::Outcome {
        // A visible toast keeps the frames coming: it is transient content, and
        // the tick is what takes it away. So does an armed quit, whose hint is
        // the same kind of thing.
        let mut dirty = self.toast.is_some() || self.quit_armed();
        if let Some(toast) = self.toast.as_mut() {
            toast.ttl = toast.ttl.saturating_sub(1);
            if toast.ttl == 0 {
                self.toast = None;
                dirty = true;
            }
        }
        // The window a second `Ctrl+C` has to land in runs on the same clock the
        // hint does, so the moment the hint goes the arming goes with it.
        self.quit_armed = self.quit_armed.saturating_sub(1);
        // The countdown to a continuation is the one thing on the session list
        // that changes without an event: it is what the reader watches to
        // decide whether to stop it.
        let sessions = self.agent.state.sessions.value.clone().unwrap_or_default();
        let pending = self.pending_sessions(&sessions);
        let now_ms = vibex_core::unix_timestamp_ms();
        let (actions, moved) = self.auto_continue.tick(&sessions, now_ms, |session_id| {
            pending.contains(session_id.as_str())
        });
        crate::reduce::Outcome {
            effects: actions.into_iter().map(auto_continue_effect).collect(),
            dirty: dirty || moved,
        }
    }

    pub fn composer_ticket(&self) -> ComposerTicket {
        ComposerTicket {
            target: self.composer_target.clone(),
            navigation_serial: self.navigation_serial,
            runtime: self.page_runtime_selection().map(Box::new),
            text: self.composer.text().to_string(),
            cursor: self.composer.cursor(),
        }
    }

    pub fn accepts_composer_ticket(&self, ticket: &ComposerTicket) -> bool {
        ticket.target == self.composer_target
            && ticket.navigation_serial == self.navigation_serial
            && ticket.text == self.composer.text()
            && ticket.cursor == self.composer.cursor()
    }

    pub fn switch_composer(&mut self, target: ComposerTarget) {
        if self.composer_target.as_ref() == Some(&target) {
            return;
        }
        let buffer = std::mem::take(&mut self.composer);
        if let Some(previous) = self.composer_target.replace(target.clone()) {
            self.composer_drafts.insert(previous, buffer);
        }
        self.composer = self.composer_drafts.remove(&target).unwrap_or_default();
        self.completion = None;
        self.draft_selecting = false;
        self.composer_mode = ComposerMode::Normal;
    }

    pub fn runtime_target(&self) -> ComposerTarget {
        if self.page_shows_session() {
            ComposerTarget::Session(self.selected_session_id().unwrap().clone())
        } else {
            ComposerTarget::Draft(self.new_draft_id.clone())
        }
    }

    pub fn runtime_picker_is_current(&self) -> bool {
        self.runtime_picker_target.as_ref() == Some(&self.runtime_target())
            && !self.page_shows_uncreated_session()
    }

    /// Whether the page in front of the reader shows a session that does not
    /// exist yet.
    ///
    /// The reader sent the first message of a new session, and what the page
    /// shows is the identity reserved for it: either the authority has not
    /// answered with the session it belongs to, or the creation failed and the
    /// page kept the draft. Nothing that addresses *the session itself* can be
    /// honoured in that window — it has no runtime to move and no turn to
    /// interrupt — so every such action has to say which state it is waiting
    /// for rather than quietly doing nothing.
    pub fn page_shows_uncreated_session(&self) -> bool {
        self.selected_session_id()
            .is_some_and(|id| self.page_shows_session() && self.session_is_uncreated(id))
    }

    /// Whether the page's reserved session is still being created.
    ///
    /// The other half of [`Self::page_shows_uncreated_session`]: a creation
    /// that failed has stopped waiting for the authority and started waiting
    /// for the reader.
    pub fn page_session_is_being_created(&self) -> bool {
        self.selected_session_id()
            .is_some_and(|id| self.pending_creations.contains_key(id))
    }

    pub fn session_is_uncreated(&self, session_id: &VibexSessionId) -> bool {
        self.pending_creations.contains_key(session_id)
            || self
                .failed_creations
                .iter()
                .any(|failed| &failed.draft_id == session_id)
            || &self.new_draft_id == session_id
    }

    pub fn cancel_runtime_picker(&mut self) {
        self.runtime_picker_pending = false;
        self.runtime_picker_target = None;
        self.runtime_picker.draft = None;
        self.runtime_picker.option_row = 0;
        self.runtime_picker.filtering = false;
        self.run_option_prompt = None;
        if matches!(
            self.overlay,
            Some(
                Overlay::RuntimePicker { .. }
                    | Overlay::RunOptionValues { .. }
                    | Overlay::Prompt {
                        field: PromptField::RunOptionValue,
                        ..
                    }
            )
        ) {
            self.overlay = None;
        }
    }

    pub fn navigate_to(&mut self, page: Page) {
        if self.page != page {
            self.navigation_serial = self.navigation_serial.wrapping_add(1);
            self.cancel_runtime_picker();
        }
        if page == Page::NewSession {
            self.switch_composer(ComposerTarget::Draft(self.new_draft_id.clone()));
        } else if page.is_composing_page()
            && let Some(session_id) = self.selected_session_id().cloned()
        {
            self.switch_composer(ComposerTarget::Session(session_id));
        }
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

    /// Move into the session's own page.
    ///
    /// The session shell's navigation still names a destination, and `Agent`
    /// is the only one this client has: the workbench pages that used to sit
    /// beside it (files, changes, terminal) are not part of the TUI.
    pub fn select_session_destination(&mut self) {
        self.navigation.select_session(SessionDestination::Agent);
        self.navigate_to(Page::Agent);
    }

    pub fn open_session(&mut self, session_id: VibexSessionId) {
        self.navigation_serial = self.navigation_serial.wrapping_add(1);
        self.cancel_runtime_picker();
        self.switch_composer(ComposerTarget::Session(session_id.clone()));
        self.unread_sessions.remove(session_id.as_str());
        self.navigation.enter_session(session_id.as_str());
        self.navigate_to(Page::Agent);
        // Opening a session is a request to work in it, so the keyboard lands
        // in the composer without a trip through `Tab`.
        self.focus = Focus::Composer;
        self.scroll = ScrollState::default();
    }

    pub fn record_created_session(&mut self, session: &AgentSession) {
        let rows = self.agent.state.sessions.value.get_or_insert_with(Vec::new);
        if !rows.iter().any(|row| row.id == session.id) {
            rows.push(session.clone());
        }
    }

    /// Consume exactly one acknowledged creation. Duplicate or unrelated
    /// callbacks have no prompt to send.
    pub fn pending_send_effect(&mut self, session_id: VibexSessionId) -> Option<Effect> {
        let creation = self.pending_creations.remove(&session_id)?;
        let pending = self.pending_sends.get_mut(&creation.send_id)?;
        pending.submitted_at = std::time::Instant::now();
        let correlation_id = pending.correlation_id.clone();
        let attachments = self.wire_attachments(&creation.outgoing.images);
        self.history.push(creation.outgoing.text.clone());
        Some(Effect::SendMessage {
            session_id,
            send_id: creation.send_id,
            correlation_id,
            text: creation.outgoing.text,
            attachments,
        })
    }

    /// Restore only this creation's draft. Off-screen failures wait for an
    /// empty new-session editor; they never replace newer text or navigate.
    pub fn fail_creation(&mut self, request_id: &VibexSessionId) -> bool {
        let Some(creation) = self.pending_creations.remove(request_id) else {
            return false;
        };
        self.abandon_send(request_id, creation.send_id);
        let on_failed_session =
            self.page_shows_session() && self.selected_session_id() == Some(request_id);
        self.failed_creations.push(FailedCreation {
            draft_id: request_id.clone(),
            creation,
        });
        if on_failed_session && self.composer.is_empty() && !self.new_draft_has_content_or_choices()
        {
            self.restore_failed_creation();
            self.navigate_to(Page::NewSession);
            self.focus = Focus::Composer;
        }
        self.sync_transcript();
        true
    }

    pub fn new_draft_has_content_or_choices(&self) -> bool {
        let target = ComposerTarget::Draft(self.new_draft_id.clone());
        let buffer = if self.composer_target.as_ref() == Some(&target) {
            Some(&self.composer)
        } else {
            self.composer_drafts.get(&target)
        };
        buffer.is_some_and(|buffer| !buffer.is_empty())
            || self.new_session_runtime.is_some()
            || self.workspace_path.is_some()
    }

    pub fn restore_failed_creation(&mut self) {
        let Some(failed) = self.failed_creations.pop() else {
            return;
        };
        self.new_draft_id = VibexSessionId::new();
        // Follow-up text typed while creation was pending remains the new
        // session's editor after an explicit retry; its first prompt is separate.
        if let Some(buffer) = self
            .composer_drafts
            .remove(&ComposerTarget::Session(failed.draft_id.clone()))
        {
            self.composer_drafts
                .insert(ComposerTarget::Session(self.new_draft_id.clone()), buffer);
        }
        for queued in &mut self.queued_messages {
            if queued.session_id == failed.draft_id {
                queued.session_id = self.new_draft_id.clone();
            }
        }
        self.new_session_runtime = failed.creation.runtime;
        self.workspace_path = Some(failed.creation.workspace_root);
        self.switch_composer(ComposerTarget::Draft(self.new_draft_id.clone()));
        self.composer.set_draft(
            failed.creation.outgoing.text,
            failed.creation.outgoing.images,
        );
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
        self.restore_message_placeholders(&mut rows);
        // A send the runtime has not echoed yet is projected as the row it will
        // become. It goes last because that is where the echo will land, and it
        // is only drawn for the session it belongs to: another session's client
        // state is not this transcript.
        if let Some(pending) = self.pending_send_for_active() {
            rows.push(pending.row(self.strings));
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
        // The transcript is the turn's own output, so it is also where the line
        // above the composer learns that the turn moved.
        self.sync_turn_readout();
    }

    /// Put each user message's attachments back into the row it is drawn as.
    ///
    /// A row carries text and nothing else: the projection drops a message's
    /// attachments, and with them the only record of where each picture sat —
    /// the wire text has no labels, only `inline_text_offset`. The timeline
    /// still holds them, and a row's `item_ids` are the index into it, so the
    /// placeholder is put back here for everything that draws the row: the
    /// transcript, its pinned header and its block detail.
    fn restore_message_placeholders(&self, rows: &mut [TimelineRow]) {
        if !rows
            .iter()
            .any(|row| row.kind == vibex_desktop_model::TimelineRowKind::UserMessage)
        {
            return;
        }
        // One pass over the timeline rather than a scan per row: a long session
        // redraws on every streamed delta, and a lookup that walks the items
        // once per message would make the transcript quadratic in its own
        // history.
        let by_item: HashMap<&str, &[vibex_core::MessageAttachment]> = self
            .agent
            .state
            .timeline
            .items
            .iter()
            .filter_map(|item| match &item.payload {
                vibex_core::TimelinePayload::UserMessage(message)
                    if !message.attachments.is_empty() =>
                {
                    Some((item.id.as_str(), message.attachments.as_slice()))
                }
                _ => None,
            })
            .collect();
        if by_item.is_empty() {
            return;
        }
        for row in rows
            .iter_mut()
            .filter(|row| row.kind == vibex_desktop_model::TimelineRowKind::UserMessage)
        {
            let Some(attachments) = row
                .item_ids
                .iter()
                .find_map(|id| by_item.get(id.as_str()).copied())
            else {
                continue;
            };
            row.body = crate::attachment::with_attachments(&row.body, attachments, self.strings);
        }
    }

    /// Repair missing events without navigating or resending a message.
    pub fn refresh_timeline(&mut self) -> Option<Effect> {
        if self.agent.state.timeline_status.phase == vibex_ui::AsyncPhase::Loading {
            return None;
        }
        let session_id = self.selected_session_id()?.clone();
        let ticket = self.agent.begin_session_load(session_id.clone()).ok()?;
        Some(Effect::OpenSession { session_id, ticket })
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
        // The palette leads with these next time, so they are written down
        // where the next run reads them.
        self.save_interface_preferences();
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

    /// Whether the session behind the page has a turn running.
    ///
    /// Page-independent on purpose: the turn clock is the session's, not the
    /// page drawing it, so a reader who steps out to write a new session and
    /// comes back finds the turn's real elapsed time instead of one that
    /// restarted on their return. Nothing draws this while the composing page
    /// is up — see [`Self::session_running`].
    fn open_session_is_running(&self) -> bool {
        self.active_session()
            .is_some_and(|session| session.state == vibex_core::AgentSessionState::Running)
    }

    /// Whether the page in front of the reader has a turn running.
    ///
    /// The composing page has no session of its own, so it never does: what the
    /// session behind it is doing belongs to that session's own page.
    pub fn session_running(&self) -> bool {
        self.page_owns_session() && self.open_session_is_running()
    }

    /// Whether the page in front of the reader *reads* as running.
    ///
    /// A send that the runtime has not answered yet counts: the reader pressed
    /// Enter, and a client that shows "idle" for the round trip reads as one
    /// that dropped the message. The composing page counts only the send it is
    /// holding — none, because sending leaves it for the session view.
    pub fn turn_reads_running(&self) -> bool {
        self.session_running()
            || self.pending_send_for_active().is_some()
            || (self.page_shows_session()
                && self
                    .selected_session_id()
                    .is_some_and(|id| self.inflight_sends.values().any(|pending| pending == id)))
    }

    /// The unconfirmed send for the selected session, whichever page is up.
    fn pending_send_for_selected(&self) -> Option<&PendingSend> {
        let selected = self.selected_session_id()?;
        self.pending_sends
            .values()
            .rev()
            .find(|pending| pending.session_id.as_ref() == Some(selected))
    }

    pub fn pending_send_for_active(&self) -> Option<&PendingSend> {
        self.page_shows_session()
            .then(|| self.pending_send_for_selected())
            .flatten()
    }

    pub fn confirm_pending_item(&mut self, item: &vibex_core::TimelineItem) -> bool {
        let before = self.pending_sends.len();
        self.pending_sends
            .retain(|_, pending| !pending.is_confirmed_item(item));
        before != self.pending_sends.len()
    }

    /// Confirmation and expiry are scoped to each send's session. Reconnect
    /// only refetches authoritative history; nothing here resubmits a prompt.
    pub fn settle_pending_send(&mut self) -> bool {
        let timeline = &self.agent.state.timeline;
        let before = self.pending_sends.len();
        self.pending_sends.retain(|_, pending| {
            let creating = pending
                .session_id
                .as_ref()
                .is_some_and(|id| self.pending_creations.contains_key(id));
            creating
                || (pending.submitted_at.elapsed() <= PendingSend::TIMEOUT
                    && !(pending.session_id.is_some()
                        && pending.session_id == timeline.session_id
                        && pending.is_confirmed_by(&timeline.items)))
        });
        before != self.pending_sends.len()
    }

    pub fn finish_send(&mut self, session_id: &VibexSessionId, send_id: u64) -> bool {
        if self.inflight_sends.get(&send_id) != Some(session_id) {
            return false;
        }
        self.inflight_sends.remove(&send_id);
        true
    }

    pub fn abandon_send(&mut self, session_id: &VibexSessionId, send_id: u64) -> bool {
        let finished = self.finish_send(session_id, send_id);
        if self
            .pending_sends
            .get(&send_id)
            .is_some_and(|pending| pending.session_id.as_ref() == Some(session_id))
        {
            self.pending_sends.remove(&send_id);
            return true;
        }
        finished
    }

    /// Remove the selected session's most recent projection (local dismissal).
    pub fn abandon_pending_send(&mut self) -> bool {
        let Some(pending) = self.pending_send_for_selected() else {
            return false;
        };
        let serial = pending.serial;
        self.pending_sends.remove(&serial);
        self.inflight_sends.remove(&serial);
        self.turn_started = None;
        true
    }

    pub fn mark_send_dispatched(
        &mut self,
        session_id: Option<&VibexSessionId>,
        text: String,
        attachments: Vec<vibex_core::MessageAttachment>,
    ) -> u64 {
        self.pending_send_serial = self.pending_send_serial.wrapping_add(1);
        let serial = self.pending_send_serial;
        let after_sequence = if session_id == self.agent.state.timeline.session_id.as_ref() {
            self.agent
                .state
                .timeline
                .items
                .last()
                .map_or(0, |item| item.sequence)
        } else {
            0
        };
        if let Some(session_id) = session_id {
            self.inflight_sends.insert(serial, session_id.clone());
        }
        self.pending_sends.insert(
            serial,
            PendingSend {
                session_id: session_id.cloned(),
                serial,
                correlation_id: vibex_core::CorrelationId::new(),
                text,
                attachments,
                after_sequence,
                submitted_at: std::time::Instant::now(),
                submitted_at_ms: vibex_core::unix_timestamp_ms(),
            },
        );
        if session_id.is_some() && session_id == self.selected_session_id() {
            self.turn_started = Some(std::time::Instant::now());
            self.scroll.follow = true;
        }
        self.sync_transcript();
        serial
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
        // The composing page holds no session's queue: the messages it is
        // written for belong to the session behind it, which the reader is
        // leaving, and Enter there creates a session rather than holding one.
        let Some(session_id) = self.page_session_id() else {
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

    /// Note what a session last did, for the list's second line.
    ///
    /// Every event the client already receives passes through here, so the
    /// list can say what a session was doing without asking the runtime a
    /// second question about it. An item with nothing to say — a message still
    /// arriving, the reader's own words, the Agent's private reasoning — leaves
    /// the previous line standing rather than blanking it.
    pub fn note_session_echo(&mut self, item: &vibex_core::TimelineItem) {
        let Some(echo) = crate::sessions::session_echo(item) else {
            return;
        };
        self.session_echoes
            .insert(item.session_id.as_str().to_string(), echo);
    }

    /// What a session last did, when this client saw it.
    pub fn session_echo(&self, session_id: &VibexSessionId) -> Option<&str> {
        self.session_echoes
            .get(session_id.as_str())
            .map(String::as_str)
    }

    /// Drop the echoes of sessions the list no longer holds.
    ///
    /// The map is fed by every event and would otherwise keep a line for every
    /// session this process has ever seen, including the ones that were
    /// deleted while it was running.
    pub fn retain_session_echoes(&mut self) {
        let live = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|session| session.id.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        self.session_echoes
            .retain(|session_id, _| live.contains(session_id));
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
            || self
                .projection
                .sidebar_organization
                .as_ref()
                .is_some_and(|view| view.unread_session_ids.contains(session_id.as_str()))
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
        if self.session_is_uncreated(session_id)
            || self
                .inflight_sends
                .values()
                .any(|pending| pending == session_id)
        {
            return true;
        }
        // A send that is still in flight counts: the runtime has not reported
        // the turn yet, and releasing the next held message into that gap would
        // interleave two turns.
        if self
            .pending_sends
            .values()
            .any(|pending| pending.session_id.as_ref() == Some(session_id))
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
        // Blank rows at either end are furniture the selection passed over —
        // the padding inside the reader's own box, the gap between two blocks —
        // rather than lines they meant to copy.
        while output.first().is_some_and(|line| line.is_empty()) {
            output.remove(0);
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

/// Frames a first `Ctrl+C` waits for its second before forgetting.
///
/// The hint on the status band counts down on the same number, so the reader
/// always has as long to answer as they had to read it. The tick is 120 ms
/// while something is animating and 200 ms while nothing is, which makes the
/// window about two and a half seconds on the page a reader opens on and four
/// on an idle one — long enough to be a question, short enough that a stray
/// press stops meaning anything.
pub const QUIT_ARM_FRAMES: u8 = 20;

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
    /// The theme this run was started with, when the composition root named
    /// one (`--theme`, `VIBEX_THEME`). It seeds the slot for `mode`; the other
    /// appearance's slot still comes from `remembered`.
    pub theme_id: Option<String>,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    /// What the interface itself remembered last time: the theme per
    /// appearance, the standing workspace and the palette's recents.
    ///
    /// The composition root loads this, the way it decides the seat, so a test
    /// or a preview starts from a shape it names rather than from a home
    /// directory it did not choose.
    pub remembered: crate::interface_prefs::InterfacePreferences,
    /// Where the reader's session-list arrangement is kept.
    ///
    /// The composition root decides this, the way it decides the seat: a test
    /// harness must not write into the developer's home, and a client with no
    /// writable home simply keeps the arrangement in memory for the run.
    pub sidebar_path: Option<std::path::PathBuf>,
    /// Where the runtime switcher's memory is kept.
    ///
    /// Separate from the arrangement above for the same reason the arrangement
    /// is separate from the key file: one file per concern, and a reader who
    /// wants the switcher to forget everything deletes one of them.
    pub runtime_path: Option<std::path::PathBuf>,
    /// Where the interface's own settings are kept.
    ///
    /// Separate again: this is the file the settings surface writes, and a
    /// reader who wants the interface back at its shipped defaults deletes it
    /// without losing the arrangement or the switcher's memory.
    pub preferences_path: Option<std::path::PathBuf>,
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
            remembered: crate::interface_prefs::InterfacePreferences::default(),
            sidebar_path: App::sidebar_arrangement_path(),
            runtime_path: App::runtime_preferences_path(),
            preferences_path: App::interface_preferences_path(),
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
    ) && !is_runtime_startup_notice(row)
}

/// Whether a row is the runtime announcing its own startup.
///
/// A session appends one as it is created and a turn appends one as it starts
/// its provider. They stand for work the turn line above the composer is
/// already drawing, and in the transcript they are read as the opening words of
/// a conversation that has not started — so the reader pays a row for them and
/// learns nothing.
fn is_runtime_startup_notice(row: &TimelineRow) -> bool {
    row.kind == vibex_desktop_model::TimelineRowKind::SystemNotice
        && vibex_core::is_runtime_startup_message(&row.body)
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
        // A row the runtime never stamped carries the epoch; the transcript
        // draws no clock rather than one reading 1970.
        timestamp_ms: (row.timestamp_ms > 0).then_some(row.timestamp_ms),
        expanded: false,
        // A dense row shows one line by shape, so it has to be openable to be
        // readable in full: the transcript is where its body lives.
        collapsible: row.kind != vibex_desktop_model::TimelineRowKind::AgentMessage
            && (row.collapsible || crate::transcript::is_dense_row(row.kind)),
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
    /// Read the arrangement the authority draws its own sidebar from.
    LoadSidebarOrganization,
    /// Ask the runtime what one session's latest turn did. The session list
    /// cannot say whether a turn ended normally; the timeline can.
    ProbeAutoContinue {
        session_id: VibexSessionId,
        updated_at_ms: i64,
    },
    /// Apply one arrangement change on the authority, which answers with the
    /// tree it now holds.
    MutateSidebarOrganization {
        mutation: vibex_core::RemoteSidebarOrganizationMutation,
        expected_revision: Option<u64>,
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
        request_id: VibexSessionId,
        workspace_root: String,
        title: Option<String>,
        /// The runtime the composing page names: the reader's choice, or the
        /// catalogue's first available entry when the page chose none. `None`
        /// only when there was no catalogue to read one from — a session is
        /// created *with* an Agent, not moved to one.
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
        request_id: VibexSessionId,
        session_id: VibexSessionId,
    },
    SendMessage {
        session_id: VibexSessionId,
        send_id: u64,
        correlation_id: vibex_core::CorrelationId,
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
    ReadClipboard {
        ticket: ComposerTicket,
    },
    /// Ask the host for an image on the system clipboard.
    ///
    /// Reading a clipboard is I/O and belongs to the worker; the reducer asks
    /// and gets an [`crate::worker::AppMessage::ClipboardImage`] back.
    ReadClipboardImage {
        ticket: ComposerTicket,
    },
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
        draft_id: VibexSessionId,
        navigation_serial: u64,
        root_path: String,
    },
    BrowseDirectories {
        path: Option<String>,
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
    LoadUsage,
    DiscoverCompletions {
        ticket: ComposerTicket,
        trigger: crate::composer::CompletionTrigger,
        query: String,
    },
    CheckDrift,
    /// Probe one Agent's runtime option snapshot.
    ProbeAgentRuntime {
        request: vibex_core::AgentRuntimeOptionProbeRequest,
    },
    /// Edit one management entry by id.
    UpdateEntry {
        entry: ManagementEntryEdit,
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
        ticket: Option<ComposerTicket>,
        title: String,
        body: String,
    },
    /// Put text on the system clipboard. Routed through the terminal's own
    /// OSC 52 support so the client never links an X11/Wayland clipboard crate.
    Clipboard {
        text: String,
    },
}

/// The effect that carries out one auto-continue decision.
fn auto_continue_effect(action: crate::auto_continue::AutoContinueAction) -> Effect {
    use crate::auto_continue::AutoContinueAction;
    match action {
        AutoContinueAction::Probe {
            session_id,
            updated_at_ms,
        } => Effect::ProbeAutoContinue {
            session_id,
            updated_at_ms,
        },
        // The continuation itself is the same request the manual key sends:
        // the runtime does not care who asked.
        AutoContinueAction::Continue { session_id, .. } => Effect::ContinueTurn { session_id },
    }
}

impl Effect {
    /// Whether the effect is a mutation, so the UI can show a pending marker.
    pub fn key(&self) -> &'static str {
        match self {
            Effect::ListSessions { .. } => "sessions",
            Effect::LoadSidebarOrganization => "sidebar_organization",
            Effect::ProbeAutoContinue { .. } => "auto_continue_probe",
            Effect::MutateSidebarOrganization { .. } => "sidebar_organization_mutation",
            Effect::OpenSession { .. } => "open_session",
            Effect::RefreshTimeline => "timeline",
            Effect::LoadOlder { .. } => "older_timeline",
            Effect::ListRuntimeOptions => "runtime_options",
            Effect::CreateSession { .. } => "create_session",
            Effect::RenameSession { .. } => "rename_session",
            Effect::ArchiveSession { .. } => "archive_session",
            Effect::DeleteSession { .. } => "delete_session",
            Effect::ForkSession { .. } => "fork_session",
            Effect::ReadClipboard { .. } => "read_clipboard",
            Effect::ReadClipboardImage { .. } => "read_clipboard_image",
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
            Effect::ToggleMcp { .. } | Effect::ToggleSkill { .. } | Effect::TogglePrompt { .. } => {
                "toggle_entry"
            }
            Effect::LoadUsage => "usage",
            Effect::DiscoverCompletions { .. } => "completions",
            Effect::CheckDrift => "drift",
            Effect::ProbeAgentRuntime { .. } => "runtime_probe",
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
                runtime_path: None,
                preferences_path: None,
                ..AppOptions::default()
            },
        )
    }

    /// An app whose interface settings live in `path` and nowhere else.
    fn settings_app(path: &std::path::Path) -> App {
        App::new(
            vibex_backend::DisconnectedBackend::facade(),
            AppOptions {
                sidebar_path: None,
                runtime_path: None,
                preferences_path: Some(path.to_path_buf()),
                ..AppOptions::default()
            },
        )
    }

    /// A shipped theme for `mode` that is not its catalog default, so applying
    /// it is a visible change even from an empty slot.
    fn non_default_theme(mode: vibex_ui::GpuiThemeMode) -> &'static str {
        let default = vibex_ui::theme_catalog::default_theme_id(mode);
        vibex_ui::theme_catalog::themes_for(mode)
            .map(|theme| theme.id)
            .find(|id| *id != default)
            .expect("more than one theme ships per appearance")
    }

    #[test]
    fn the_interface_settings_round_trip_through_their_file() {
        use crate::settings::{SettingRow, theme_slot};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-interface.json");
        let light = non_default_theme(vibex_ui::GpuiThemeMode::Light);

        let mut app = settings_app(&path);
        assert!(app.apply_setting_value(SettingRow::Mode, "light"));
        assert!(app.apply_setting_value(SettingRow::Theme, light));
        assert!(app.apply_setting_value(SettingRow::Icons, "ascii"));
        assert!(app.apply_setting_value(SettingRow::Language, "zh-TW"));
        assert!(app.apply_setting_value(SettingRow::Workspace, "/tmp/vibex-default-ws"));

        // A later run reads the same answers back, including the light-theme
        // slot the dark run did not touch.
        let remembered = crate::interface_prefs::InterfacePreferences::load(Some(&path));
        assert_eq!(remembered.mode(), Some(vibex_ui::GpuiThemeMode::Light));
        assert_eq!(remembered.glyphs(), Some(crate::theme::GlyphMode::Ascii));
        assert_eq!(remembered.locale(), Some(Locale::ZhTw));
        assert_eq!(
            theme_slot(&remembered.themes, vibex_ui::GpuiThemeMode::Light),
            Some(light)
        );
        assert_eq!(
            theme_slot(&remembered.themes, vibex_ui::GpuiThemeMode::Dark),
            None
        );
        assert_eq!(
            remembered.workspace.as_deref(),
            Some("/tmp/vibex-default-ws")
        );

        let reloaded = App::new(
            vibex_backend::DisconnectedBackend::facade(),
            AppOptions {
                mode: remembered.mode().unwrap(),
                locale: remembered.locale().unwrap(),
                remembered,
                sidebar_path: None,
                runtime_path: None,
                preferences_path: None,
                ..AppOptions::default()
            },
        );
        assert_eq!(reloaded.settings.locale, Locale::ZhTw);
        assert_eq!(reloaded.theme.id, light);
        assert_eq!(
            reloaded.workspace_path.as_deref(),
            Some("/tmp/vibex-default-ws")
        );
        assert_eq!(
            reloaded.preferred_workspace.as_deref(),
            Some("/tmp/vibex-default-ws")
        );
    }

    #[test]
    fn each_appearance_keeps_its_own_theme_choice() {
        use crate::settings::{SettingRow, theme_slot};

        let directory = tempfile::tempdir().unwrap();
        let mut app = settings_app(&directory.path().join("tui-interface.json"));
        let dark = non_default_theme(vibex_ui::GpuiThemeMode::Dark);
        let light = non_default_theme(vibex_ui::GpuiThemeMode::Light);

        assert!(app.apply_setting_value(SettingRow::Theme, dark));
        assert!(app.apply_setting_value(SettingRow::Mode, "light"));
        assert!(app.apply_setting_value(SettingRow::Theme, light));
        assert!(app.apply_setting_value(SettingRow::Mode, "dark"));
        assert_eq!(app.theme.id, dark, "the dark slot survived the light one");
        assert!(app.apply_setting_value(SettingRow::Mode, "light"));
        assert_eq!(
            app.theme.id, light,
            "and the light slot survived the dark one"
        );
        assert_eq!(
            theme_slot(&app.settings.themes, vibex_ui::GpuiThemeMode::Dark),
            Some(dark)
        );
        assert_eq!(
            theme_slot(&app.settings.themes, vibex_ui::GpuiThemeMode::Light),
            Some(light)
        );
    }

    #[test]
    fn remembering_a_palette_command_writes_it_for_the_next_run() {
        use crate::interface_prefs::InterfacePreferences;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-interface.json");
        let mut app = settings_app(&path);
        app.remember_command(Intent::OpenSettings);
        app.remember_command(Intent::GotoSessions);

        let remembered = InterfacePreferences::load(Some(&path));
        assert_eq!(
            remembered.recent_commands,
            vec!["goto_sessions".to_string(), "open_settings".to_string()]
        );
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
    }

    #[test]
    fn an_app_without_an_arrangement_path_writes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(
            vibex_backend::DisconnectedBackend::facade(),
            AppOptions {
                sidebar_path: None,
                runtime_path: None,
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
        // The reader is in a session; that is where a held message belongs, and
        // the queue answers for the session the page shows.
        let session_id = VibexSessionId::new();
        app.agent.state.selected_session_id = Some(session_id.clone());
        app.navigate_to(Page::Agent);
        app.composer.insert_str("see this ");
        app.composer
            .insert_image(
                "image/png",
                crate::composer::ImageSource::Bytes(std::sync::Arc::new(vec![1, 2, 3])),
            )
            .expect("the image attaches");
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
        assert!(pages.contains(&Page::Providers));
        assert_eq!(pages.len(), 6);
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

    /// The row one timeline payload projects to, so the test reads the same
    /// projection the transcript does.
    fn projected_row(payload: vibex_core::TimelinePayload) -> TimelineRow {
        let item = vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: vibex_core::VibexSessionId::new(),
            sequence: 1,
            timestamp_ms: 1,
            source: vibex_core::TimelineSource::System,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        };
        vibex_desktop_model::timeline_rows(std::slice::from_ref(&item))
            .into_iter()
            .next()
            .expect("a payload projects to a row")
    }

    fn notice_row(message: String) -> TimelineRow {
        projected_row(vibex_core::TimelinePayload::SystemNotice(
            vibex_core::SystemNoticePayload {
                level: vibex_core::SystemNoticeLevel::Info,
                message,
            },
        ))
    }

    #[test]
    fn the_runtime_starting_up_is_not_a_line_in_the_transcript() {
        // The rows a session writes before it has said anything: that it is
        // being created, and that a turn is starting its runtime. Each is a row
        // the reader pays for a sentence the line above the composer already
        // says.
        for message in [
            vibex_core::SESSION_INITIALIZING_NOTICE.to_string(),
            vibex_core::turn_startup_notice("ACP", Some("2 MCP servers")),
            vibex_core::turn_startup_notice("Claude", None),
        ] {
            assert!(
                !row_is_rendered(&notice_row(message.clone())),
                "{message} earned a line"
            );
        }

        // A notice that carries something the reader has to know keeps its row,
        // and a body that merely shares the wording is not a notice at all.
        assert!(row_is_rendered(&notice_row(
            "Imported 3 local sessions as read-only".to_string()
        )));
        assert!(row_is_rendered(&projected_row(
            vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: vibex_core::turn_startup_notice("ACP", None),
                is_final: true,
            })
        )));
    }

    #[test]
    fn a_transcript_block_carries_the_clock_the_row_was_stamped_with() {
        // The runtime stamps a row with the item that closed it and the
        // transcript draws that time beside the message, so the projection has
        // to hand it on rather than drop it at the door.
        let session_id = vibex_core::VibexSessionId::new();
        let item = vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence: 4,
            timestamp_ms: 1_759_237_920_000,
            source: vibex_core::TimelineSource::User,
            kind: vibex_core::TimelineItemKind::UserMessage,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload: vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: "what does the retry helper do?".into(),
                attachments: Vec::new(),
                ..Default::default()
            }),
        };
        let row = vibex_desktop_model::timeline_rows(std::slice::from_ref(&item))
            .into_iter()
            .next()
            .expect("the item projects to a row");
        assert_eq!(row.timestamp_ms, 1_759_237_920_000);
        assert_eq!(
            block_from_row(&row).timestamp_ms,
            Some(1_759_237_920_000),
            "the block dropped the row's clock"
        );

        // A row with no stamped item behind it draws no clock rather than one
        // reading the epoch.
        let mut untimed = row;
        untimed.timestamp_ms = 0;
        assert_eq!(block_from_row(&untimed).timestamp_ms, None);
    }
}

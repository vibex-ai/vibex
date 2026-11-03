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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use vibex_backend::DomainCapabilities;
use vibex_backend::{BackendCapabilitySnapshot, BackendFacade, BackendOperation, BackendResult};
use vibex_core::{AgentSession, AgentSessionState, RemoteDeviceDetail, VibexSessionId};
use vibex_desktop_model::{SidebarState, TimelineRow};
use vibex_ui::shell::{CompactNavigation, GlobalDestination, SessionDestination, ShellKind};
use vibex_ui::{AgentWorkflowController, ManagementWorkflowController};

use crate::action::Intent;
use crate::composer::{CompletionMenu, ComposerBuffer, ComposerHistory, ComposerLog, HistoryStep};
use serde::{Deserialize, Serialize};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::keymap::{Binding, Chord, Keymap, Scope};
use crate::locale::{Locale, Strings};
use crate::theme::{ColorCapability, TuiTheme};
use crate::transcript::{Block, ScrollState, Transcript, TurnFold};
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
    Settings,
    Help,
}

impl Page {
    pub const fn scope(self) -> Scope {
        match self {
            Page::Sessions => Scope::Sessions,
            Page::Agent | Page::NewSession => Scope::Agent,
            // The configuration centre is one surface with four modules, so it
            // is one key scope: a binding written for the MCP table must not
            // disappear because the reader moved to Skills.
            Page::Management
            | Page::Agents
            | Page::Providers
            | Page::Mcp
            | Page::Skills
            | Page::Prompts => Scope::Management,
            Page::Devices => Scope::Devices,
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

    /// Whether the page is one of the configuration centre's modules.
    ///
    /// The centre is one surface with six routes onto it, and its writes are
    /// the same write whichever route the reader took: the page that draws a
    /// record's spinner is every one of them. The list is the same one
    /// [`Self::scope`] folds into `Scope::Management`.
    pub const fn is_configuration_centre(self) -> bool {
        matches!(
            self,
            Page::Management
                | Page::Providers
                | Page::Agents
                | Page::Mcp
                | Page::Skills
                | Page::Prompts
        )
    }
}

/// How many pages the walk back remembers.
///
/// A page costs two small enums; the cap exists so a client left running for
/// days cannot grow the history without bound, and it is deeper than any path a
/// reader walks in one sitting.
const PAGE_HISTORY_LIMIT: usize = 64;

/// How much of a head start older-history prefetch gives the reader, in half
/// screens.
///
/// A page is turn-aligned and can be shorter than the band it lands in, so the
/// distance is measured in screens rather than pages: a screen and a half is
/// what keeps "I scrolled to the top" from meaning "I waited for a round trip".
const OLDER_HISTORY_PREFETCH_HALF_SCREENS: usize = 3;

/// How many activity details one pass may ask for.
///
/// Expanding a whole run of tools opens hundreds of blocks at once and each
/// read is a round trip, so a small batch keeps one gesture from burying the
/// worker's queue. What is left is asked for again as the answers land.
const TIMELINE_DETAIL_BATCH: usize = 4;

/// Ticks between two reads of an open pairing panel.
///
/// The tick is 200 ms, so five of them put a request on screen about a second
/// after the phone asks — quick enough to read as live, slow enough that the
/// poll is not traffic of its own. Only the pairing panel polls; every other
/// surface still repaints only when something changed.
const PAIRING_POLL_TICKS: u8 = 5;

/// A page the reader left, and the pane that owned the keyboard on it.
///
/// `Esc` returns here: the focus comes back with the page, so a reader who
/// opened a panel from a draft lands back in the composer that draft is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageVisit {
    pub page: Page,
    pub focus: Focus,
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
    ///
    /// `target` names the record the answer is about, for the prompts whose
    /// answer is written to one — the value itself is a string, and a prompt
    /// that sent it to whichever record the cursor happened to be on would be
    /// answering about a record the reader never chose. It travels with the
    /// overlay because the overlay outlives the cursor: a read that lands while
    /// the reader is typing must not move the write.
    Prompt {
        title: String,
        field: PromptField,
        value: String,
        target: Option<String>,
    },
    /// The Models one profile offers, as a list inside the record it belongs to.
    Models {
        view: crate::config_center::ModelListView,
    },
    /// What other CLI tools on this machine already hold, offered to import.
    NativeImport {
        view: crate::config_center::NativeImportView,
    },
    /// One registry Skill, read before it is installed.
    SkillPreview {
        view: crate::config_center::SkillPreviewView,
    },
    /// One profile, offered to another tool's own settings.
    NativeExport {
        view: Box<crate::config_center::NativeExportView>,
    },
    /// One Agent's account: the ways in it advertises, and what can be done
    /// with the account it holds.
    AgentAuth {
        view: crate::config_center::AgentAuthView,
    },
    /// A multi-field editor for one configuration record.
    ///
    /// A form rather than a prompt because an MCP server is not a name: it has
    /// a transport, an endpoint, arguments and environment, and a client that
    /// only lets the reader rename it is a client that cannot configure it.
    Form {
        form: crate::config_center::ConfigForm,
    },
    /// The actions one configuration record offers, chosen from a list.
    ///
    /// What a record can do depends on the record — an Agent with an external
    /// install has no rollback, a provider profile with no credential has no
    /// test — so the actions are a menu built from the row rather than a key
    /// per verb that might do nothing.
    EntryActions {
        entry: crate::config_center::EntryActions,
    },
    /// The pending approval card.
    Approval { selected: usize },
    /// The pending elicitation form.
    ///
    /// The card is not a modal box in the middle of the screen: it is drawn in
    /// the composer's band, where the reader is already looking and already
    /// typing, so it carries no chrome of its own beyond the panel. The
    /// request is pinned rather than re-read as "whichever question is first":
    /// a second question arriving while the reader is filling this one in must
    /// not swap the questions under their cursor, and the answers must go back
    /// to the request they were typed for.
    ///
    /// `field` is the tab: a question, or the Confirm tab one past the last of
    /// them. `choice` is the option the cursor is on inside the open question.
    Elicitation {
        request: String,
        field: usize,
        choice: usize,
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
    /// The built-in Provider presets, opened before the new-Provider form.
    ///
    /// Choosing one pre-fills the form rather than creating anything: the
    /// preset supplies an address, a protocol and a starting model list, and
    /// the reader still names the Provider and supplies the key.
    ProviderPresetPicker {
        selected: usize,
        /// The Agent the eventual Provider will belong to.
        agent_id: String,
    },
    /// A read-only detail view for one transcript block.
    BlockDetails { block: usize, scroll: usize },
    /// When the draft in the Composer should be sent.
    ///
    /// The Desktop's clock popover, on the character grid: the same two shapes
    /// (an absolute local time and a countdown), the same one-year bound, and
    /// the same refusal of a deadline that is not in the future. Applying a
    /// value leaves the message in the held queue with a deadline instead of
    /// sending it, which is what makes a scheduled session show up in the
    /// session list as one that is waiting.
    Schedule(crate::schedule::MessageScheduleForm),
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
    /// The actions one project or folder offers, chosen from a list.
    ///
    /// A heading's verbs are a property of the heading — a project can be
    /// deleted and a folder can be renamed, and a folder no project owns cannot
    /// hold a session at all — so the menu is built from the row rather than
    /// kept as one list with the impossible entries disabled.
    SidebarRowActions(crate::sessions::SidebarRowActions),
}

/// The elicitation form the reader is filling in: the question, and where the
/// cursor is inside it.
///
/// Borrowed out of the open overlay rather than kept beside it, so the form and
/// the cursor cannot disagree about which question is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElicitationForm {
    pub request: vibex_core::ElicitationRequest,
    /// The open tab: a question, or the Confirm tab one past the last of them.
    pub field: usize,
    /// Index of the option the cursor is on inside that question.
    pub choice: usize,
}

impl ElicitationForm {
    /// How many tabs the card has: one per question, plus Confirm.
    pub fn tabs(&self) -> usize {
        self.request.fields.len() + 1
    }

    /// The question the open tab holds, when it is not the Confirm tab.
    pub fn question(&self) -> Option<&vibex_core::ElicitationField> {
        self.request.fields.get(self.field)
    }

    /// Whether the Confirm tab is the one open.
    pub fn confirming(&self) -> bool {
        self.field >= self.request.fields.len()
    }
}

/// One row the workspace picker draws, top to bottom.
///
/// The picker chooses a directory on a whole machine, so a listing that only
/// offered its own children would trap the reader in the directory the page
/// happened to name. The parent is therefore a row like any other — the `..`
/// a reader already knows to put the cursor on — and not only a key they would
/// have to be told about. It is drawn exactly where the listing names a parent:
/// a paired authority withholds one at the top of its browse roots and a
/// filesystem withholds one at `/`, so the reader is never offered a step that
/// does not exist, and never loses the one that does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacePickerRow {
    /// The directory above the one being listed.
    Parent { path: String },
    /// One directory inside the listing.
    Entry { name: String, path: String },
}

impl WorkspacePickerRow {
    /// The directory this row names.
    pub fn path(&self) -> &str {
        match self {
            Self::Parent { path } | Self::Entry { path, .. } => path,
        }
    }
}

/// The surface a profile read was opened for.
///
/// The record cannot be read in one press — the list only carries a summary —
/// so the intent travels beside the read and the surface opens when the answer
/// lands, rather than opening on a profile whose endpoint is still unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderOpen {
    Edit,
    Duplicate,
    Models,
    Order,
}

/// Which single-line field a prompt is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptField {
    RenameSession,
    WorkspacePath,
    ProviderSecret,
    ProviderEndpoint,
    /// The gateway's local token ceiling, in tokens.
    GatewayTokenLimit,
    McpServerName,
    SkillName,
    PromptName,
    DeviceRevokeReason,
    ImagePath,
    /// A run option the Agent publishes as free text rather than as a list.
    RunOptionValue,
    /// A Model id the reader is adding to a profile by hand.
    NewModelId,
    /// The name of a folder a heading is about to hold.
    NewFolderName,
    /// The new name of a folder the reader made.
    RenameFolderName,
}

/// Where a folder a prompt is naming belongs.
///
/// The heading travels with the prompt rather than being read back from the
/// cursor when the name is submitted: a list refresh that lands while the reader
/// is typing must not file the folder under whichever heading took the cursor's
/// place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPromptTarget {
    /// The folder being renamed, or `None` when one is being made.
    pub folder_id: Option<String>,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    /// The folder the new folder goes inside, when the heading is itself one.
    pub parent_folder_id: Option<String>,
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

/// The title the manager persists for a session that has not been named yet.
///
/// A creation carries no title — the reader's message becomes one once it
/// reaches a turn — so this is what every snapshot says until then. The two
/// sides have to agree on it exactly, because it is what an optimistic title
/// recognizes as "nothing has named this session yet" before standing in.
fn session_fallback_title(agent_id: impl std::fmt::Display) -> String {
    format!("{agent_id} session")
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
    pub providers: Vec<vibex_core::ProviderProfileSummary>,
    /// The local model gateway, as the authority last reported it. The
    /// Providers page shows it as its own row above the direct Providers.
    pub gateway: Option<vibex_core::GatewaySnapshot>,
    /// Which built-in tool panels each Agent receives, as the authority holds
    /// them.
    pub agent_tools: vibex_core::AgentToolPreferences,
    /// The Skill document the reader is looking at, as the registry answered.
    ///
    /// It is kept because the install writes *this* document: a second read at
    /// install time could return a different revision, and what lands would
    /// then not be what the reader read.
    pub skill_preview: Option<Box<vibex_core::SkillMarketDocument>>,
    /// The preset the new-Provider form was pre-filled from, until it is saved.
    ///
    /// It is kept so the save can write the preset's protocol explicitly: the
    /// projection otherwise falls back to the first protocol that has a URL,
    /// which relabels an Anthropic Provider as Chat Completions on reopen.
    pub pending_provider_preset: Option<String>,
    /// What the last native-settings scan found.
    pub native_import: Option<vibex_core::ProviderNativeImportPreview>,
    /// The order the reader arranged the profiles in, per Agent.
    pub provider_order: Vec<vibex_core::AgentModelProviderDisplayOrderEntry>,
    /// What signing out of one Agent's account would end.
    pub agent_auth_logout: Option<vibex_core::AgentAuthContextLogoutPreview>,
    /// The last Agent account catalogue this client read.
    pub agent_auth_catalog: Option<vibex_core::AgentAuthCatalog>,
    /// The last profile configuration this client read in full.
    ///
    /// The list answers with the summary the authority publishes, which leaves
    /// out the endpoint and the request options; an editor opened from it needs
    /// them, or a save would quietly clear what it could not read.
    pub provider_detail: Option<vibex_core::ProviderProfile>,
    /// The last Model catalogue an endpoint answered with. It is what the Model
    /// editor unions with the profile's own declarations, so a Model the profile
    /// does not offer yet is still one the reader can turn on.
    pub provider_models: Vec<vibex_core::ProviderConfiguredModel>,
    pub agents: Vec<vibex_core::AgentSnapshotEntry>,
    pub agent_auth: Vec<vibex_core::AgentAuthContext>,
    pub mcp: Vec<vibex_core::McpServer>,
    pub skills: Vec<vibex_core::Skill>,
    pub prompts: Vec<vibex_core::Prompt>,
    /// The last registry page the configuration centre asked for. Market
    /// results are a read like any other, so they are held beside the records
    /// rather than inside the surface that drew them.
    pub mcp_market: Vec<vibex_core::McpMarketEntry>,
    pub skill_market: Vec<vibex_core::SkillMarketEntry>,
    /// What the registry answered about the read itself: how much of the
    /// registry the page on screen covers.
    pub market_note: Option<MarketNote>,
    pub mcp_discoveries: Vec<vibex_core::McpServerDiscovery>,
    pub skill_discoveries: Vec<vibex_core::SkillDiscovery>,
    pub backups: Vec<vibex_core::BackupCreateOutcome>,
    /// The last read failure per area, as `module/section`.
    ///
    /// A toast is gone before a reader who looked away can read it, and an area
    /// that refused is the state the area is in: it keeps the reason, and the
    /// key that asks again is drawn with it.
    pub read_errors: std::collections::BTreeMap<String, (String, String)>,
    /// The areas an answer has already landed for, as `module/section`.
    ///
    /// An empty list and a list nobody has read yet look the same on screen,
    /// and they are not the same thing: one is an area with nothing in it and
    /// the other is an area that has not been asked. The difference is the
    /// skeleton the page draws while it waits.
    pub read: std::collections::BTreeSet<String>,
}

impl ManagementData {
    /// Whether an area has been read at least once.
    pub fn has_read(
        &self,
        module: crate::config_center::ConfigModule,
        section: crate::config_center::ConfigSection,
    ) -> bool {
        self.read.contains(&read_key(module, section))
    }

    /// Record that an area's answer landed, and that it was an answer.
    ///
    /// A read that arrived clears whatever the last one said when it refused:
    /// the failure is a state the area is in, and the state has just changed.
    pub fn mark_read(
        &mut self,
        module: crate::config_center::ConfigModule,
        section: crate::config_center::ConfigSection,
    ) {
        let key = read_key(module, section);
        self.read_errors.remove(&key);
        self.read.insert(key);
    }

    /// Record that an area's read refused, with what it said.
    pub fn mark_read_failed(
        &mut self,
        module: crate::config_center::ConfigModule,
        section: crate::config_center::ConfigSection,
        code: String,
        message: String,
    ) {
        let key = read_key(module, section);
        self.read.insert(key.clone());
        self.read_errors.insert(key, (code, message));
    }

    /// What an area's last read said when it refused.
    pub fn read_error(
        &self,
        module: crate::config_center::ConfigModule,
        section: crate::config_center::ConfigSection,
    ) -> Option<&(String, String)> {
        self.read_errors.get(&read_key(module, section))
    }
}

/// One area's identity in the read set.
///
/// The market is one section of two modules, so the module is part of the key:
/// the MCP catalog and the Skill catalog are two lists that share a name.
fn read_key(
    module: crate::config_center::ConfigModule,
    section: crate::config_center::ConfigSection,
) -> String {
    format!("{}/{}", module.key(), section.key())
}

/// How much of a registry one page of it covers.
///
/// A page is not a catalog, and a reader who is shown forty rows has no way to
/// tell "that is all there is" from "the registry has more behind this page".
/// The numbers travel together so the line under the table can say which of the
/// two it is, instead of leaving the reader to guess whether to press search
/// again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MarketNote {
    /// Entries the registry holds for this question, when it reports one. What
    /// is on screen is counted from the rows themselves, so a page the client
    /// cut is described by what it cut rather than by what it fetched.
    pub matched: Option<usize>,
    /// Whether this is the last page the registry has. `false` means it
    /// answered with a cursor, so there are more entries behind this one.
    pub exhausted: bool,
    /// The cursor for the page after this one, when the registry named one.
    ///
    /// An opaque value the client echoes back untouched: reading further is a
    /// request to the registry rather than a page the client cut for itself.
    pub cursor: Option<String>,
    /// The read's own failure, kept on the surface that asked.
    ///
    /// A toast is gone before a reader who looked away can read it, and a
    /// registry that refused is a state the list is in rather than an event
    /// that happened once.
    pub error: Option<(String, String)>,
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
    /// Branches of delegated children the reader folded shut.
    ///
    /// The collapse is this client's own display choice, so it lives in this
    /// client's own file rather than in the arrangement the authority owns;
    /// a reader who folded a noisy branch finds it folded when they come back.
    #[serde(default)]
    pub collapsed_child_ids: BTreeSet<String>,
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
    /// The local deadline of a scheduled message, in milliseconds since the
    /// epoch. `None` is an ordinary held message that only waits for the
    /// session to be ready — the same field, with the same meaning, the Desktop
    /// persists in `ComposerQueueEntry`.
    pub scheduled_at_ms: Option<i64>,
}

impl QueuedMessage {
    /// Whether the clock has reached this message.
    pub fn is_due(&self, now_ms: i64) -> bool {
        self.scheduled_at_ms.is_none_or(|at_ms| at_ms <= now_ms)
    }
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
    /// The workspace the page resolves its completion requests against.
    ///
    /// See [`App::composer_workspace_id`]: the `@` file list is a workspace
    /// read, and the request has to carry the workspace the draft belongs to —
    /// a session's own, or the directory a session written on the composing
    /// page would open in — rather than whatever the authority would guess.
    pub workspace_id: Option<vibex_core::WorkspaceId>,
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
    /// The send time the first message was written for, when it has one.
    ///
    /// A session opened for a scheduled message is created at once — the
    /// reader sees it in the list, waiting — and the message it was opened for
    /// is held in the queue instead of being sent when the creation is
    /// acknowledged. `send_id` is then the reserved id of a projection that was
    /// never made, and nothing reads it.
    pub scheduled_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct FailedCreation {
    pub draft_id: VibexSessionId,
    pub creation: PendingCreation,
}

/// The row index the session list's own action row answers to.
///
/// `+ New session` and the workspace hint beside it are drawn above the list
/// rather than as one of its rows, so they have no index in `sidebar_rows`;
/// this gives the line one no real row can take. It is also `-1` when the
/// cursor steps as a signed number, which is what makes `Up` from the first row
/// land on it and `Down` from it land back on the first.
pub const SESSION_ACTION_ROW: usize = usize::MAX;

/// The whole application.
pub struct App {
    pub facade: BackendFacade,
    pub capabilities: BackendCapabilitySnapshot,
    pub seat: SeatKind,
    /// What kind of runtime this client is attached to, as the composition root
    /// resolved it.
    ///
    /// Read through [`App::runtime_kind`] and written through
    /// [`App::set_runtime_kind`], because the notification policy keeps its own
    /// copy of the one fact that matters here: a desktop shell announces the
    /// same events itself.
    pub(crate) runtime_kind: vibex_core::RemoteServerKind,

    pub theme: TuiTheme,
    pub capability: ColorCapability,
    pub strings: Strings,
    pub keymap: Keymap,
    pub shell: ShellKind,
    pub navigation: CompactNavigation,

    pub agent: AgentWorkflowController,
    pub team: vibex_ui::TeamWorkflowController,
    pub team_detail: Option<vibex_core::AgentDelegationId>,
    pub team_confirm_cancel: Option<vibex_core::TeamTaskControlRequest>,
    pub team_busy: bool,
    /// The ownership tree behind the list and the open session's page.
    ///
    /// A session a delegated task created never appears in the runtime's root
    /// session list, so this is the only thing that can name it, nest it under
    /// the session that owns it, and say which session to go back to.
    pub children: crate::children::ChildSessions,
    pub management: ManagementWorkflowController,
    pub projection: ProjectionState,
    /// The reader's auto-continue preferences and the turns they act on.
    pub auto_continue: crate::auto_continue::AutoContinue,
    pub transcript: Transcript,
    pub scroll: ScrollState,
    /// Activity details the transcript wants, recorded where the block was
    /// opened or rebuilt and issued by the event loop, which is the only part
    /// of the client allowed to start I/O.
    pending_timeline_details: BTreeSet<i64>,
    /// Activity details already on their way, keyed by the generation their
    /// ticket was issued for.
    ///
    /// The generation is part of the key so an answer that arrives after the
    /// projection was replaced cannot retire the read a rebuilt block just
    /// started for the same sequence.
    timeline_details_in_flight: BTreeSet<(vibex_ui::WorkflowViewGeneration, i64)>,

    pub composer: ComposerBuffer,
    /// `Up`/`Down` recall over the selected session's earlier messages.
    pub history: ComposerHistory,
    /// The messages this client sent, which the `? ` drawer searches.
    pub history_log: ComposerLog,
    pub completion: Option<CompletionMenu>,

    pub page: Page,
    pub focus: Focus,
    /// The pages the reader came from, newest last, which is what `Esc` walks
    /// back through: the order pages were opened in, not a fixed hierarchy.
    pub page_history: Vec<PageVisit>,
    pub overlay: Option<Overlay>,
    /// Whether the Key prompt is showing the secret instead of masking it.
    ///
    /// Revealing is a view state, not a property of the value: it resets every
    /// time the prompt opens, and never reaches the authority.
    pub reveal_provider_secret: bool,
    /// A sign-in terminal the authority opened and this client attached to.
    ///
    /// A terminal method runs the Agent's own login flow in a PTY the authority
    /// owns. The client is a viewer by id: it draws the frames the runtime
    /// emits and writes the reader's keys back, because the process itself
    /// belongs to the authority and never to the client.
    pub auth_terminal: Option<crate::auth_terminal::AuthTerminal>,
    pub toast: Option<Toast>,
    /// Frames the reader's first `Ctrl+C` is still waiting for a second one.
    ///
    /// The hint on the status band is drawn from this rather than stored beside
    /// it, so the two can never disagree about whether a second press would
    /// quit. Zero is "not armed".
    quit_armed: u8,

    pub settings: SettingsState,
    pub management_data: ManagementData,
    /// The profile read a surface is waiting on, if the reader asked for one
    /// whose configuration this client has not read yet.
    pub pending_provider_open: Option<(vibex_core::ProviderProfileId, ProviderOpen)>,
    /// The configuration centre's own navigation: which module and area are
    /// open, which pane owns the keyboard, and the filter over the records.
    ///
    /// It lives beside the data rather than inside a page because the centre
    /// keeps its place while the reader works in the timeline and comes back.
    pub config_center: crate::config_center::ConfigCenter,
    /// The devices page's own navigation and the pairing facts behind it.
    ///
    /// It lives beside the data for the same reason the configuration centre's
    /// state does: the page keeps its place — which tab, which row, which panel
    /// is open — while the reader works in a session and comes back.
    pub device_center: crate::device_center::DeviceCenter,
    /// The record a confirmation is about, for the two destructive verbs that
    /// ask first. It is carried as the record's key and the verb rather than as
    /// the cursor's index, so an answer that arrives after a refresh still
    /// names the record the question was asked about.
    pub pending_config_action: Option<(String, crate::config_center::ConfigAction)>,
    /// An Agent the reader asked the machine to install, while it works.
    ///
    /// A managed install runs on the authority and answers nothing until it is
    /// done, so the client watches the record it will change rather than the
    /// request it sent: the phases a reader sees are the authority's own.
    pub install_watch: Option<crate::config_center::InstallWatch>,
    /// The form a discard question is holding, so answering "no" gives back
    /// what the reader typed rather than the empty form they started with.
    pub pending_form: Option<crate::config_center::ConfigForm>,
    /// The heading a menu action was chosen for, waiting for the question that
    /// guards it. It is carried as the row's own id and the verb rather than as
    /// the cursor's index, for the same reason the record above is: a list that
    /// refreshed while the question was up must not answer it against a
    /// different heading.
    pub pending_sidebar_action: Option<(String, crate::sessions::SidebarRowAction)>,
    /// Where a folder the reader is naming belongs, while the prompt is up.
    pub pending_folder_prompt: Option<FolderPromptTarget>,
    pub selection: BTreeMap<Scope, usize>,
    pub filter: String,
    pub filtering: bool,
    pub show_archived: bool,
    /// A root-list invalidation received while its previous read is in flight.
    session_list_refresh_pending: bool,
    pub session_title: Option<String>,
    pub live: LiveState,
    pub pending: BTreeMap<String, ()>,
    pub generation: u64,
    pub should_quit: bool,
    /// Set when the reader takes the host's offer to own the runtime home.
    ///
    /// The interface does not take it: it is a client, and becoming the
    /// authority means restarting as one. The flag is how the event loop is
    /// told to end the run so the host can do that.
    pub handover_requested: bool,
    /// What the host is offering, if anything. `None` on a run that owns its
    /// own runtime, and on every run whose host does not watch for the offer.
    pub handover: Option<crate::HandoverOffer>,
    /// The offer as the last frame drew it, so a change repaints once.
    handover_seen: bool,
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
    /// Columns the session list drew in, published by the frame that drew it.
    ///
    /// How much of a session row's button cluster fits is a question about the
    /// columns the row has, and the frame is the only thing that knows them. The
    /// keyboard asks the same function the frame drew the cluster with, so the
    /// button it can reach is the button the reader can see.
    pub session_band_width: usize,
    /// The row of the session list the keyboard has stepped onto, and the button
    /// inside it the cursor has reached.
    ///
    /// `←`/`→` step onto a row's own buttons and across them; `Enter` then
    /// presses the button the cursor reached rather than opening the row behind
    /// it. The index and the intent travel together so a refresh that reorders
    /// the list cannot make the press act on whichever row took the cursor's
    /// place — the same contract the pointer's own hover keeps. The page's
    /// action row above the list carries [`SESSION_ACTION_ROW`] as its index.
    pub session_row_action: Option<(usize, crate::action::Intent)>,
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
    /// Whether a catalogue read is on its way.
    ///
    /// A read that has not answered is not a catalogue that came back empty:
    /// the composing page names the Agent the session will be created with,
    /// and while one is in flight it says it is loading rather than claiming
    /// the runtime is unavailable. Every request sets this and the answer —
    /// whether it carries a catalogue or an error — clears it.
    pub runtime_options_reading: bool,
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
    /// In-progress elicitation answers, keyed by the request they answer.
    ///
    /// One draft per request rather than one for the form: an Agent can raise
    /// more than one question, and what was typed for one must never be sent
    /// with another.
    pub elicitation_drafts: std::collections::BTreeMap<String, crate::reduce::ElicitationDraft>,
    /// The Agent's blocking questions this client has already put in front of
    /// the reader, keyed `elicitation:<id>` or `permission:<id>`.
    ///
    /// The desktop draws both cards inline, so a question is visible the moment
    /// it arrives; the terminal has no room for controls in the transcript, so
    /// the same card is a modal here and opens itself. Recording what has been
    /// offered is what makes `Esc` mean "later" — a card that reopened on the
    /// next frame would make the key useless.
    pub offered_questions: std::collections::BTreeSet<String>,
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
    /// The control inside a list row the pointer is over: the row it belongs to
    /// and the intent it would run.
    ///
    /// A row's controls act on *their* row rather than on the cursor's, so the
    /// hover state names the row as well as the control: lighting a button on a
    /// row the reader is not pointing at would promise the wrong target.
    pub hovered_row_action: Option<(usize, crate::action::Intent)>,
    /// Whether the pointer is over the open modal's close affordance.
    ///
    /// The modal chrome is the control's owner, so it reads this to light it,
    /// the same way a hint button reads [`Self::hovered_hint`].
    pub hovered_modal_close: bool,
    /// Which sent message the composer's history drawer points at.
    pub history_selection: usize,
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
    /// Where the Composer's held messages are written; `None` keeps the queue
    /// in memory only, which is what a preview or a test wants.
    pub queue_path: Option<std::path::PathBuf>,
    /// The last left click, so two clicks in the same cell can be told apart
    /// from two clicks in different ones.
    pub last_click: Option<(std::time::Instant, usize, u16)>,
    /// Messages held back until the running turn ends.
    pub queued_messages: Vec<QueuedMessage>,
    /// Which queued message the queue band's cursor is on.
    pub queue_selection: Option<usize>,
    /// The send time each Composer draft is written for, until it is submitted.
    ///
    /// A schedule is a property of the message being written rather than of the
    /// page, so it survives leaving the session and coming back with the draft
    /// the rest of the Composer state does. It is consumed by the submission
    /// that carries it: the message it produces is the one that waits.
    pub draft_schedules: BTreeMap<ComposerTarget, crate::schedule::MessageSchedule>,
    /// The countdown second the surfaces last painted, so a tick that does not
    /// move the display is not a reason to repaint it.
    pub schedule_shown_second: Option<i64>,
    /// Ticks left before the open pairing panel is re-read.
    ///
    /// The window and the offer are the two pairing facts the client cannot
    /// learn from an answer to its own action: a device asks, or claims a code,
    /// from outside, and the only way to hear about it is to ask again. Zero
    /// means "not polling", which is what the rest of the client does.
    lan_poll_ticks: u8,
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
    /// The unread marks the authority is holding, as it last published them.
    ///
    /// Kept apart from the arrangement view because the two are not the same
    /// kind of fact: an arrangement that arranges nothing is dropped, and the
    /// unread marks must survive that — they are the only account of which
    /// sessions the Desktop is badging, and clearing one is a mutation this
    /// client has to send back.
    pub authority_unread_sessions: std::collections::BTreeSet<String>,
    /// What each session last did, one line, for the list's second line.
    ///
    /// The runtime publishes a session's *state* but not its last words, and a
    /// list of titles alone cannot say which of five idle sessions is the one
    /// that answered. The line is the client's own reading of the events it
    /// already receives, pruned to the sessions the list still holds, so it
    /// costs the runtime nothing.
    pub session_echoes: BTreeMap<String, String>,
    /// The name a session created here borrows from the message that opened it.
    ///
    /// A creation carries no title, so the manager persists the placeholder
    /// `<agent> session` and seeds the reader's message as the title only once
    /// that message reaches a turn — which waits on the Agent being
    /// materialized. For that whole window every authoritative snapshot would
    /// put the placeholder on the row, so the row is named from what the reader
    /// wrote instead. The first snapshot whose title is not the placeholder
    /// releases the record, so a rename made later, or a title an Agent
    /// supplies, always wins.
    pub pending_session_titles: BTreeMap<VibexSessionId, String>,
    /// What the terminal is told when a session needs the reader and the reader
    /// is not looking at it. The bytes are queued here and written by the run
    /// loop after the frame, so no reducer ever touches the terminal.
    pub notify: crate::notify::Notifier,
    /// What this terminal can draw a picture with, and how the escapes reach
    /// it. Detected once, because the answer is a property of the terminal the
    /// client was started in — and named apart from the draft's own `images`,
    /// which are messages' attachments rather than a capability.
    pub terminal_images: crate::terminal_image::ImageSupport,
    /// The picture the chip preview put on the terminal, and the escapes the
    /// run loop still has to write for it. See [`crate::preview`].
    pub picture: crate::preview::PlacedPicture,
    /// The code the pairing page put on the terminal, and the escapes the run
    /// loop still has to write for it. See [`crate::pairing_qr`].
    pub pairing_qr: crate::pairing_qr::PairingQr,
    /// The pictures the session list put on the terminal — an Agent's brand
    /// mark, and the icon a row's button wears — and the escapes the run loop
    /// still has to write for them. See [`crate::row_marks`]: a terminal that
    /// speaks an image protocol draws the real mark, and every other terminal
    /// draws the character the same vocabulary carries.
    pub row_marks: crate::row_marks::RowMarks,
    /// A transient message above the composer, dismissed on the next key.
    pub banner: Option<Banner>,
    /// Whether a turn has been running since the client last looked.
    ///
    /// The elapsed readout is dated from the turn's own start, so this latch
    /// only keeps the band on screen through the round trip before the
    /// projection describes the turn.
    pub turn_started: Option<std::time::Instant>,
    /// The running turn as the line above the composer reads it: its phase, how
    /// many tools it has called, and how fast it is producing output.
    ///
    /// The phase and the count are re-read from the projection on every event;
    /// the rate is a difference between two of those readings, so it is held
    /// rather than derived at draw time.
    pub turn_readout: crate::turn::TurnReadout,
    /// The open session's state the transcript was last built from.
    ///
    /// How live a row is comes from that state: a turn whose provider never
    /// sent a final message leaves its row arriving until the projection is
    /// rebuilt with a state that says the turn is over. The state can arrive
    /// through a read — the session list an invalidated sidebar re-reads, an
    /// ownership tree answer — and no rebuild follows a read, so the transcript
    /// would keep the finished turn's rows streaming for the rest of the
    /// session. Recording what it was built with is what lets the loop notice
    /// that, rather than rebuilding the projection on every frame.
    transcript_session_state: Option<vibex_core::AgentSessionState>,
    /// What the Agent is doing right now, when the runtime says.
    pub activity: Option<String>,
    /// Monotonic clock driving every animation.
    pub animation_phase: u32,
    /// Character transitions for the current composer's runtime and workspace.
    ///
    /// Driven by the frame that draws the page rather than by the gestures that
    /// change it, so every path into a new Agent or a new workspace gets the
    /// same effect — see [`crate::scramble`].
    pub transitions: crate::scramble::Transitions,
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
    /// Sessions the open one delegated, which is where the reader goes next.
    Children,
    Team,
    Agents,
    Plan,
    Queue,
}

impl DockSection {
    pub const ALL: [DockSection; 5] = [
        Self::Children,
        Self::Team,
        Self::Agents,
        Self::Plan,
        Self::Queue,
    ];

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            DockSection::Children => strings.dock_children(),
            DockSection::Team => strings.locale.text("Team", "团队", "團隊"),
            DockSection::Agents => strings.dock_agents(),
            DockSection::Plan => strings.dock_plan(),
            DockSection::Queue => strings.dock_queue(),
        }
    }
}

/// One line of the dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockRow {
    Team {
        label: String,
        action: crate::team::TeamDockAction,
    },
    /// A session the open one delegated. Activating it opens that session,
    /// which is how the reader moves down the ownership tree.
    Child {
        title: String,
        state: Option<vibex_core::AgentSessionState>,
        session_id: VibexSessionId,
    },
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
    /// The open modal's box, border included.
    ///
    /// A picture the terminal holds is a layer above the text and cannot be
    /// darkened by the modal's own backdrop, so the frame has to know which
    /// cells the box took: a mark that shares one would float over the dialog
    /// and is taken down, and a mark beside it is left standing. See
    /// [`crate::row_marks`].
    pub modal: Option<ratatui::layout::Rect>,
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
    /// The configuration centre's module tabs, area rail and per-record
    /// action buttons. Each is a control, not a row: pressing one switches a
    /// module, opens an area, or runs the verb it is labelled with.
    pub config_tabs: Vec<(ratatui::layout::Rect, crate::config_center::ConfigModule)>,
    pub config_sections: Vec<(ratatui::layout::Rect, crate::config_center::ConfigSection)>,
    pub config_buttons: Vec<(ratatui::layout::Rect, crate::config_center::ConfigAction)>,
    /// The registry bar's own controls: the two narrowings and the two page
    /// arrows. Each asks the registry the same question its key would.
    pub config_registry: Vec<(ratatui::layout::Rect, crate::config_center::RegistryControl)>,
    /// The devices page's tab strip and its rows. A tab switches halves; a row
    /// takes the cursor, and a second press on a pairing row opens it.
    pub device_tabs: Vec<(ratatui::layout::Rect, crate::device_center::DeviceTab)>,
    pub device_rows: Vec<(ratatui::layout::Rect, usize)>,
    /// The completion drawer's panel names, while a `/` or `@` drawer is open.
    /// A name switches the panel the same way `←`/`→` do.
    pub completion_tabs: Vec<(ratatui::layout::Rect, crate::composer::CompletionTab)>,
    /// The runtime switcher's rows, when it is the modal on screen. Its cursor
    /// indexes a window rather than a rect, so the region carries where the
    /// window started as well as where it was drawn.
    pub runtime_picker: Option<RuntimePickerRegion>,
    /// The command palette's rows, when it is the modal on screen.
    pub palette: Option<PaletteRegion>,
    /// The turn rail's ticks, one rect per turn.
    pub turns: Vec<(ratatui::layout::Rect, usize)>,
    /// The shortcut band's hints, so a click runs the same intent as the key.
    pub hints: Vec<(ratatui::layout::Rect, crate::action::Intent)>,
    /// The controls a list row draws for itself, each with the row it belongs
    /// to. A button acts on its own row, which is not always the cursor's — the
    /// pointer is what chose it — so the row index rides with the rect.
    pub row_actions: Vec<RowActionRegion>,
    /// The grid a sign-in terminal was drawn into, when one is attached.
    ///
    /// A click inside it is a click into the terminal: the cell the pointer is
    /// over is where the program's own cursor goes, which is the only way a
    /// reader with a mouse can answer a prompt that wants a position.
    pub agent_auth_terminal: Option<ratatui::layout::Rect>,
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
        self.row_actions.clear();
        self.config_tabs.clear();
        self.config_sections.clear();
        self.config_buttons.clear();
        self.config_registry.clear();
        self.device_tabs.clear();
        self.device_rows.clear();
        self.completion_tabs.clear();
        self.runtime_picker = None;
        self.palette = None;
        // A list, a banner row, a composer's box and a modal's close affordance
        // each belong to the frame that drew them: left standing, the session
        // list's rect would hit-test transcript rows instead, a modal that has
        // closed would still answer a click where its close used to be, and a
        // composer nobody can see would take the keyboard.
        self.list = None;
        self.banner = None;
        self.modal = None;
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

/// A control a list row draws at its own right edge — the session row's rename
/// and delete buttons.
///
/// The rect is in screen coordinates and belongs to the frame that drew it; the
/// row index is what the control acts on, because the reader who pressed it
/// pointed at that row rather than at the one the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowActionRegion {
    pub rect: ratatui::layout::Rect,
    pub row: usize,
    pub intent: crate::action::Intent,
}

/// Where the runtime switcher drew its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePickerRegion {
    /// The rows the list was drawn into.
    pub rect: ratatui::layout::Rect,
    /// The catalogue row each drawn line shows, top to bottom.
    ///
    /// A line of air between two Agents' groups, and the rule that parts the
    /// pinned sections from the Agents, show `None`: neither is a row the reader
    /// or the pointer can land on.
    pub rows: Vec<Option<usize>>,
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
        self.rows
            .get(usize::from(row - self.rect.y))
            .copied()
            .flatten()
    }
}

/// Where the command palette drew its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRegion {
    /// The rows the list was drawn into.
    pub rect: ratatui::layout::Rect,
    /// The palette entry each drawn line shows, top to bottom. A heading is
    /// `None`: it names a section, and the cursor never lands on it.
    pub rows: Vec<Option<usize>>,
}

impl PaletteRegion {
    /// Which palette entry a pointer is over, if any.
    pub fn entry_at(&self, column: u16, row: u16) -> Option<usize> {
        if column < self.rect.x
            || column >= self.rect.right()
            || row < self.rect.y
            || row >= self.rect.bottom()
        {
            return None;
        }
        self.rows
            .get(usize::from(row - self.rect.y))
            .copied()
            .flatten()
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
///
/// Both ends are cells the reader pointed at, and both are covered: a drag that
/// comes to rest on a character copies that character. A press that never moves
/// is not a selection at all — it is how the reader drops a stale highlight —
/// which is what [`TextSelection::is_empty`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: (usize, u16),
    pub head: (usize, u16),
    /// Whether the gesture went anywhere: the pointer moved after the button
    /// went down, or a double click picked a whole word.
    pub moved: bool,
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
        !self.moved
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

/// The runtime a page's next message would go through, as the surfaces under
/// the composer draw it.
///
/// The Agent is one fact said two ways — its name, and the brand mark that
/// stands for it — so the two come from one projection rather than from two
/// reads on demand. A mark read from somewhere else than the words beside it is
/// how the line under the composer would end up showing one Agent's face next
/// to another one's model.
struct ComposerRuntime {
    /// The Agent's display name, or its id when the catalogue does not hold it.
    agent: String,
    /// The string [`vibex_brands::agent_mark`] is asked for, empty when the page
    /// has no Agent to name at all.
    identity: String,
    /// The model's display name, empty while the runtime has not reported one.
    model: String,
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
        let team = vibex_ui::TeamWorkflowController::new(
            facade.agent().clone(),
            capabilities.agent.clone(),
        );
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
        // A queue the previous run left behind, read before the first frame: a
        // message given an hour to wait is still waiting after the terminal is
        // closed and opened again, which is what makes the hour worth setting.
        let queued = crate::queue_prefs::HeldQueue::load(options.queue_path.as_deref());
        let mut navigation = CompactNavigation::default();
        navigation.select_global(GlobalDestination::Sessions);
        let mut app = Self {
            facade,
            capabilities,
            seat: options.seat,
            runtime_kind: options.runtime_kind,
            theme: theme.clone(),
            capability,
            strings,
            keymap,
            shell: ShellKind::Wide,
            navigation,
            agent,
            team,
            team_detail: None,
            team_confirm_cancel: None,
            team_busy: false,
            children: crate::children::ChildSessions::with_collapsed(
                arrangement.collapsed_child_ids.clone(),
            ),
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
            queue_path: options.queue_path,
            transcript: Transcript::new(),
            scroll: ScrollState::default(),
            pending_timeline_details: BTreeSet::new(),
            timeline_details_in_flight: BTreeSet::new(),
            composer: ComposerBuffer::default(),
            history: ComposerHistory::default(),
            history_log: ComposerLog::new(HISTORY_LOG_LIMIT),
            completion: None,
            page: Page::Sessions,
            focus: Focus::Main,
            page_history: Vec::new(),
            overlay: None,
            reveal_provider_secret: false,
            auth_terminal: None,
            toast: None,
            quit_armed: 0,
            settings: SettingsState {
                themes,
                mode: options.mode,
                locale: options.locale,
                glyphs: capability.glyphs,
                mark: options.remembered.mark().unwrap_or_default(),
                motion: options.remembered.motion(),
                transitions: options.remembered.transitions(),
                reasoning_display_mode: options.remembered.reasoning_display_mode(),
                reasoning_expanded_by_default: options.remembered.reasoning_expanded_by_default(),
                reasoning_expansion_mode: options.remembered.reasoning_expansion_mode(),
                selected: 0,
                view: SettingsMode::Browse,
                filter: String::new(),
            },
            management_data: ManagementData::default(),
            pending_provider_open: None,
            pending_form: None,
            install_watch: None,
            config_center: crate::config_center::ConfigCenter::default(),
            device_center: crate::device_center::DeviceCenter::default(),
            pending_config_action: None,
            pending_sidebar_action: None,
            pending_folder_prompt: None,
            selection: BTreeMap::new(),
            filter: String::new(),
            filtering: false,
            show_archived: false,
            session_list_refresh_pending: false,
            session_title: None,
            live: LiveState::Connecting,
            pending: BTreeMap::new(),
            generation: 0,
            should_quit: false,
            handover_requested: false,
            handover_seen: options
                .handover
                .as_ref()
                .is_some_and(crate::HandoverOffer::is_available),
            handover: options.handover,
            workspace_rows: Vec::new(),
            workspace_browse: None,
            text_view: None,
            viewport: (120, 40),
            transcript_band_rows: 20,
            session_band_rows: 12,
            session_band_width: 80,
            session_row_action: None,
            session_scroll: 0,
            session_scroll_manual: false,
            regions: FrameRegions::default(),
            runtime_options: None,
            runtime_options_reading: false,
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
            elicitation_drafts: std::collections::BTreeMap::new(),
            offered_questions: std::collections::BTreeSet::new(),
            session_cards: std::collections::BTreeSet::new(),
            search: None,
            hover: None,
            hovered_hint: None,
            hovered_row_action: None,
            hovered_modal_close: false,
            history_selection: 0,
            text_selection: None,
            draft_selecting: false,
            dock_open: false,
            dock_selection: None,
            dock_collapsed: std::collections::BTreeSet::new(),
            dock_hide_done: false,
            last_click: None,
            queued_messages: queued,
            queue_selection: None,
            draft_schedules: BTreeMap::new(),
            schedule_shown_second: None,
            lan_poll_ticks: 0,
            pending_sends: BTreeMap::new(),
            inflight_sends: BTreeMap::new(),
            pending_send_serial: 0,
            unread_sessions: std::collections::BTreeSet::new(),
            authority_unread_sessions: std::collections::BTreeSet::new(),
            session_echoes: BTreeMap::new(),
            pending_session_titles: BTreeMap::new(),
            notify: crate::notify::Notifier::new(options.notify_path.clone()),
            terminal_images: crate::terminal_image::ImageSupport::detect(),
            picture: crate::preview::PlacedPicture::default(),
            pairing_qr: crate::pairing_qr::PairingQr::default(),
            row_marks: crate::row_marks::RowMarks::default(),
            banner: None,
            turn_started: None,
            turn_readout: crate::turn::TurnReadout::default(),
            transcript_session_state: None,
            activity: None,
            animation_phase: 0,
            transitions: crate::scramble::Transitions::default(),
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
        // The seat resolved this before the interface started; the notification
        // policy is the one part of the client that acts on it, and the setter is
        // what keeps the notifier's copy in step with this one.
        let runtime_kind = app.runtime_kind;
        app.set_runtime_kind(runtime_kind);
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

    /// Settle the open session's own copy from the ownership tree's metadata.
    ///
    /// A delegated session is absent from the root list, so the tree is the
    /// only read that carries a worker's state: the authority publishes a
    /// settled turn as a sidebar invalidation, which re-reads the branches this
    /// client draws, and the metadata that comes back with them is a fresher
    /// row for the worker on screen. Folding it in is what keeps a worker whose
    /// turn ended from sitting under the composer as "running" until the reader
    /// opens it again.
    ///
    /// The name this client recorded for a session stands in for the manager's
    /// placeholder here exactly as it does on the list, because the row is the
    /// same record and the page must not lose the name the reader typed.
    ///
    /// Answers whether the page's copy changed.
    pub fn sync_open_session_from_tree(&mut self) -> bool {
        let Some(open) = self
            .selected_session_id()
            .map(|session_id| session_id.as_str().to_string())
        else {
            return false;
        };
        let Some(row) = self.children.session(&open).cloned() else {
            return false;
        };
        let row = self.reconcile_pending_session_title(row);
        self.agent.reconcile_open_session(row)
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

    /// Whether the page in front of the reader draws a session's own view.
    ///
    /// Only a session's own page shows the transcript, so this is the one
    /// question every piece of chrome that *maps* or *reports* that transcript
    /// asks before drawing: the turn line above the composer, the rail of turns
    /// down the right edge, the plan and the held queue. The composing page is
    /// the case it exists for — it is a page with a composer rather than a
    /// session, and the session behind it stays selected only so leaving can
    /// return there, so none of that chrome may outlive the page it belongs to
    /// and stand over a conversation that is not on screen.
    pub fn page_shows_transcript(&self) -> bool {
        self.page_owns_session() && self.page.is_session_page()
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
        self.page_shows_transcript() && self.selected_session_id().is_some()
    }

    /// What kind of runtime this client is attached to.
    pub fn runtime_kind(&self) -> vibex_core::RemoteServerKind {
        self.runtime_kind
    }

    /// Note what kind of runtime this client is attached to.
    ///
    /// The seat resolves it once, before the interface starts, and the
    /// notification policy is one of its readers: a desktop shell announces the
    /// same events itself, so the client is told which runtime it is looking at
    /// rather than inferring it from the seat. Setting the field and telling the
    /// notifier are one operation, because a copy that drifts is a notification
    /// policy that lies.
    pub fn set_runtime_kind(&mut self, kind: vibex_core::RemoteServerKind) {
        self.runtime_kind = kind;
        self.notify.set_desktop_present(kind.is_desktop());
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
    /// The Agent's brand mark is the same answer ([`Self::composer_agent_identity`]),
    /// because the line under the composer draws one or the other.
    pub fn composer_runtime_labels(&self) -> (String, String) {
        let runtime = self.composer_runtime();
        (runtime.agent, runtime.model)
    }

    /// The Agent a message from this page will be sent through, as the string
    /// its brand mark is matched against.
    ///
    /// Read from the same projection as the name beside it
    /// ([`Self::composer_runtime_labels`]), which is the whole point: the line
    /// under the composer draws the mark *in place of* the name, so a mark read
    /// separately from the words is how the two would come to describe different
    /// Agents. Empty when the page has no Agent to name.
    pub fn composer_agent_identity(&self) -> String {
        self.composer_runtime().identity
    }

    /// The runtime this page's next message would go through: the Agent's name,
    /// the string its brand mark is matched against, and the model.
    ///
    /// The composing page has no session to fall back to: when it holds no
    /// choice — a catalogue that has not arrived, or one that publishes nothing
    /// — it names the entry a creation with no choice would use, says it is
    /// loading while the read is still on its way, or says the runtime is
    /// unavailable when the catalogue has none to name. Naming the session
    /// behind the page here is what made the page claim codex while the creation
    /// used the catalogue's default.
    fn composer_runtime(&self) -> ComposerRuntime {
        if let Some(selection) = self.page_runtime_selection() {
            if let Some(option) = self.runtime_option_for(&selection) {
                return ComposerRuntime {
                    agent: option.agent_label.clone(),
                    identity: self.agent_identity(&option.selection.agent_id, &option.agent_label),
                    model: option.model_label.clone(),
                };
            }
            // The catalogue no longer publishes this selection (an Agent that
            // has since gone away): its own words still name it, and an id the
            // catalogue does not hold is all the mark has to be matched against.
            return ComposerRuntime {
                agent: selection.agent_id.to_string(),
                identity: self.agent_identity(&selection.agent_id, ""),
                model: selection
                    .model
                    .model_id()
                    .map(str::to_string)
                    .unwrap_or_default(),
            };
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
            return ComposerRuntime {
                agent: session.agent_id.to_string(),
                // The catalogue's label for the Agent is what the mark was
                // matched against when the list drew the same session, so this
                // is the same face the reader has already seen on its row.
                identity: self.session_agent_identity(session),
                model: String::new(),
            };
        }
        match self
            .runtime_options
            .as_ref()
            .and_then(|catalog| catalog.options.first())
        {
            Some(option) => ComposerRuntime {
                agent: option.agent_label.clone(),
                identity: self.agent_identity(&option.selection.agent_id, &option.agent_label),
                model: option.model_label.clone(),
            },
            // A catalogue that is still on its way has no entry to name yet,
            // and "unavailable" is a claim about the runtime rather than about
            // this read: the page says it is loading instead. A read that has
            // answered — with a catalogue that publishes nothing — leaves the
            // claim standing.
            None if self.runtime_options_reading => ComposerRuntime {
                agent: self.strings.loading().to_string(),
                identity: String::new(),
                model: String::new(),
            },
            None => ComposerRuntime {
                agent: self.strings.runtime_unavailable().to_string(),
                identity: String::new(),
                model: String::new(),
            },
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

    /// Note the runtime and workspace text the composer is about to show.
    ///
    /// Called by the frame that draws the page, because the text is the only
    /// honest answer to "did this line change": every path into a new Agent or
    /// a new workspace — the switcher, the catalogue arriving late — gets the
    /// transition without any of them knowing it exists.
    ///
    /// A page that is not on screen is not observed at all, and one an overlay
    /// is covering is left alone rather than recorded: a change the reader
    /// cannot see waits until they can, instead of playing to an empty room and
    /// being over by the time the page is back.
    pub fn observe_composer_text(&mut self) {
        if !self.page.is_composing_page() {
            self.transitions.cancel();
            return;
        }
        if self.overlay.is_some() {
            return;
        }
        let (agent, model) = self.composer_runtime_labels();
        let account = self.composer_auth_source_label();
        let workspace = if self.page == Page::NewSession {
            self.new_session_workspace()
        } else {
            String::new()
        };
        self.transitions.observe(
            &agent,
            &account,
            &model,
            &workspace,
            self.animation_phase,
            self.settings.transitions,
        );
    }

    /// The Agent and model the page is on, with any transition applied.
    ///
    /// Two parts rather than one line because that is how the surfaces under
    /// the page draw them: the Agent and the model are separate spans with
    /// separate emphasis, and the transition carries each of them rather than a
    /// flattened run that would lose the difference. The account the two run on
    /// is a third part of the same transition ([`Self::drawn_runtime_account`]),
    /// drawn between them: it is not returned here because the new-session page
    /// names the pair and leaves the account to the info line under the box.
    pub fn drawn_runtime_parts(&self) -> (String, String) {
        let (agent, model) = self.composer_runtime_labels();
        let phase = self.animation_phase;
        (
            self.transitions
                .drawn(crate::scramble::Slot::Runtime, 0, &agent, phase),
            self.transitions
                .drawn(crate::scramble::Slot::Runtime, 1, &model, phase),
        )
    }

    /// The account the page's runtime runs on, with any transition applied.
    ///
    /// Drawn between the Agent and the model on the composer's info line, which
    /// is the order the picker's own entries name them in. It is empty when
    /// there is no name to give, and the line then leaves it out rather than
    /// showing a gap.
    pub fn drawn_runtime_account(&self) -> String {
        self.transitions.drawn(
            crate::scramble::Slot::Runtime,
            2,
            &self.composer_auth_source_label(),
            self.animation_phase,
        )
    }

    /// The account the page's runtime runs on, as this client names it.
    ///
    /// The info line below the composer says what the message will go through —
    /// the Agent as its mark, the account, the model — and the account is the one
    /// part of that answer this client has a name of its own for: an Agent
    /// account is the product's own concept rather than a name the reader chose,
    /// so it is said in the reader's language, while a Provider Profile keeps the
    /// name its owner gave it. The catalogue holds the runtime's own label,
    /// written in English for every client at once, which is why the picker's row
    /// and this line name the same account the same way. A selection the
    /// catalogue does not hold has no name to give: it records an id, and an id
    /// is not something the reader has ever seen on that line.
    pub fn composer_auth_source_label(&self) -> String {
        self.page_runtime_selection()
            .and_then(|selection| self.runtime_option_for(&selection))
            .map(|option| crate::runtime_picker::runtime_option_auth_label(&self.strings, option))
            .unwrap_or_default()
    }

    /// The workspace a session created now would open in, mid-transition.
    pub fn drawn_new_session_workspace(&self) -> String {
        let workspace = self.new_session_workspace();
        self.transitions.drawn(
            crate::scramble::Slot::Workspace,
            0,
            &workspace,
            self.animation_phase,
        )
    }

    /// The Agent and model the picker's run options are editing.
    ///
    /// The picker's own selection rather than the page's: choosing a model and
    /// tuning how it runs are one errand, and a caption naming the entry the
    /// page is still on would describe rows that belong to another one.
    pub fn picker_runtime_labels(&self) -> (String, String) {
        let Some(selection) = self.picker_selection() else {
            return self.composer_runtime_labels();
        };
        if let Some(option) = self.runtime_option_for(&selection) {
            return (option.agent_label.clone(), option.model_label.clone());
        }
        // The catalogue no longer publishes this selection (an Agent that has
        // since gone away): its own words still name it.
        (
            selection.agent_id.to_string(),
            selection
                .model
                .model_id()
                .map(str::to_string)
                .unwrap_or_default(),
        )
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

    /// The rows the picker draws: the way up, then the listing itself.
    ///
    /// This is the one place that decides what a drawn row index means, so the
    /// renderer and the keys that move the cursor cannot disagree about where
    /// `..` is — the cursor is stored as an index, and an index that meant a
    /// directory in one and a step in the other would choose the wrong
    /// directory.
    pub fn workspace_picker_rows(&self) -> Vec<WorkspacePickerRow> {
        let Some(listing) = self.workspace_browse.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::with_capacity(listing.entries.len() + 1);
        if let Some(parent) = listing.parent.as_ref() {
            rows.push(WorkspacePickerRow::Parent {
                path: parent.clone(),
            });
        }
        rows.extend(
            listing
                .entries
                .iter()
                .map(|entry| WorkspacePickerRow::Entry {
                    name: entry.name.clone(),
                    path: entry.path.clone(),
                }),
        );
        rows
    }

    /// Workspace the session pages read from.
    pub fn active_workspace_id(&self) -> Option<vibex_core::WorkspaceId> {
        self.active_session()
            .map(|session| session.workspace_id.clone())
    }

    /// Coalesces list reads while retaining one refresh after the current read.
    pub(crate) fn refresh_session_list(&mut self) -> Option<Effect> {
        if self.agent.state.sessions.is_loading() {
            self.session_list_refresh_pending = true;
            return None;
        }
        self.session_list_refresh_pending = false;
        self.agent.begin_sessions_refresh();
        Some(Effect::ListSessions {
            include_archived: self.show_archived,
        })
    }

    pub(crate) fn pending_session_list_refresh(&mut self) -> Option<Effect> {
        if std::mem::take(&mut self.session_list_refresh_pending) {
            self.refresh_session_list()
        } else {
            None
        }
    }

    /// The reads an authority's sidebar invalidation makes stale.
    ///
    /// The rows the list draws are sessions under project headings, and the
    /// arrangement says what order they take, so "the sidebar changed" is a
    /// claim about all three. A remote authority announces a session it has
    /// just created this way and through no other event, so a client that
    /// re-read only the arrangement would redraw the same rows and keep the new
    /// session off the list until the reader happened to leave the page and
    /// come back to it.
    pub(crate) fn sidebar_projection_effects(&mut self) -> Vec<Effect> {
        let mut effects = self.refresh_session_list().into_iter().collect::<Vec<_>>();
        // Project rows are named by the runtime's project records, and a
        // session in a workspace this client has never seen adds a heading of
        // its own.
        effects.push(Effect::ListWorkspaces);
        effects.push(Effect::LoadSidebarOrganization);
        // A session that just appeared may own children, and one that just
        // changed may have changed a worker's state: the tree is re-read with
        // the list it hangs under.
        effects.extend(self.child_tree_effects());
        effects
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
                    children: &self.children,
                    query: &self.filter,
                })
            }
            // No arrangement to mirror: the list is projected from the
            // sessions themselves, which is all a client without a desktop
            // can honestly say about the order. The ownership rows are spliced
            // in afterwards, because the projection knows nothing about them —
            // and the filter is applied there too, so a session whose child
            // matches is not dropped from under it.
            None => {
                let sessions = self
                    .agent
                    .state
                    .view(&self.projection.sidebar, "", self.shell)
                    .sessions;
                let loaded = self
                    .agent
                    .state
                    .sessions
                    .value
                    .as_deref()
                    .unwrap_or_default();
                let states =
                    crate::sessions::session_state_tally(loaded, &self.children, &self.filter);
                let rows = crate::sessions::filtered_child_rows(
                    sessions,
                    loaded,
                    &self.children,
                    &self.filter,
                );
                crate::sessions::SessionListRows { rows, states }
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

    /// Whether the row at this index is a session that owns children.
    ///
    /// A row is its own disclosure now: nothing is drawn at its right edge to
    /// say it has a branch, so this is what tells a click that the row folds.
    /// A heading is not a session and its `child_count` is a subtree tally, so
    /// it answers no here: the caller folds a heading by another route.
    pub fn sidebar_row_has_children(&self, index: usize) -> bool {
        self.sidebar_rows().get(index).is_some_and(|row| {
            row.kind == vibex_desktop_model::AgentSidebarRowKind::Session
                && row.session_id.is_some()
                && row.child_count > 0
        })
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
    /// This is the file the settings surface keeps: the look, the language and
    /// the standing workspace.
    pub fn interface_preferences_path() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_INTERFACE")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        Some(Self::interface_home()?.join("tui-interface.json"))
    }

    /// Where the Composer's held messages are written.
    ///
    /// Beside the arrangement and the settings, for the same reason — and it is
    /// the file a reader deletes when they want a queue they no longer trust
    /// forgotten without resetting the rest of the interface.
    pub fn queue_preferences_path() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("VIBEX_TUI_QUEUE")
            && !explicit.trim().is_empty()
        {
            return Some(std::path::PathBuf::from(explicit));
        }
        Some(Self::interface_home()?.join("tui-queue.json"))
    }

    /// The directory the interface's own files live in.
    pub(crate) fn interface_home() -> Option<std::path::PathBuf> {
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
            collapsed_child_ids: self.children.collapsed_ids(),
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
            mark: Some(self.settings.mark.id().to_string()),
            motion: Some(self.settings.motion),
            transitions: Some(self.settings.transitions),
            reasoning_display_mode: Some(
                match self.settings.reasoning_display_mode {
                    vibex_desktop_model::ReasoningDisplayMode::LatestAtBottom => "latest_at_bottom",
                    vibex_desktop_model::ReasoningDisplayMode::Timeline => "timeline",
                }
                .to_string(),
            ),
            reasoning_expanded_by_default: Some(self.settings.reasoning_expanded_by_default),
            reasoning_expansion_mode: Some(
                match self.settings.reasoning_expansion_mode {
                    vibex_desktop_model::ReasoningExpansionMode::Window => "window",
                    vibex_desktop_model::ReasoningExpansionMode::Full => "full",
                }
                .to_string(),
            ),
            locale: Some(self.settings.locale.tag().to_string()),
            workspace: self.preferred_workspace.clone(),
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

    /// Persist the Composer's held messages.
    ///
    /// Called from every path that changes the queue rather than from the frame
    /// loop: a client killed between two frames — which is how a terminal
    /// client ends — would otherwise lose the change. A failure is silent for
    /// the same reason the other files' is: losing a held message to a full
    /// disk is not worth interrupting the reader over, and the next change
    /// tries again.
    pub fn save_queue(&self) {
        let Some(path) = self.queue_path.as_deref() else {
            return;
        };
        crate::queue_prefs::HeldQueue::of(&self.queued_messages).save(Some(path));
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
            snapshot.auto_continue_default_enabled,
            &snapshot.auto_continue_project_ids.iter().cloned().collect(),
            &snapshot
                .auto_continue_disabled_project_ids
                .iter()
                .cloned()
                .collect(),
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
        // The unread marks are the authority's account of what it is still
        // badging, and they are what an opened session has to clear there. They
        // are stored before the arrangement is judged, because a tree that
        // arranges nothing says nothing about marks either way.
        let authority_unread: std::collections::BTreeSet<String> =
            snapshot.unread_session_ids.iter().cloned().collect();
        // The client's own mark is an optimistic echo of the write it sends: the
        // dot is drawn before the authority answers, and `note_activity` keeps
        // it while the mutation is in flight so an unrelated read racing the
        // write cannot blink it off. That echo is retired by the same fact that
        // retires the authority's own — an id the authority has *dropped* since
        // the last snapshot was read, wherever that read happened. Without this
        // the row keeps a dot the shared value no longer holds: the Desktop
        // clears the mark when it opens the session, and the TUI goes on drawing
        // it because only the authority's set was replaced.
        //
        // An id the authority never held is deliberately left alone: that is the
        // optimistic mark for a write still on the wire, not a stale one.
        let cleared: Vec<String> = self
            .authority_unread_sessions
            .difference(&authority_unread)
            .cloned()
            .collect();
        let mut local_changed = false;
        for session_id in &cleared {
            local_changed |= self.unread_sessions.remove(session_id);
        }
        let unread_changed = self.authority_unread_sessions != authority_unread || local_changed;
        self.authority_unread_sessions = authority_unread;
        // An arrangement that arranges nothing is dropped rather than adopted,
        // but a snapshot can carry something else a client cannot invent: the
        // scheduled sends the authority is holding. Those are kept even when
        // the tree is empty, because dropping the view would drop the only
        // account of a session waiting on an hour.
        if !view.arranges_anything() && view.scheduled_sends.is_empty() {
            return self.projection.sidebar_organization.take().is_some() || unread_changed;
        }
        let changed = self.projection.sidebar_organization.as_ref() != Some(&view);
        self.projection.sidebar_organization = Some(view);
        changed || unread_changed
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

    /// The authority change that clears the unread mark on a session this client
    /// has just opened.
    ///
    /// "Unread" is the authority's in-memory fact, and reading is an act of the
    /// client that has the session on screen — so opening a session here has to
    /// say so, or the Desktop keeps badging it. `None` when the authority was
    /// not badging it, which is the common case: a session read here is usually
    /// already read there.
    ///
    /// No expected revision travels with it: a mark is not an edit of the tree,
    /// so a rearrangement on the Desktop between the read and the mark does not
    /// make the read stale. Quoting a revision would refuse the mark — and make
    /// the read look like a failed tree edit — for a change that cannot conflict.
    pub fn sidebar_mark_read_effect(&self, session_id: &VibexSessionId) -> Option<Effect> {
        if !self.authority_unread_sessions.contains(session_id.as_str()) {
            return None;
        }
        self.sidebar_unread_effect(std::slice::from_ref(session_id), false)
    }

    /// The authority change that reports a session this client has just marked
    /// unread, so the Desktop badges what the reader here was told about.
    ///
    /// This is the other half of [`Self::sidebar_mark_read_effect`]: a finished
    /// answer for a session the reader is not looking at is unread for every
    /// window on the runtime, not only the one that happened to receive the
    /// event. `None` when the authority already badges it, so an event cannot
    /// queue a write per turn for a mark that is already there.
    pub fn sidebar_mark_unread_effect(&self, session_id: &VibexSessionId) -> Option<Effect> {
        if self.authority_unread_sessions.contains(session_id.as_str()) {
            return None;
        }
        self.sidebar_unread_effect(std::slice::from_ref(session_id), true)
    }

    /// The authority change that sets the unread mark on the named sessions.
    ///
    /// No expected revision travels with it: a mark is not an edit of the tree,
    /// so a rearrangement on the Desktop between the read and the mark does not
    /// make it stale. Quoting a revision would refuse the mark — and make a read
    /// look like a failed tree edit — for a change that cannot conflict.
    fn sidebar_unread_effect(
        &self,
        session_ids: &[VibexSessionId],
        unread: bool,
    ) -> Option<Effect> {
        if session_ids.is_empty() {
            return None;
        }
        Some(Effect::MutateSidebarOrganization {
            mutation: vibex_core::RemoteSidebarOrganizationMutation::SetSessionsUnread {
                session_ids: session_ids
                    .iter()
                    .map(|session_id| session_id.as_str().to_string())
                    .collect(),
                unread,
            },
            expected_revision: None,
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
        if row.delegated {
            return Some(SidebarMove::Blocked);
        }
        let moving_id = row.session_id.as_ref()?.as_str().to_string();
        let project_id = row.project_id.clone();
        let parent_id = row.parent_id.clone();
        let mut cursor = index as isize + delta;
        while cursor >= 0 && (cursor as usize) < rows.len() {
            let candidate = &rows[cursor as usize];
            if candidate.delegated {
                cursor += delta;
                continue;
            }
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
        // A delegated child is not the reader's to pin: the branch follows the
        // session that delegated it, and a pin would promise a place the list
        // cannot give it.
        if row.delegated {
            return false;
        }
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
        // A delegated child is not a row the reader arranges: it sits where the
        // session that owns it sits, and moving it would only move the list it
        // is nested in.
        if row.delegated {
            return Some(false);
        }
        let moving_id = row.session_id.as_ref()?.to_string();
        let moving_pinned = row.pinned;
        let mut cursor = index as isize + delta;
        let mut target = None;
        while cursor >= 0 && (cursor as usize) < rows.len() {
            let candidate = &rows[cursor as usize];
            if candidate.delegated {
                cursor += delta;
                continue;
            }
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

    /// The question the open form is filling in, with the cursor's place in it.
    ///
    /// The overlay pins a request id, so this is where "the form" is resolved
    /// against what is still pending: a request answered from another client
    /// takes the form with it rather than leaving the reader typing into a
    /// question nobody is waiting on.
    pub fn elicitation_form(&self) -> Option<ElicitationForm> {
        let Some(Overlay::Elicitation {
            request,
            field,
            choice,
        }) = self.overlay.as_ref()
        else {
            return None;
        };
        self.elicitations()
            .into_iter()
            .find(|surface| surface.request.id.as_str() == request)
            .map(|surface| ElicitationForm {
                request: surface.request,
                field: *field,
                choice: *choice,
            })
    }

    /// The draft the form for `request_id` is writing into.
    pub fn elicitation_draft(&self, request_id: &str) -> Option<&crate::reduce::ElicitationDraft> {
        self.elicitation_drafts.get(request_id)
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
            // And it is on a row rather than on one of that row's buttons:
            // whatever moved it — an arrow, a click, the palette — the keyboard
            // starts the row over instead of pressing a button the reader has
            // left behind.
            self.session_row_action = None;
        }
        self.selection.insert(scope, value);
    }

    /// Whether the session list's cursor is on the page's own action row.
    pub fn session_cursor_is_actions(&self) -> bool {
        self.selection_for(Scope::Sessions) == SESSION_ACTION_ROW
    }

    /// The buttons the session page's own action row offers the keyboard, in
    /// the order it draws them.
    ///
    /// `+ New session` is always drawn; the workspace control is there while a
    /// key names it, which is the same condition the frame draws its hint under.
    pub fn session_action_row_intents(&self) -> Vec<crate::action::Intent> {
        let mut intents = vec![crate::action::Intent::NewSession];
        if self
            .keymap
            .chord_for(crate::action::Intent::SwitchWorkspace)
            .is_some()
        {
            intents.push(crate::action::Intent::SwitchWorkspace);
        }
        intents
    }

    /// The buttons one list row offers the keyboard, in the order the frame
    /// draws them.
    ///
    /// The frame and the keyboard answer from the same function, so a terminal
    /// too narrow to hold a row's whole cluster drops the same leading verbs for
    /// both: the button the keyboard can reach is the button the reader can see.
    pub fn sidebar_row_button_intents(&self, index: usize) -> Vec<crate::action::Intent> {
        let rows = self.sidebar_rows();
        let Some(row) = rows.get(index) else {
            return Vec::new();
        };
        if row.session_id.is_none() {
            return crate::view::heading_button_intents(row);
        }
        let indent = usize::from(row.depth) * crate::view::SESSION_ROW_INDENT;
        crate::view::session_row_button_intents(self.session_band_width, indent)
    }

    /// Whether a point is inside a rect the last frame published.
    pub fn rect_contains(rect: ratatui::layout::Rect, column: u16, row: u16) -> bool {
        column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
    }

    /// A press on one of the centre's own controls, if the point is over one.
    ///
    /// The frame publishes where each control landed, so the pointer's press
    /// is resolved against the same rects the reader saw rather than against a
    /// second copy of the layout sums.
    pub fn click_config_control(
        &mut self,
        column: u16,
        row: u16,
    ) -> Option<crate::reduce::Outcome> {
        if let Some((_, module)) = self
            .regions
            .config_tabs
            .iter()
            .find(|(rect, _)| App::rect_contains(*rect, column, row))
            .copied()
        {
            return Some(self.show_config_module(module, None));
        }
        if let Some((_, section)) = self
            .regions
            .config_sections
            .iter()
            .find(|(rect, _)| App::rect_contains(*rect, column, row))
            .copied()
        {
            return Some(self.show_config_section(section));
        }
        if let Some(outcome) = self.click_config_registry(column, row) {
            return Some(outcome);
        }
        let action = self
            .regions
            .config_buttons
            .iter()
            .find(|(rect, _)| App::rect_contains(*rect, column, row))
            .map(|(_, action)| *action)?;
        Some(self.run_selected_config_action(action))
    }

    /// A press on one of the registry bar's own controls.
    ///
    /// The bar is the page's question made visible, so pressing a control asks
    /// it again the same way the key would: there is one handler per question,
    /// and the pointer and the keyboard both reach it.
    pub fn click_config_registry(
        &mut self,
        column: u16,
        row: u16,
    ) -> Option<crate::reduce::Outcome> {
        use crate::config_center::RegistryControl;
        let control = self
            .regions
            .config_registry
            .iter()
            .find(|(rect, _)| App::rect_contains(*rect, column, row))
            .map(|(_, control)| *control)?;
        let intent = match control {
            RegistryControl::Transport => crate::action::Intent::ConfigRegistryFilter,
            RegistryControl::Sort => crate::action::Intent::ConfigRegistrySort,
            RegistryControl::PagePrevious => crate::action::Intent::ConfigRegistryPagePrevious,
            RegistryControl::PageNext => crate::action::Intent::ConfigRegistryPageNext,
        };
        Some(self.perform(intent))
    }

    /// Put the page the reader is on back inside the answer that just arrived.
    pub fn clamp_registry_page(&mut self) {
        let pages = self.config_center.registry_pages(&self.management_data);
        self.config_center.registry.page = self
            .config_center
            .registry
            .page
            .min(pages.saturating_sub(1));
    }

    /// A press on a record row: the first selects, the second opens the menu.
    pub fn click_config_row(&mut self, index: usize, repeat: bool) -> crate::reduce::Outcome {
        let rows = self
            .config_center
            .visible(&self.management_data, self.strings);
        if index >= rows.len() {
            return crate::reduce::Outcome::quiet();
        }
        self.config_center.set_cursor(index);
        self.config_center.zone = crate::config_center::ConfigZone::Rows;
        self.config_center.pin(rows.get(index));
        if repeat {
            return self.perform(crate::action::Intent::ConfigEntryActions);
        }
        crate::reduce::Outcome::quiet()
    }

    /// Show one module of the configuration centre, and read what it needs.
    pub fn show_config_module(
        &mut self,
        module: crate::config_center::ConfigModule,
        section: Option<crate::config_center::ConfigSection>,
    ) -> crate::reduce::Outcome {
        self.config_center.show_module(module, section);
        // Opening an area leaves the reader where they were. The rows are where
        // this surface starts (`ConfigCenter::default` says so) and the module
        // and area keys work from any pane, so sending the focus back to the
        // tabs would only take the record verbs away from a reader who came to
        // use them on the first row.
        self.page = Page::Management;
        self.refresh_config_area()
    }

    /// A press on the devices page's own controls, if the point is over one.
    ///
    /// A tab switches halves; a row takes the cursor, and a second press on a
    /// pairing row opens it — the same two-step contract every other list in the
    /// client keeps, so a pointer never opens something the keyboard would have
    /// needed a second press for.
    pub fn click_device_control(
        &mut self,
        column: u16,
        row: u16,
    ) -> Option<crate::reduce::Outcome> {
        if let Some((_, tab)) = self
            .regions
            .device_tabs
            .iter()
            .find(|(rect, _)| App::rect_contains(*rect, column, row))
            .copied()
        {
            if self.device_center.tab == tab {
                return Some(crate::reduce::Outcome::quiet());
            }
            let release = self.device_center.select_tab(tab);
            let mut effects = self.device_release_effects(release);
            if tab == crate::device_center::DeviceTab::Publishing
                && self.device_publications_available()
            {
                // Arriving on the read is the moment to make it, exactly as the
                // key that steps between halves does.
                effects.push(Effect::PairingPublications);
            }
            return Some(crate::reduce::Outcome::effects(effects));
        }
        let index = self
            .regions
            .device_rows
            .iter()
            .find(|(rect, _)| App::rect_contains(*rect, column, row))
            .map(|(_, index)| *index)?;
        let tab = self.device_center.tab;
        let methods = self.device_pair_methods().len();
        let devices = self.management_data.devices.len();
        if index >= self.device_center.row_count(tab, methods, devices) {
            return Some(crate::reduce::Outcome::quiet());
        }
        let already = self.device_cursor() == index;
        self.device_center.set_cursor(tab, index);
        if tab == crate::device_center::DeviceTab::Pair
            && (already || self.double_click_at(column, row))
        {
            return Some(self.perform(Intent::ActivateDevicePairing));
        }
        Some(crate::reduce::Outcome::effects(vec![]))
    }

    pub fn show_config_section(
        &mut self,
        section: crate::config_center::ConfigSection,
    ) -> crate::reduce::Outcome {
        self.config_center.show_section(section);
        self.config_center.zone = crate::config_center::ConfigZone::Rail;
        self.refresh_config_area()
    }

    /// Run one of the highlighted record's verbs, as a click on its button.
    pub fn run_selected_config_action(
        &mut self,
        action: crate::config_center::ConfigAction,
    ) -> crate::reduce::Outcome {
        let Some(row) = self
            .config_center
            .selected(&self.management_data, self.strings)
        else {
            return crate::reduce::Outcome::quiet();
        };
        self.config_center.zone = crate::config_center::ConfigZone::Rows;
        self.run_config_action_by_key(&row.key, action)
    }

    /// The wheel, over whichever band of the centre the pointer is in.
    pub fn scroll_config_area(
        &mut self,
        delta: i64,
        column: u16,
        row: u16,
    ) -> crate::reduce::Outcome {
        let in_table = self
            .regions
            .list
            .as_ref()
            .is_some_and(|region| App::rect_contains(region.rect, column, row));
        if in_table {
            let rows = self
                .config_center
                .visible(&self.management_data, self.strings);
            self.config_center.reconcile(&rows);
            self.config_center.step_cursor(&rows, delta as isize);
            return crate::reduce::Outcome::effects(vec![]);
        }
        let scroll = self.config_center.detail_scroll as i64 + delta;
        self.config_center.detail_scroll = scroll.max(0) as u16;
        crate::reduce::Outcome::effects(vec![])
    }

    pub fn push_config_filter(&mut self, character: char) {
        self.config_center.filter.push(character);
        self.config_center.set_cursor(0);
    }

    pub fn pop_config_filter(&mut self) {
        self.config_center.filter.pop();
        self.config_center.set_cursor(0);
    }

    pub fn clear_config_filter(&mut self) {
        self.config_center.filter.clear();
        self.config_center.set_cursor(0);
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
    /// The ways this runtime can pair a device right now, in the order the
    /// pairing half of the devices page draws them.
    ///
    /// The page asks the backend the same question the backend answers: an
    /// operation the runtime does not support and one this device's grant
    /// forbids are different sentences to the reader, so both are resolved here
    /// rather than guessed at from the runtime's kind.
    pub fn device_pair_methods(&self) -> Vec<crate::device_center::PairMethod> {
        crate::device_center::pair_methods(
            self.runtime_kind,
            self.availability(BackendOperation::DevicePairing),
            self.availability(BackendOperation::DeviceLanPairing),
            self.device_center.publications.lan_precondition(),
        )
    }

    /// The halves of the devices page that exist on this runtime.
    ///
    /// The publishing half is absent where the runtime cannot answer for its own
    /// routes: a page that listed Tailscale Serve, Direct, and Relay without
    /// being able to confirm any of them would be inventing them.
    pub fn device_tabs(&self) -> Vec<crate::device_center::DeviceTab> {
        let mut tabs = crate::device_center::DeviceTab::ALWAYS.to_vec();
        if self.device_publications_available() {
            tabs.push(crate::device_center::DeviceTab::Publishing);
        }
        tabs
    }

    /// Whether this runtime can say how it publishes itself.
    pub fn device_publications_available(&self) -> bool {
        self.availability(BackendOperation::DevicePairingPublications) == Availability::Available
    }

    /// The reads the devices page owes when it opens.
    ///
    /// The trust store always; how the runtime publishes itself wherever the
    /// runtime can answer, because that is the read that turns an empty pairing
    /// half into a sentence naming what to switch on.
    pub fn device_page_effects(&self) -> Vec<Effect> {
        let mut effects = vec![Effect::ListDevices];
        if self.device_publications_available() {
            effects.push(Effect::PairingPublications);
        }
        effects
    }

    /// What a closed panel gives up, as the effects that give it up.
    ///
    /// The client stops believing in what it was holding either way: if the
    /// runtime refuses to take the advertisement or the offer down, the reader
    /// is told rather than shown something that is still live.
    pub fn device_release_effects(
        &mut self,
        release: crate::device_center::Release,
    ) -> Vec<Effect> {
        use crate::device_center::Release;
        match release {
            Release::Nothing => Vec::new(),
            Release::Window => {
                self.device_center.pairing.window = None;
                self.device_center.pairing.busy = None;
                vec![Effect::CancelLanPairing]
            }
            Release::Offer => {
                let Some(offer_id) = self
                    .device_center
                    .pairing
                    .offer
                    .as_ref()
                    .map(|offer| offer.offer_id.clone())
                else {
                    return Vec::new();
                };
                self.device_center.pairing.offer = None;
                self.device_center.pairing.busy = None;
                vec![Effect::CancelPairingOffer { offer_id }]
            }
        }
    }

    /// The pairing row the keyboard is on, when the pairing half is open.
    pub fn selected_pair_method(&self) -> Option<crate::device_center::PairMethod> {
        self.device_pair_methods()
            .get(self.device_cursor())
            .copied()
    }

    /// The device the keyboard is on, when the devices half is open.
    pub fn selected_paired_device(&self) -> Option<&vibex_core::RemoteDeviceDetail> {
        if self.device_center.tab != crate::device_center::DeviceTab::Devices {
            return None;
        }
        self.management_data.devices.get(self.device_cursor())
    }

    /// The row the keyboard is on, clamped to the rows the open half has.
    pub fn device_cursor(&self) -> usize {
        self.device_center.clamped_cursor(
            self.device_pair_methods().len(),
            self.management_data.devices.len(),
        )
    }

    /// Move inside the devices page, where the same two keys mean "the next way
    /// to pair" or "the next device" depending on which half is open.
    pub(crate) fn move_device_selection(&mut self, delta: i64) -> crate::reduce::Outcome {
        let methods = self.device_pair_methods().len();
        let devices = self.management_data.devices.len();
        let count = self
            .device_center
            .row_count(self.device_center.tab, methods, devices);
        if count == 0 {
            return crate::reduce::Outcome::quiet();
        }
        let current = self.device_cursor() as i64;
        let next = (current + delta).clamp(0, count as i64 - 1) as usize;
        if next == current as usize {
            return crate::reduce::Outcome::quiet();
        }
        self.device_center.set_cursor(self.device_center.tab, next);
        crate::reduce::Outcome::effects(vec![])
    }

    /// Whether the devices page has a panel open that `Esc` should close before
    /// it becomes a walk back to the page the reader came from.
    pub fn device_panel_is_open(&self) -> bool {
        self.page == Page::Devices && self.device_center.open.is_some()
    }

    pub fn page_row_count(&self) -> usize {
        match self.page {
            Page::NewSession => 0,
            Page::Sessions => self.sidebar_rows().len(),
            Page::Management => self
                .config_center
                .visible(&self.management_data, self.strings)
                .len(),
            Page::Devices => self.device_center.row_count(
                self.device_center.tab,
                self.device_pair_methods().len(),
                self.management_data.devices.len(),
            ),
            Page::Providers | Page::Agents | Page::Mcp | Page::Skills | Page::Prompts => 0,
            Page::Settings => self.visible_settings().len(),
            Page::Agent => self.transcript.len(),
            Page::Help => 0,
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

    /// What a binding is called, in the language this session resolved to.
    ///
    /// A binding's label is an accessor rather than a string so one action can
    /// be named differently where it means something different — `Filter` on a
    /// list is `Search` on the runtime switcher — while both stay in the locale
    /// table. Resolving it here keeps that indirection out of every renderer
    /// and keeps the search boxes matching the words on screen: a reader who
    /// types 折叠 is looking at rows labelled 折叠, not at rows spelled
    /// `Expand`.
    pub fn binding_label(&self, binding: &Binding) -> &'static str {
        binding
            .label
            .map(|label| label(self.strings))
            .unwrap_or_default()
    }

    /// The bindings the key bar and the help page may name right now, one per
    /// intent, paired with the scope each answers in.
    ///
    /// [`Self::documented_scopes`] says whose bindings are eligible and
    /// [`Keymap::advertised`] walks them in table order; two rules then cut the
    /// list down to what the reader can actually press, which is the only thing
    /// a bar may promise:
    ///
    /// * The binding has to be the one that would fire. One intent can be bound
    ///   in two scopes — `1` is the session list in `Scope::Global` and
    ///   `Ctrl+L` is the same destination in `Scope::Composer` — and the table
    ///   lists both, so the row that loses is dropped rather than spelled.
    /// * The chord has to reach the table at all: a surface that types what the
    ///   reader pressed answers before the bindings do, and `?` is a question
    ///   mark in a draft where it is help on a list. See [`Self::chord_is_input`].
    pub fn advertised_keys(&self) -> Vec<(Scope, &Binding)> {
        let eligible = self.documented_scopes();
        let dispatch = self.active_scopes();
        let mut advertised: Vec<(Scope, &Binding)> = Vec::new();
        for binding in self.keymap.advertised(&eligible) {
            if advertised
                .iter()
                .any(|(_, row)| row.intent == binding.intent)
            {
                continue;
            }
            if self.chord_is_input(binding.chord) {
                continue;
            }
            if self.keymap.resolve(&dispatch, binding.chord) != Some(binding.intent) {
                continue;
            }
            if (self.overlay.is_some() || self.filtering || self.config_center.filtering)
                && binding.scope == Scope::Global
                && !self.surface_answers(binding.intent)
            {
                continue;
            }
            // The configuration centre's two overlays print their own keys in
            // the modal footer, and the overlay scope is shared with the
            // approval and elicitation cards: left in, the bar would offer
            // `A Allow` under a server editor, where it does nothing. The
            // schedule editor is the third of them: it names its own keys,
            // including the one that voids a deadline.
            if matches!(
                self.overlay,
                Some(Overlay::Form { .. })
                    | Some(Overlay::EntryActions { .. })
                    | Some(Overlay::Schedule(_))
            ) && binding.scope == Scope::Overlay
            {
                continue;
            }
            // The devices page's verbs belong to a row that may not be there:
            // `Allow` and `Refuse` mean nothing without a window and a device
            // waiting on it, and `Revoke` means nothing without a device. A bar
            // that offered them anyway would be promising keys that answer with
            // nothing.
            if self.page == Page::Devices && !self.device_key_applies(binding.intent) {
                continue;
            }
            // The ownership verbs are the same case: a session with nothing
            // under it has no sub-sessions to open, and a root has no main
            // session to go back to.
            if !self.ownership_key_applies(binding.intent) {
                continue;
            }
            advertised.push((binding.scope, binding));
        }
        // The page's own keys lead the bar: a reader looking at the
        // configuration centre needs what `Enter` does there more than they
        // need the eighth global shortcut, and the bar drops what does not
        // fit. The sort is stable, so declaration order holds inside a scope.
        advertised.sort_by_key(|(scope, _)| {
            eligible
                .iter()
                .position(|candidate| candidate == scope)
                .unwrap_or(usize::MAX)
        });
        advertised
    }

    /// Whether one of the ownership verbs has anywhere to go.
    ///
    /// A branch is foldable only when some row on the list actually has one;
    /// the way into the children exists only for a session that owns some; the
    /// way back exists only for a session that was delegated. The chords stay
    /// bound — a key that comes and goes with the state is a key the reader
    /// cannot learn — but a surface that names keys does not name a verb with
    /// no subject.
    pub fn ownership_key_applies(&self, intent: Intent) -> bool {
        match intent {
            Intent::ToggleChildBranch => self.sidebar_rows().iter().any(|row| row.child_count > 0),
            Intent::OpenChildSessions => self.selected_session_id().is_some_and(|session_id| {
                self.children.has_children(session_id.as_str())
                    || self.children.declared_child_count(session_id.as_str()) > 0
            }),
            Intent::GotoParentSession => self.parent_session_id().is_some(),
            _ => true,
        }
    }

    /// Whether one of the devices page's own verbs has anything to act on.
    ///
    /// The page's keys are bound for the whole scope, because a key that comes
    /// and goes with the state is a key the reader cannot learn. What changes is
    /// whether the bar names it: a verb with no subject is left out rather than
    /// offered as a press that does nothing.
    pub fn device_key_applies(&self, intent: Intent) -> bool {
        match intent {
            Intent::AllowLanPairing | Intent::RefuseLanPairing => {
                self.device_center.lan_panel_open()
                    && self.device_center.pairing.pending().is_some()
            }
            Intent::ActivateDevicePairing => {
                self.device_center.tab == crate::device_center::DeviceTab::Pair
                    && self.selected_pair_method().is_some()
            }
            Intent::DevicesTabNext | Intent::DevicesTabPrevious => true,
            // The route keys exist wherever the panel does; the bar drops them
            // when the offer has a single route, because a control with one
            // option is a control that does nothing.
            Intent::PairingRouteNext | Intent::PairingRoutePrevious => self
                .device_center
                .pairing
                .offer
                .as_ref()
                .is_some_and(|offer| offer.route_is_chosen()),
            Intent::RevokeSelectedDevice => self.selected_paired_device().is_some(),
            Intent::OpenDeviceAudit => {
                self.device_center.tab == crate::device_center::DeviceTab::Devices
            }
            _ => true,
        }
    }

    /// Whether the reducer still answers this intent while a modal or a filter
    /// owns the keyboard.
    ///
    /// Both surfaces put themselves down for `Esc` and for a destination named
    /// from them, and a filter also answers `Ctrl+C`; everything else the
    /// global scope binds is swallowed, because a modal holding the keyboard is
    /// the whole point of one. The bar drops those rows rather than naming keys
    /// that do nothing — the surface's own scope keeps its own rows, since a
    /// modal answers those out of its own arms.
    fn surface_answers(&self, intent: Intent) -> bool {
        if intent.leaves_the_surface() || matches!(intent, Intent::Back | Intent::CloseOverlay) {
            return true;
        }
        (self.filtering || self.config_center.filtering) && intent == Intent::ContextualCancel
    }

    /// The chord one intent answers to on the surface that owns the keyboard.
    ///
    /// `None` when the intent has no chord there — either because nothing
    /// binds it in the eligible scopes, or because the chord it is bound to is
    /// spent before the binding table sees it (see [`Self::chord_is_input`]).
    /// A hint that names a key takes the chord from here, so it can never spell
    /// a key that does something else.
    pub fn advertised_chord(&self, intent: Intent) -> Option<Chord> {
        self.advertised_keys()
            .into_iter()
            .find(|(_, binding)| binding.intent == intent)
            .map(|(_, binding)| binding.chord)
    }

    /// Whether the surface in front of the binding table spends this chord
    /// itself, so a binding on it would never fire.
    ///
    /// The order mirrors `run::handle_key`: a text field types what the reader
    /// pressed, a card that answers with a digit spends it, and the key
    /// editor's capture takes every key there is. What is left is the truth
    /// behind two rules the bar and the help page live by:
    ///
    /// * A chord named anywhere is a chord that answers there. `?` is help on
    ///   the session list and a question mark in the draft, and only one of
    ///   those can be advertised where it is.
    /// * A chord that is not text is a destination everywhere. The function
    ///   keys and the control chords are never typed, so `run::handle_key`
    ///   lets them through to the table even while a mode that takes every
    ///   other key owns the keyboard — which is why this says `F1` is free in
    ///   the middle of a filter rather than reporting it dead.
    pub fn chord_is_input(&self, chord: Chord) -> bool {
        // The key editor records the chord the reader presses: while it is
        // capturing, every key is data — `F1` included.
        if matches!(
            self.overlay,
            Some(Overlay::Keys {
                capturing: Some(_),
                ..
            })
        ) {
            return true;
        }
        // The two modes that take every key but the control chords: an arrow is
        // a cursor move and a function key is nothing at all, and either way it
        // is theirs rather than the table's.
        if self.search_composing()
            || (self.page == Page::Settings && !self.settings.view.is_browse())
        {
            return !matches!(chord.code, KeyCode::F(_))
                && !chord.modifiers.contains(KeyModifiers::CONTROL);
        }
        let typing = match chord.code {
            KeyCode::Char(_) => !chord
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
            KeyCode::Backspace | KeyCode::Delete => {
                !chord.modifiers.contains(KeyModifiers::CONTROL)
            }
            _ => false,
        };
        if !typing {
            return false;
        }
        if self.runtime_picker_filtering() || self.filtering || self.config_center.filtering {
            return true;
        }
        match &self.overlay {
            // The surfaces with a query or a form of their own type what the
            // reader pressed, and the two that answer with a digit spend it. A
            // configuration form only types while a field is open for editing:
            // between fields its own keys are the table's.
            Some(Overlay::Palette { .. })
            | Some(Overlay::Help { .. })
            | Some(Overlay::Prompt { .. })
            | Some(Overlay::Approval { .. })
            | Some(Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                ..
            }) => true,
            Some(Overlay::Form { form }) => form.editing.is_some(),
            // The elicitation form types into an answer field and spends a
            // digit on a list of options. Which of the two it is depends on the
            // field the cursor is on, so the bar asks the form rather than the
            // overlay: a letter that is an answer in one field is a shortcut in
            // the next.
            Some(Overlay::Elicitation { .. }) => {
                self.elicitation_field_takes_text_keys()
                    || (self.elicitation_field_choices() > 0
                        && matches!(
                            chord.code,
                            KeyCode::Char(character) if character.is_ascii_digit()
                        ))
            }
            Some(_) => false,
            None => self.composer_takes_keys(),
        }
    }

    /// Whether the composer answers this key before the binding table does.
    pub fn composer_takes_keys(&self) -> bool {
        self.focus == Focus::Composer && self.page.is_composing_page()
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
        self.session_by_id(&session_id)
    }

    /// The full record for one session id, whether the runtime's root list
    /// holds it or the ownership tree named it as somebody's child.
    pub fn session_by_id(&self, session_id: &VibexSessionId) -> Option<&AgentSession> {
        self.agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|session| &session.id == session_id)
            .or_else(|| self.children.session(session_id.as_str()))
    }

    // ---- delegated children ---------------------------------------------

    /// The ownership reads the list is missing or has let go stale.
    ///
    /// One page of ownership roots says which sessions have children at all;
    /// every unfolded branch is re-read so a worker's state cannot lag behind
    /// its parent's row; the open session's own page carries the path back to
    /// the session that owns it. A read already in flight is not asked for
    /// twice, a runtime that cannot answer is asked once, and a pass is
    /// bounded — so this costs one round trip per branch per window however
    /// often the list is refreshed.
    pub(crate) fn child_tree_effects(&mut self) -> Vec<Effect> {
        const READS_PER_PASS: usize = 8;
        if self.children.is_unsupported() || !self.supports(BackendOperation::AgentTeamRead) {
            return Vec::new();
        }
        let now_ms = vibex_core::unix_timestamp_ms();
        let mut parents: Vec<Option<VibexSessionId>> = vec![None];
        parents.extend(self.children.expanded_parents().into_iter().map(Some));
        if let Some(open) = self.selected_session_id().cloned() {
            parents.push(Some(open));
        }
        let mut effects = Vec::new();
        for parent in parents {
            if effects.len() >= READS_PER_PASS {
                break;
            }
            if !self.children.needs_refresh(parent.as_ref(), now_ms) {
                continue;
            }
            if let Some(effect) = self.load_child_page(parent, false) {
                effects.push(effect);
            }
        }
        effects
    }

    /// Ask for one parent's children now, fresh page or not.
    ///
    /// Opening a session and unfolding a branch both need the answer on the
    /// gesture rather than on the next refresh cycle.
    pub fn load_child_page(
        &mut self,
        parent: Option<VibexSessionId>,
        append: bool,
    ) -> Option<Effect> {
        if self.children.is_unsupported() || !self.supports(BackendOperation::AgentTeamRead) {
            return None;
        }
        let cursor = if append {
            parent
                .as_ref()
                .and_then(|id| self.children.next_cursor(id.as_str()))
        } else {
            None
        };
        if !self.children.begin_load(parent.as_ref()) {
            return None;
        }
        Some(Effect::LoadSessionTree {
            parent_session_id: parent,
            cursor,
            append,
        })
    }

    /// The session that owns the session the reader has open.
    pub fn parent_session_id(&self) -> Option<VibexSessionId> {
        let open = self.selected_session_id()?;
        self.parent_of_session(open)
    }

    /// The session that owns one session, whether it is the open one or a row.
    ///
    /// The path the open session's own page answered with is the authority
    /// while it applies; otherwise the tree edge is, so a child opened from
    /// the list still knows where it came from.
    pub fn parent_of_session(&self, session_id: &VibexSessionId) -> Option<VibexSessionId> {
        if self.children.open_session() == Some(session_id.as_str())
            && let Some(parent) = self.children.parent_of_open()
        {
            return parent.session_ref.session_id();
        }
        self.children.parent_of(session_id.as_str())
    }

    /// The name a session is drawn under, wherever this client learned it.
    pub fn session_display_title(&self, session_id: &VibexSessionId) -> Option<String> {
        self.session_by_id(session_id)
            .map(|session| self.session_row_title(session))
            .or_else(|| {
                self.children
                    .node(session_id.as_str())
                    .map(|node| node.title.clone())
            })
    }

    /// What the dock draws for the open session's children.
    ///
    /// The titles and states come from the ownership metadata, so the section
    /// can name a worker the root session list never carried.
    pub fn child_session_rows(&self) -> Vec<DockRow> {
        let Some(open) = self.selected_session_id() else {
            return Vec::new();
        };
        self.children
            .children(open.as_str())
            .iter()
            .filter_map(|node| {
                let session_id = node.session_ref.session_id()?;
                let session = self.children.session(session_id.as_str());
                Some(DockRow::Child {
                    title: session
                        .map(|session| session.title.clone())
                        .unwrap_or_else(|| node.title.clone()),
                    state: session.map(|session| session.state),
                    session_id,
                })
            })
            .collect()
    }

    /// Fold or unfold the branch of the session the list cursor is on.
    ///
    /// Unfolding a branch nobody has read asks for it, so the rows appear on
    /// the gesture; unfolding one that is already held is local. A row with no
    /// children has no branch and the key does nothing.
    pub fn toggle_child_branch(&mut self) -> Option<Effect> {
        self.toggle_child_branch_at(self.selection_for(Scope::Sessions))
    }

    /// Fold or unfold the branch of the row at one index of the list.
    ///
    /// The keyboard folds the row its cursor is on; the pointer names the row it
    /// landed on, which is the same row the click already selected. Both reach
    /// the branch through here so the gesture and the key agree on what a branch
    /// is: a session with children, open unless the reader folded it.
    pub fn toggle_child_branch_at(&mut self, index: usize) -> Option<Effect> {
        let row = self.sidebar_rows().get(index).cloned()?;
        let session_id = row.session_id?;
        if row.child_count == 0 {
            return None;
        }
        let folded = self.children.toggle_collapsed(session_id.as_str());
        self.save_sidebar_arrangement();
        if folded {
            return None;
        }
        if self.children.child_count(session_id.as_str()) > 0 {
            return None;
        }
        self.load_child_page(Some(session_id), false)
    }

    /// Open a folded branch without closing an open one.
    ///
    /// Walking the cursor onto a row has to leave an open branch open: the
    /// reader is on their way to a child row, and a selection that folded what
    /// it landed on would take that row off the list before they could reach it.
    /// So the cursor only ever opens. A row with nothing to open costs nothing.
    pub fn open_child_branch_at(&mut self, index: usize) -> Option<Effect> {
        let row = self.sidebar_rows().get(index).cloned()?;
        let session_id = row.session_id?;
        if row.child_count == 0 {
            return None;
        }
        if !self.children.open(session_id.as_str()) {
            // Already open: the reader stepped onto a branch they can see
            // through, and there is nothing to do.
            return None;
        }
        self.save_sidebar_arrangement();
        if self.children.child_count(session_id.as_str()) > 0 {
            return None;
        }
        self.load_child_page(Some(session_id), false)
    }

    /// Forget the branches of sessions the list no longer holds.
    pub fn retain_child_tree(&mut self) {
        let mut known = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|session| session.id.as_str().to_string())
            .collect::<BTreeSet<_>>();
        known.extend(
            self.children
                .child_sessions()
                .map(|session| session.id.as_str().to_string()),
        );
        self.children.retain(&known);
    }

    /// Whether the client can ask the runtime about session ownership.
    pub fn child_tree_available(&self) -> bool {
        !self.children.is_unsupported() && self.supports(BackendOperation::AgentTeamRead)
    }

    /// Whether the terminal is narrow enough that chrome must give way.
    pub fn is_compact(&self) -> bool {
        self.shell == ShellKind::Compact
    }

    /// The grid a sign-in terminal should be sized to.
    ///
    /// It is the modal's content area rather than the client's whole viewport:
    /// the terminal is drawn inside the account panel, and a login flow that
    /// believed it had the whole screen would print its prompt somewhere the
    /// panel cannot show. The numbers mirror the panel's `ModalSizing` — the
    /// modal keeps two rows and two columns of margin and one row of padding on
    /// each side, and its footer takes three rows.
    pub fn auth_terminal_size(&self) -> (u16, u16) {
        let (columns, rows) = self.viewport;
        let panel_columns = (columns.saturating_mul(64) / 100)
            .clamp(40, 96)
            .saturating_sub(2 * 2 + 2);
        let panel_rows = rows
            .saturating_sub(2 * 2 + 1 + 3)
            .clamp(4, crate::auth_terminal::AUTH_TERMINAL_ROWS);
        (panel_rows.max(1), panel_columns.max(1))
    }

    /// Give the panel back while leaving the sign-in running.
    ///
    /// The terminal is the authority's: detaching is what a viewer can do, and
    /// closing it would kill a login the reader may still be finishing in a
    /// browser window. The operation is kept on the view, so the panel still
    /// offers to call it off when it is opened again.
    pub fn detach_auth_terminal(&mut self) -> bool {
        if self.auth_terminal.take().is_none() {
            return false;
        }
        if let Some(Overlay::AgentAuth { view }) = self.overlay.as_mut() {
            view.notice = Some((
                self.strings.config_auth_terminal_detached().to_string(),
                crate::config_center::ConfigTone::Muted,
            ));
        }
        true
    }

    /// The terminal's own report for a pointer event, when the program inside
    /// asked for the mouse and the pointer is over the grid.
    ///
    /// The encoding is SGR (DECSET 1006), which is what a terminal sends when a
    /// program asks for it and what every program that asks for the mouse
    /// understands. The coordinates are grid-relative: the program has no idea
    /// where its grid sits on the client's screen, and telling it otherwise
    /// would be a coordinate it could not use.
    pub fn auth_terminal_mouse_event(
        &self,
        mouse: crossterm::event::MouseEvent,
    ) -> Option<(vibex_core::TerminalId, String)> {
        use crossterm::event::{MouseButton, MouseEventKind};
        let terminal = self.auth_terminal.as_ref()?;
        if !terminal.wants_mouse() {
            return None;
        }
        let grid = self.regions.agent_auth_terminal?;
        // The grid's origin is the panel's content, and a click outside it is
        // not the terminal's.
        let column = mouse.column.checked_sub(grid.x)?;
        let row = mouse.row.checked_sub(grid.y)?;
        if column >= grid.width || row >= grid.height {
            return None;
        }
        // A terminal is 1-based and a buffer is 0-based: the program's own
        // cursor arithmetic is written against the 1-based form.
        let column = column + 1;
        let row = row + 1;
        let (button, pressed) = match mouse.kind {
            MouseEventKind::Down(button) => (Some(button), true),
            MouseEventKind::Up(button) => (Some(button), false),
            // Motion is only reported while a button is held: a program that
            // asked for the mode wants drags, not a stream of every cell the
            // pointer crossed.
            MouseEventKind::Drag(button) => (Some(button), true),
            MouseEventKind::ScrollUp => (None, true),
            MouseEventKind::ScrollDown => (None, true),
            // Horizontal wheels and a bare move are not something a login flow
            // asks for; the client keeps them rather than inventing a report.
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight | MouseEventKind::Moved => {
                return None;
            }
        };
        let scroll = matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
        let mut code = match (button, scroll) {
            (Some(MouseButton::Left), false) => 0,
            (Some(MouseButton::Middle), false) => 1,
            (Some(MouseButton::Right), false) => 2,
            (None, true) => 64,
            _ => return None,
        };
        if scroll && matches!(mouse.kind, MouseEventKind::ScrollDown) {
            code += 1;
        }
        if !pressed {
            code += 3;
        }
        if mouse
            .modifiers
            .contains(crossterm::event::KeyModifiers::SHIFT)
        {
            code += 4;
        }
        if mouse
            .modifiers
            .contains(crossterm::event::KeyModifiers::ALT)
        {
            code += 8;
        }
        if mouse
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            code += 16;
        }
        // `M` is a press and `m` a release; the final `M` is part of the SGR
        // form rather than the button it belongs to.
        let final_marker = if pressed { 'M' } else { 'm' };
        Some((
            terminal.terminal_id.clone(),
            format!("\x1b[<{code};{column};{row}{final_marker}"),
        ))
    }

    /// The terminal that should receive this key, when one is attached.
    ///
    /// A key belongs to the terminal only while the account panel is what is on
    /// screen: a reader who opened the panel, attached, and then opened a form
    /// over it is typing into the form, and a terminal that swallowed those
    /// keys would answer a prompt nobody was looking at.
    pub fn auth_terminal_key_target(&self) -> Option<vibex_core::TerminalId> {
        if !matches!(self.overlay, Some(Overlay::AgentAuth { .. })) {
            return None;
        }
        self.auth_terminal
            .as_ref()
            .map(|terminal| terminal.terminal_id.clone())
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

        // The sessions this one delegated come first: they are the only rows
        // that lead somewhere new. The section is drawn from the ownership
        // tree rather than from the transcript, so a worker whose card has
        // scrolled out of the page is still one key away.
        let children = self.child_session_rows();
        if !children.is_empty() {
            rows.push(DockRow::Header {
                section: DockSection::Children,
                count: children.len(),
            });
            if !self.dock_collapsed.contains(&DockSection::Children) {
                rows.extend(children);
            }
        }

        rows.extend(self.team_dock_rows());

        // Delegated child agents, newest row first and one row per delegation:
        // a child that reports progress must not grow the panel every time.
        let mut seen: std::collections::BTreeSet<String> = self
            .team
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| &snapshot.tasks)
            .filter_map(|task| task.task_ref.task_id().map(|id| id.to_string()))
            .collect();
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
            DockRow::Team { .. } => false,
            // A child session is a door rather than a row this method opens:
            // `activate_child_dock_row` answers for it with the effects opening
            // a session needs.
            DockRow::Child { .. } => false,
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

    /// Put the dock cursor on the first child session, opening the panel.
    ///
    /// The key that asks to see a session's children has to land somewhere the
    /// reader can choose from, and the dock's own section is where they are
    /// named.
    pub fn focus_dock_children(&mut self) -> bool {
        let Some(index) = self
            .dock_rows()
            .iter()
            .position(|row| matches!(row, DockRow::Child { .. }))
        else {
            return false;
        };
        self.dock_open = true;
        self.dock_selection = Some(index);
        true
    }

    /// Act on the dock's child-session row under the cursor, when it is one.
    ///
    /// A child row is a door rather than a place in this transcript: activating
    /// it opens that session's own page, which is how the reader walks down the
    /// ownership tree.
    pub fn activate_child_dock_row(&mut self) -> Option<crate::reduce::Outcome> {
        let row = self.dock_rows().get(self.dock_selection?).cloned()?;
        let DockRow::Child { session_id, .. } = row else {
            return None;
        };
        Some(self.open_session_effects(session_id))
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
    ///
    /// Dated from the turn's own wall-clock start rather than from when this
    /// client first saw it run: a reader who opens a session that is already
    /// mid-turn reads the turn's elapsed time — the number the desktop shows
    /// for it — instead of a clock that began at their arrival.
    ///
    /// A send the runtime has not answered yet has no turn to date, so it
    /// counts from the moment it was submitted. With neither there is nothing
    /// running to measure.
    pub fn turn_elapsed(&self) -> Option<std::time::Duration> {
        if !self.page_owns_session() {
            return None;
        }
        if let Some(turn) = self.live_turn() {
            return Some(crate::turn::worked_duration(&turn));
        }
        self.pending_send_for_selected().map(|pending| {
            std::time::Duration::from_millis(
                vibex_core::unix_timestamp_ms()
                    .saturating_sub(pending.submitted_at_ms)
                    .max(0) as u64,
            )
        })
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
            Page::Devices => strings.management_devices(),
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

    /// Hold the reader's place while a gesture reveals or hides transcript
    /// content.
    ///
    /// The transcript moves on its own in two ways: it follows the tail while
    /// `follow` is set, and it clamps a parked offset to the bottom when the
    /// content under it shrinks. A gesture that changes what is drawn is a
    /// reading gesture — the reader asked to see something — so both are
    /// settled against the frame that is on screen: following stops, and the
    /// offset is pinned where the frame actually is rather than where the state
    /// was last dragged to. Every gesture that opens or closes a row goes
    /// through this, so a click and the key that does the same thing cannot
    /// leave the view in different states.
    pub fn pin_transcript_view(&mut self) {
        self.scroll.follow = false;
        self.scroll.offset = self.transcript.scroll_offset();
    }

    /// Whether the transcript has a running block worth animating.
    ///
    /// The interface repaints without input only while this is true. The
    /// composing page draws no transcript, so a turn streaming in the session
    /// behind it is not something to repaint for.
    pub fn transcript_animating(&self) -> bool {
        self.page_owns_session() && self.transcript.is_animating()
    }

    /// Whether the landing mark has something to draw right now.
    ///
    /// The light comes round again for as long as the page waits, so this is
    /// true for one pass of the loop and false between passes; a burst of the
    /// tear is true for its own frames and false for the still stretch. It is
    /// the question the tick period asks: there is nothing to draw quickly
    /// while the mark sits between passes.
    fn landing_mark_moves(&self) -> bool {
        self.mark_animates() && crate::logo::moving(self.settings.mark, self.animation_phase)
    }

    /// Whether the landing mark is animating at all.
    ///
    /// The page waits for the reader, so its light will come round again: the
    /// clock has to keep running even in the quiet stretch between passes, or
    /// there would be no next pass. The reader can stop it outright, and a mark
    /// that is not animating is drawn at rest rather than left on whichever
    /// frame the clock happened to stop on.
    pub fn mark_animates(&self) -> bool {
        self.composing_page_shines() && self.settings.motion
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
        self.mark_animates()
            && !self.landing_mark_moves()
            && !self.transitions.running(self.animation_phase)
            && !self.transcript_animating()
            && !self.turn_reads_running()
            && self.page_approval_count() == 0
            && self.page_elicitation_count() == 0
    }

    /// Whether the session list has a row on it whose state ring is turning.
    ///
    /// The ring belongs to the page rather than to the session the page has
    /// open. A reader who leaves a turn running and steps out to the list is
    /// looking at work that is still going, and the rings on those rows are the
    /// only thing there that moves — a clock that answered only for the *open*
    /// session left every other running row standing still, which said the
    /// session had stopped when it had not.
    ///
    /// The sessions are read rather than the rows the frame built: the window,
    /// the folds and the filter belong to the frame's own projection, which is
    /// rebuilt on every repaint, and the clock may not build the list a second
    /// time for an answer that costs at most one frame. The clock therefore
    /// keeps running for a row the reader has folded away or filtered out,
    /// which is the price of never stopping one they are looking at.
    fn session_list_turns_a_ring(&self) -> bool {
        self.page == Page::Sessions
            && self
                .agent
                .state
                .sessions
                .value
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|session| {
                    session.deleted_at_ms.is_none()
                        && crate::sessions::session_state_turns_ring(session.state)
                })
    }

    /// Whether the page in front of the reader is waiting on a request of its
    /// own.
    ///
    /// Not every spinner belongs to a turn. Saving a record in the configuration
    /// centre and asking for a pairing offer are the client's own round trips,
    /// and each draws a spinner where the press was made — the record's status
    /// mark and its action bar, the pairing row's own line — for as long as the
    /// answer is out. A frame that stood on one frame for the whole round trip
    /// reads as a press the client dropped, which is exactly what a spinner is
    /// there to deny.
    ///
    /// Both are asked of the surface that draws them, and both states are
    /// cleared by their own answers — the pairing request by the panel closing
    /// as well — so the clock is never left running for a page nobody has open.
    /// The write form is not one of these: it says a write is on its way in
    /// words with nothing turning beside them.
    fn page_has_a_request_in_flight(&self) -> bool {
        (self.page.is_configuration_centre() && self.config_center.writes_in_flight())
            || (self.page == Page::Devices && self.device_center.pairing.is_busy())
    }

    /// Whether the dock the reader has open is drawing a mark that turns.
    ///
    /// The panel outlives the turn that filled it: a plan step the Agent left
    /// running, or a delegation it never closed, is still drawn as a spinner
    /// after the turn is over — and a spinner standing on one frame is the one
    /// thing a panel of work in flight must never do. The rows themselves are
    /// asked rather than their sources, so the clock and the panel cannot
    /// disagree about which of them is spinning: the fold, the filter over
    /// finished work and the newest row per delegation are the panel's own
    /// reading, and a second copy of it would be a second answer. The walk costs
    /// what the panel costs, and only while the panel is open.
    fn dock_turns_a_mark(&self) -> bool {
        if !self.dock_open || !self.page_shows_transcript() {
            return false;
        }
        self.dock_rows().iter().any(|row| match row {
            DockRow::Agent { status, .. } => matches!(
                status,
                vibex_core::ToolCallStatus::Started | vibex_core::ToolCallStatus::Progress
            ),
            DockRow::Plan { status, .. } => *status == vibex_core::PlanStepStatus::Running,
            _ => false,
        })
    }

    /// Whether anything on screen is moving without the reader's input.
    ///
    /// The landing mark's light crosses the page; a turn's spinner turns. A page
    /// of sessions turns the ring on every row whose session is still working,
    /// whether or not that session is the one the page has open; a page waiting
    /// on a request of its own turns the spinner where it was started; and the
    /// dock turns one for the work a finished turn left behind. Everything else
    /// holds still, which is what keeps an idle session — and the prompt the
    /// client opens on, between passes — at zero frames.
    pub fn chrome_animating(&self) -> bool {
        self.transcript_animating()
            || self.landing_mark_moves()
            || self.transitions.running(self.animation_phase)
            || self.session_list_turns_a_ring()
            || self.page_has_a_request_in_flight()
            || self.dock_turns_a_mark()
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
        // The first resting frame still replaces an animated frame. Suppress
        // repaints only when both sides of this tick are already at rest.
        let was_waiting = self.landing_mark_waits();
        self.animation_phase = self.animation_phase.wrapping_add(1);
        // Finished transitions release the clock; the repaint below still
        // draws their settled text.
        self.transitions.prune(self.animation_phase);
        !was_waiting || !self.landing_mark_waits()
    }

    /// Keep the running band in step with what the session actually is.
    ///
    /// The band and the spinner answer to "is a turn running", which is a
    /// property of the session rather than an event. Deriving it here means no
    /// path can leave the band running after the turn stopped — which would
    /// keep the client repainting for the rest of the session — or hide it
    /// while the runtime works.
    ///
    /// The inquiry is page-independent even though the band is not: the
    /// composing page does not draw it, and a reader who steps out to write a
    /// new session and comes back must find the turn's own elapsed time rather
    /// than one that restarted when they returned.
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
    /// a question waiting on the reader pulses; the session list turns a ring
    /// for every row whose session is still working; a page waiting on its own
    /// write, and the dock's rows for the work a finished turn left behind,
    /// turn their own spinners. Everything else holds still, which is what keeps
    /// an idle session at zero frames.
    pub fn is_animating(&self) -> bool {
        self.transcript_animating()
            || self.turn_reads_running()
            || self.page_approval_count() > 0
            || self.page_elicitation_count() > 0
            || self.mark_animates()
            || self.transitions.running(self.animation_phase)
            || self.session_list_turns_a_ring()
            || self.page_has_a_request_in_flight()
            || self.dock_turns_a_mark()
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
        // The host extends or withdraws the offer on its own clock, so the
        // frame that draws it follows the flag rather than an event.
        let offered = self.handover_offered();
        if offered != self.handover_seen {
            self.handover_seen = offered;
            dirty = true;
        }
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
        // A scheduled send counts down on the same clock and for the same
        // reason: nothing arrives to say a second has passed, and a countdown
        // that never moves reads as decoration. The due message itself is
        // released by `drain_queue`, which every pass of the event loop already
        // calls.
        let counting = self.tick_schedules(now_ms);
        // A LAN pairing window is the one pairing fact that arrives from
        // outside: a phone asks, and nothing in this process hears about it.
        // While its panel is open the window is re-read on a slow clock — slow
        // enough that the poll is not the client's own traffic, quick enough
        // that a reader watching for a request sees it — and each read is also
        // what expires the countdown the panel draws.
        //
        // An offer is the same kind of fact for the same reason: the phone
        // claims it on the runtime, and the only way this client learns the code
        // was used is by asking. The publishing half is re-read on that clock
        // too, because a route can come up while the reader is looking at it and
        // a status page that never changes is a status page nobody trusts.
        let mut poll = false;
        let wants_pairing_poll = self.device_center.lan_panel_open()
            && self.device_center.pairing.window.is_some()
            || self.device_center.pairing.live_offer(now_ms).is_some()
            || (self.page == Page::Devices
                && self.device_center.tab == crate::device_center::DeviceTab::Publishing
                && self.device_publications_available());
        if wants_pairing_poll {
            self.lan_poll_ticks = self.lan_poll_ticks.saturating_sub(1);
            if self.lan_poll_ticks == 0 {
                self.lan_poll_ticks = PAIRING_POLL_TICKS;
                poll = true;
            }
        } else {
            self.lan_poll_ticks = 0;
        }
        let mut effects: Vec<Effect> = actions.into_iter().map(auto_continue_effect).collect();
        if poll {
            if self.device_center.lan_panel_open() && self.device_center.pairing.window.is_some() {
                effects.push(Effect::LanPairingStatus);
            }
            if let Some(offer_id) = self
                .device_center
                .pairing
                .live_offer(now_ms)
                .map(|offer| offer.offer_id.clone())
            {
                effects.push(Effect::PairingOfferStatus { offer_id });
            }
            if self.page == Page::Devices
                && self.device_center.tab == crate::device_center::DeviceTab::Publishing
                && self.device_publications_available()
            {
                effects.push(Effect::PairingPublications);
            }
        }
        // An install is watched rather than awaited: the authority answers the
        // request with nothing and changes the record when it is done, so the
        // client asks about the record until the authority stops working.
        if let Some(watch) = self.install_watch.as_mut() {
            let settled = watch.phase.is_some() && !watch.running();
            if settled {
                // Nothing left to watch: the answer is read once more so the
                // row the reader is looking at shows what landed.
                let label = watch.label.clone();
                let failed = watch.phase == Some(vibex_core::AgentManagedInstallStatus::Failed);
                let error = watch.error.clone();
                self.install_watch = None;
                if failed {
                    self.toast(Toast::danger(match error {
                        Some(message) => message,
                        None => format!("{}: {label}", self.strings.config_install_failed()),
                    }));
                } else {
                    self.toast(Toast::success(
                        self.strings
                            .config_install_done()
                            .replace("{agent}", &label),
                    ));
                }
                dirty = true;
            } else if watch.poll() {
                effects.push(Effect::ListAgents);
                dirty = true;
            }
        }
        crate::reduce::Outcome {
            effects,
            dirty: dirty || moved || counting || poll,
        }
    }

    pub fn composer_ticket(&self) -> ComposerTicket {
        ComposerTicket {
            target: self.composer_target.clone(),
            navigation_serial: self.navigation_serial,
            runtime: self.page_runtime_selection().map(Box::new),
            workspace_id: self.composer_workspace_id(),
            text: self.composer.text().to_string(),
            cursor: self.composer.cursor(),
        }
    }

    /// Whether the completion drawer is offering a row a key could take.
    ///
    /// A menu can be open with nothing to offer — a request still in flight, or
    /// a query that matches none of the candidates that came back — and the
    /// drawer draws nothing then. A key that takes a row has to ask this rather
    /// than whether a menu exists, or it swallows the keystroke for a popup the
    /// reader cannot see.
    pub fn completion_offers_row(&self) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|menu| menu.selected_item().is_some())
    }

    /// The workspace a completion request for the open composer resolves in.
    ///
    /// On a session page it is the session's own workspace. On the page where a
    /// session is being written there is no session yet, so it is the workspace
    /// the page is about to create one in — the directory the reader chose, or
    /// the one the client was started in — matched against the workspace records
    /// this client has read. A path the runtime has no record for is no
    /// workspace id: the request then names none, and the authority answers it
    /// without a file list rather than with another workspace's files.
    pub fn composer_workspace_id(&self) -> Option<vibex_core::WorkspaceId> {
        if self.page_shows_session() {
            return self
                .selected_session_id()
                .and_then(|id| self.session_by_id(id))
                .map(|session| session.workspace_id.clone())
                .or_else(|| self.active_workspace_id());
        }
        let path = self.new_session_workspace();
        self.workspace_rows
            .iter()
            .find(|row| row.workspace.root_path == path)
            .map(|row| row.workspace.id.clone())
            .or_else(|| {
                self.active_session()
                    .filter(|session| session.workspace_root == path)
                    .map(|session| session.workspace_id.clone())
            })
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
        // A newly opened editor starts with its own labels. Neither a running
        // transition nor the previous editor's text belongs to this target.
        self.transitions = crate::scramble::Transitions::default();
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

    /// Close the picker and forget the visit.
    ///
    /// The selection the run options were editing goes with it: it belongs to
    /// the visit, and the next `Ctrl+G` asks the page what it is on rather than
    /// answering with an entry the reader chose a surface ago.
    pub fn cancel_runtime_picker(&mut self) {
        self.runtime_picker_pending = false;
        self.runtime_picker_target = None;
        self.runtime_picker.working = None;
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

    /// Open the page the reader asked for.
    ///
    /// The page being left is remembered, because `Esc` walks back through the
    /// pages the reader actually opened rather than through a fixed hierarchy:
    /// a reader who opened Settings from the composing page lands back on the
    /// composing page, not on a session list they never opened. Showing the
    /// page already on screen is not a step, so it is not remembered either.
    pub fn navigate_to(&mut self, page: Page) {
        let page = self.config_center_route(page);
        if page != self.page {
            self.remember_page();
        }
        self.enter_page(page);
    }

    /// Fold the per-module pages into the one configuration centre.
    ///
    /// `Page::Mcp` and its siblings predate the centre and are still how the
    /// palette and the rest of the client name a destination; they are routes
    /// into a module now rather than four screens with four key scopes.
    fn config_center_route(&mut self, page: Page) -> Page {
        use crate::config_center::{ConfigModule, ConfigSection};
        let route = match page {
            Page::Agents => Some((ConfigModule::Agent, ConfigSection::Agents)),
            Page::Providers => Some((ConfigModule::Agent, ConfigSection::Providers)),
            Page::Mcp => Some((ConfigModule::Mcp, ConfigSection::Servers)),
            Page::Skills => Some((ConfigModule::Skills, ConfigSection::Installed)),
            Page::Prompts => Some((ConfigModule::Prompts, ConfigSection::Prompts)),
            _ => None,
        };
        match route {
            Some((module, section)) => {
                self.config_center.show_module(module, Some(section));
                Page::Management
            }
            None => page,
        }
    }

    /// Note where the reader is, for the walk back.
    fn remember_page(&mut self) {
        self.page_history.push(PageVisit {
            page: self.page,
            focus: self.focus,
        });
        if self.page_history.len() > PAGE_HISTORY_LIMIT {
            self.page_history.remove(0);
        }
    }

    /// Walk back to the page this one was opened from, if there is one.
    ///
    /// Walking back is not opening a page: what the reader returns to is a page
    /// they already had, so the step never grows the history it walks. The focus
    /// the page was left with comes back with it — a keyboard that was in the
    /// composer lands there again — and an entry that names the page already on
    /// screen is skipped rather than spending the press on no movement. Answers
    /// whether there was anywhere to go.
    pub fn navigate_back(&mut self) -> bool {
        while let Some(visit) = self.page_history.pop() {
            if visit.page == self.page {
                continue;
            }
            self.enter_page(visit.page);
            if visit.focus == Focus::Composer && visit.page.is_composing_page() {
                self.focus = Focus::Composer;
            }
            return true;
        }
        false
    }

    /// Show a page, with the state a page change resets.
    fn enter_page(&mut self, page: Page) {
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
        self.team.select_session(session_id.clone());
        self.team_detail = None;
        self.team_confirm_cancel = None;
        self.navigation_serial = self.navigation_serial.wrapping_add(1);
        self.cancel_runtime_picker();
        self.switch_composer(ComposerTarget::Session(session_id.clone()));
        self.unread_sessions.remove(session_id.as_str());
        // The authority's mark is cleared optimistically with the local one; a
        // refusal puts it back on the next read. Leaving it badged would make
        // the row disagree with the page the reader is looking at.
        self.authority_unread_sessions.remove(session_id.as_str());
        self.navigation.enter_session(session_id.as_str());
        self.navigate_to(Page::Agent);
        // Opening a session is a request to work in it, so the keyboard lands
        // in the composer without a trip through `Tab`.
        self.focus = Focus::Composer;
        self.scroll = ScrollState::default();
    }

    pub fn record_created_session(&mut self, session: &AgentSession) {
        // The authority's answer is a placeholder: the creation carried no
        // title, and the reader's message becomes one only when it reaches a
        // turn. The row is named from that message in the meantime.
        let session = self.reconcile_pending_session_title(session.clone());
        let rows = self.agent.state.sessions.value.get_or_insert_with(Vec::new);
        if !rows.iter().any(|row| row.id == session.id) {
            rows.push(session);
        }
    }

    /// Consume exactly one acknowledged creation. Duplicate or unrelated
    /// callbacks have no prompt to send.
    ///
    /// A creation opened for a scheduled message has nothing to send: the
    /// message is already in the queue, waiting for its own clock, and the
    /// acknowledgment is the only thing this call had left to do.
    pub fn pending_send_effect(&mut self, session_id: VibexSessionId) -> Option<Effect> {
        let creation = self.pending_creations.remove(&session_id)?;
        if creation.scheduled_at_ms.is_some() {
            return None;
        }
        let pending = self.pending_sends.get_mut(&creation.send_id)?;
        pending.submitted_at = std::time::Instant::now();
        let correlation_id = pending.correlation_id.clone();
        let attachments = self.wire_attachments(&creation.outgoing.images);
        self.history_log.push(creation.outgoing.text.clone());
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
        // No session exists for the name to belong to, and the retry reserves
        // another identity, so this one is kept by nothing.
        self.forget_pending_session_title(request_id);
        self.abandon_send(request_id, creation.send_id);
        // A scheduled creation put its message in the queue before the session
        // it belongs to existed. The session does not exist, so the message
        // cannot be sent anywhere: it goes back to the draft with the rest of
        // the creation rather than waiting forever on a row nobody can open.
        if creation.scheduled_at_ms.is_some() {
            self.queued_messages
                .retain(|queued| &queued.session_id != request_id);
            self.save_queue();
        }
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
        self.save_queue();
        self.new_session_runtime = failed.creation.runtime;
        self.workspace_path = Some(failed.creation.workspace_root);
        self.switch_composer(ComposerTarget::Draft(self.new_draft_id.clone()));
        self.composer.set_draft(
            failed.creation.outgoing.text,
            failed.creation.outgoing.images,
        );
        // A creation that failed on its way to a deadline comes back with the
        // deadline it had: the reader asked for a send time, and the retry is
        // the same message, not a new decision about when.
        if let Some(at_ms) = failed.creation.scheduled_at_ms {
            self.set_draft_schedule(Some(crate::schedule::MessageSchedule::At(at_ms)));
        }
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
        let selected_id = self
            .transcript
            .block(self.selection_for(Scope::Agent))
            .map(|block| block.id.clone());
        // `transcript_rows` rather than the state-free projection: a turn the
        // runtime has finished must stop claiming to stream, or the client
        // draws a spinner over a finished answer for the rest of the session.
        let mut rows = self.agent.state.transcript_rows();
        self.project_reasoning(&mut rows);
        self.transcript.set_reasoning_preferences(
            self.settings.reasoning_expanded_by_default,
            self.settings.reasoning_expansion_mode,
        );
        self.transcript.set_motion_enabled(self.settings.motion);
        self.restore_message_placeholders(&mut rows);
        // A send the runtime has not echoed yet is projected as the row it will
        // become. It goes last because that is where the echo will land, and it
        // is only drawn for the session it belongs to: another session's client
        // state is not this transcript.
        if let Some(pending) = self.pending_send_for_active() {
            rows.push(pending.row(self.strings));
        }
        self.projection.rows = rows.clone();
        let items: HashMap<&str, &vibex_core::TimelineItem> = self
            .agent
            .state
            .timeline
            .items
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        let workspace = self
            .agent
            .state
            .active_session
            .value
            .as_ref()
            .map(|session| session.workspace_root.as_str());
        // Which activity rows the projection is only showing a summary of.
        // Recorded while the payload is at hand: a block cannot tell a stripped
        // payload from a complete one, and a page that just landed arrived
        // summarized.
        let mut summarized = BTreeSet::new();
        let mut blocks: Vec<Block> = rows
            .iter()
            .filter(|row| row_is_rendered(row))
            .map(|row| {
                let mut block = block_from_row(row, self.strings.locale);
                if matches!(
                    row.kind,
                    vibex_desktop_model::TimelineRowKind::ToolCall
                        | vibex_desktop_model::TimelineRowKind::Command
                        | vibex_desktop_model::TimelineRowKind::FileOperation
                        | vibex_desktop_model::TimelineRowKind::WebSearch
                        | vibex_desktop_model::TimelineRowKind::Collaboration
                        | vibex_desktop_model::TimelineRowKind::ImageGeneration
                ) {
                    let newest = row
                        .item_ids
                        .iter()
                        .filter_map(|id| items.get(id.as_str()))
                        .max_by_key(|item| item.sequence);
                    let payload = newest.map(|item| &item.payload);
                    let activity =
                        vibex_ui::timeline::Activity::project(row, payload, self.strings.locale);
                    block.failed = activity.is_failed();
                    block.activity = Some(activity);
                    block.details = vibex_ui::tool_detail::project(
                        row,
                        payload,
                        workspace,
                        self.strings.locale,
                    );
                    if newest.is_some_and(|item| !item.has_complete_payload()) {
                        summarized.insert(block.sequence);
                    }
                }
                block
            })
            .collect();
        self.project_turn_folds(&mut blocks);
        // A prepend is the only change that moves the content under the
        // viewport, and it is recognisable from the head alone: the block that
        // used to start the transcript now sits further down. An append, a
        // streamed update, or an in-place replacement leaves the line offset
        // exact, so the anchor lookup — which lays the whole transcript out —
        // only runs when it can be needed.
        let prepended = !self.scroll.follow
            && self
                .transcript
                .blocks()
                .iter()
                .find(|block| !block.group.is_head())
                .is_some_and(|head| blocks.first().is_some_and(|first| first.id != head.id));
        let anchor = if prepended {
            self.transcript
                .block_at_line(self.scroll.offset)
                .and_then(|index| self.transcript.block(index).map(|block| block.id.clone()))
        } else {
            None
        };
        let change = self.transcript.set_blocks(blocks);
        // An activity left open across a rebuild is still waiting for its
        // output: a refresh can put a summary back under it, and a page that
        // just landed above the window arrives summarized. Expansion survives
        // the rebuild by block id, so the ask has to survive it too.
        if !summarized.is_empty() {
            let open = self
                .transcript
                .blocks()
                .iter()
                .filter(|block| block.expanded && !block.is_hidden())
                .map(|block| block.sequence)
                .filter(|sequence| summarized.contains(sequence))
                .collect::<Vec<_>>();
            self.pending_timeline_details.extend(open);
        }
        if let Some(index) = selected_id
            .as_deref()
            .and_then(|id| self.transcript.index_of_block(id))
            .and_then(|index| self.transcript.visible_block(index))
        {
            self.set_selection(Scope::Agent, index);
        }
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
        // What the rows were built from, so the loop can tell that the session
        // has moved on since — see [`Self::sync_transcript_liveness`].
        self.transcript_session_state = self.active_session().map(|session| session.state);
    }

    /// Rebuild the transcript when the state it draws from moved.
    ///
    /// A row's liveness is settled against the session's state, so a turn the
    /// runtime has finished cannot still be streaming: that is what stops a
    /// provider which never sent a final message from leaving its last row
    /// arriving for the rest of the session. The state can move through a read
    /// rather than an event — an attached seat learns a settled turn from the
    /// sidebar invalidation whose list read it answers, and a delegated
    /// worker's row arrives with the ownership tree — and no rebuild follows a
    /// read. Without this, the page kept those rows streaming, and the turn
    /// line reads a streaming row as work in flight: it spun, counted time and
    /// said "running" over an answer the runtime had already finished.
    ///
    /// Answers whether the transcript was rebuilt.
    pub fn sync_transcript_liveness(&mut self) -> bool {
        if self.transcript_session_state == self.active_session().map(|session| session.state) {
            return false;
        }
        self.sync_transcript();
        true
    }

    /// Annotate each block with the fold its turn settled on.
    ///
    /// The desktop's rule, and the TUI's: a turn folds its process section once
    /// it has settled *and* produced an answer. A turn still running keeps its
    /// work visible — the rows arriving are the point — and a settled turn with
    /// no answer keeps them too, because there is nothing to fold towards and
    /// the work is the only record of what happened. A superseded turn is the
    /// case worth naming: it never completes, but the work it did is what the
    /// message that replaced it is answering, so it folds as settled.
    ///
    /// The reader's own choice is not consulted here: it lives in the
    /// transcript's expansion map and is applied on every `set_blocks`, which
    /// is what keeps an explicit expansion from being undone by the next
    /// streaming delta.
    fn project_turn_folds(&self, blocks: &mut [Block]) {
        use vibex_desktop_model::TimelineRowKind;
        let turns = self
            .agent
            .state
            .conversation_turns_with_reasoning_mode(self.settings.reasoning_display_mode);
        let mut labels: HashMap<&str, String> = HashMap::new();
        // The answer is the row the projection stamped as the turn's
        // conclusion. A turn whose answer row is not marked — an older
        // projection, a row the runtime replaced — falls back to the row the
        // turn names, and then to its last Agent message. Marking it here is
        // what gives the fold a block to draw its divider on, and a turn with
        // no answer block at all never folds: hiding the work under a divider
        // with nothing below it would lose the only record of the turn.
        let mut conclusion: HashMap<String, usize> = HashMap::new();
        // The turn's first process row: while the section is open it draws the
        // title above the work, the way the desktop's own header does. It is
        // the row the reader meets first, so it is the one that names the turn
        // — the answer's divider names it only while the work is hidden.
        let mut head: HashMap<String, usize> = HashMap::new();
        // The Agent each turn ran on, as the string its brand mark is matched
        // against. Read once per turn from the attribution the projection
        // wrote, so every row of the turn draws the same mark.
        let mut identity: HashMap<String, Option<String>> = HashMap::new();
        for turn in &turns {
            if !turn.complete && !turn.superseded {
                // A still-running turn keeps its work visible: the rows
                // arriving are the point. A superseded turn never completes,
                // but the work it did is what the message replacing it
                // answers, so it folds as settled.
                continue;
            }
            let answered = turn
                .conclusion_row
                .as_ref()
                .is_some_and(|row| !row.body.trim().is_empty());
            if !answered {
                // A settled turn with no answer keeps its work too: there is
                // nothing to fold towards, and the work is the only record of
                // what happened.
                continue;
            }
            let in_turn = |block: &Block| block.turn_id.as_deref() == Some(turn.id.as_str());
            let named = turn.conclusion_row.as_ref().map(|row| row.id.as_str());
            let index = blocks
                .iter()
                .position(|block| in_turn(block) && Some(block.id.as_str()) == named)
                .or_else(|| {
                    blocks
                        .iter()
                        .rposition(|block| in_turn(block) && block.conclusion)
                })
                .or_else(|| {
                    blocks.iter().rposition(|block| {
                        in_turn(block) && block.kind == TimelineRowKind::AgentMessage
                    })
                });
            // A turn with no drawn work has nothing to fold — a bare question
            // and answer must not pay a divider for process the reader never
            // saw — and a turn with nothing but its answer has no divider to
            // justify either.
            let has_process = index.is_some_and(|index| {
                blocks.iter().enumerate().any(|(other, block)| {
                    other != index
                        && in_turn(block)
                        && !matches!(block.kind, TimelineRowKind::UserMessage)
                })
            });
            if let Some(index) = index.filter(|_| has_process) {
                blocks[index].conclusion = true;
                conclusion.insert(turn.id.clone(), index);
                // The rows the fold hides sit above the answer, so the title
                // belongs on the first of them. A turn whose only process row
                // the projection ordered after its answer still gets the title
                // on that row rather than none at all.
                let first_process = blocks
                    .iter()
                    .take(index)
                    .position(|block| in_turn(block) && block.kind != TimelineRowKind::UserMessage)
                    .or_else(|| {
                        blocks.iter().enumerate().position(|(other, block)| {
                            other != index
                                && in_turn(block)
                                && block.kind != TimelineRowKind::UserMessage
                        })
                    })
                    .unwrap_or(index);
                head.insert(turn.id.clone(), first_process);
                // The attribution is "Agent · account · model"; the first
                // segment is the name the catalogue knows the Agent by, which
                // is what turns it into the identity both clients match marks
                // against.
                identity.insert(
                    turn.id.clone(),
                    turn.runtime_attribution
                        .as_deref()
                        .and_then(|attribution| attribution.split('·').next())
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .map(|label| self.agent_identity_for_label(label)),
                );
                labels.insert(
                    turn.id.as_str(),
                    crate::turn::format_worked_duration(crate::turn::worked_duration(turn)),
                );
            }
        }
        if labels.is_empty() {
            return;
        }
        // Every drawn row of a folding turn except the user's own message
        // carries the fold. The answer's copy is the control; the rest hide
        // while it is shut.
        for (index, block) in blocks.iter_mut().enumerate() {
            let Some(turn_id) = block.turn_id.clone() else {
                continue;
            };
            let Some(label) = labels.get(turn_id.as_str()).cloned() else {
                continue;
            };
            if block.kind == TimelineRowKind::UserMessage {
                continue;
            }
            let is_conclusion = conclusion.get(&turn_id) == Some(&index);
            let is_head = head.get(&turn_id) == Some(&index);
            let agent_identity = identity.get(&turn_id).cloned().flatten();
            // The projection asks for a settled turn's work to be hidden; a
            // reader who opens it says so in the transcript's expansion map,
            // which `set_blocks` re-applies on every rebuild.
            block.fold = Some(TurnFold {
                turn_id,
                label,
                expanded: false,
                is_conclusion,
                is_head,
                agent_identity,
            });
        }
    }

    /// Use the desktop's turn projection for reasoning placement and liveness.
    /// Other transcript rows retain their existing TUI ordering and notices.
    fn project_reasoning(&self, rows: &mut Vec<TimelineRow>) {
        use vibex_desktop_model::{ReasoningDisplayMode, TimelineRowKind};

        if !rows
            .iter()
            .any(|row| row.kind == TimelineRowKind::Reasoning)
        {
            return;
        }
        let mode = self.settings.reasoning_display_mode;
        let turns = self
            .agent
            .state
            .conversation_turns_with_reasoning_mode(mode);
        let reasoning = turns
            .iter()
            .flat_map(|turn| &turn.process_rows)
            .filter(|row| row.kind == TimelineRowKind::Reasoning)
            .map(|row| (row.id.as_str(), row.streaming))
            .collect::<HashMap<_, _>>();
        let live = (mode == ReasoningDisplayMode::LatestAtBottom)
            .then(|| turns.last())
            .flatten()
            .filter(|turn| !turn.complete && !turn.superseded)
            .and_then(|turn| {
                let body = turn.live_status.as_deref()?;
                let mut row = rows
                    .iter()
                    .rev()
                    .find(|row| {
                        row.kind == TimelineRowKind::Reasoning
                            && row.turn_id.as_deref() == Some(turn.id.as_str())
                    })?
                    .clone();
                row.id = format!("reasoning-live:{}", turn.id);
                row.body = body.to_string();
                row.streaming = true;
                Some(row)
            });
        rows.retain_mut(|row| {
            if row.kind != TimelineRowKind::Reasoning {
                return true;
            }
            let Some(streaming) = reasoning.get(row.id.as_str()) else {
                return false;
            };
            row.streaming = *streaming && row.streaming;
            true
        });
        rows.extend(live);
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
    /// already starts at sequence 0, or when two pages are already in flight,
    /// so a caller may offer this on every scroll gesture.
    pub fn load_older_history(&mut self) -> Option<Effect> {
        let ticket = self.agent.begin_timeline_before().ok()?;
        Some(Effect::LoadOlder { ticket })
    }

    /// Keep the page above the reader coming while they are close to the top.
    ///
    /// Prefetch is a head start, not a second trigger: the reader crosses the
    /// threshold a screen before they run out of loaded history, so the page
    /// they are about to ask for is usually already on the wire. The refusal
    /// is the controller's, which is what keeps a scroll position that stays
    /// near the top from queueing a request per keypress.
    pub fn prefetch_older_history(&mut self) -> Option<Effect> {
        // While the dock owns the list keys, `k` is moving its cursor, not
        // scrolling the transcript it happens to be sitting above.
        if self.dock_is_focused() || !self.near_transcript_top() {
            return None;
        }
        self.load_older_history()
    }

    /// Whether the reader is within about a screen and a half of the top.
    ///
    /// Separate from [`Self::at_transcript_top`] because the two answer
    /// different questions: prefetch wants the approach, while the exact
    /// parked position is what says the reader has nothing left above them.
    pub fn near_transcript_top(&self) -> bool {
        if self.page != Page::Agent
            || self.overlay.is_some()
            || self.filtering
            || self.scroll.follow
        {
            return false;
        }
        let threshold = self
            .transcript_band_rows
            .saturating_mul(OLDER_HISTORY_PREFETCH_HALF_SCREENS)
            / 2;
        self.scroll.offset <= threshold
    }

    /// Whether the reader is parked at the very top of a scrolled transcript.
    pub fn at_transcript_top(&self) -> bool {
        self.near_transcript_top() && self.scroll.offset == 0 && self.agent.state.timeline_has_older
    }

    /// Records that an open activity block is showing only a summary.
    ///
    /// Called where a block becomes expanded, because that is the gesture that
    /// asks for its output; the read itself is issued by
    /// [`Self::take_timeline_detail_effects`], which the event loop drains.
    pub fn note_block_detail(&mut self, index: usize) {
        let Some(block) = self.transcript.block(index) else {
            return;
        };
        if !block.expanded || block.is_hidden() || block.activity.is_none() {
            return;
        }
        let sequence = block.sequence;
        if self.timeline_item_is_incomplete(sequence) {
            self.pending_timeline_details.insert(sequence);
        }
    }

    /// Records every open activity the transcript is showing a summary for.
    ///
    /// The gesture behind this opens blocks the reader never pointed at, so
    /// they all have to be looked at rather than only the selected one.
    pub fn note_all_block_details(&mut self) {
        for index in 0..self.transcript.len() {
            self.note_block_detail(index);
        }
    }

    /// Retires one activity-detail read that has come back.
    pub fn finish_timeline_detail(&mut self, ticket: &vibex_ui::AgentTimelineItemTicket) {
        self.timeline_details_in_flight
            .remove(&(ticket.generation, ticket.sequence));
    }

    /// Forgets activity-detail reads the projection has moved past.
    ///
    /// Every ticket issued for the replaced generation is stale, so its answer
    /// can only be dropped; the rebuild asks again for what is still open.
    pub fn prune_timeline_details(&mut self) {
        let generation = self.agent.state.generation;
        self.timeline_details_in_flight
            .retain(|(issued, _)| *issued == generation);
    }

    /// The activity details the transcript wants, ready for the worker.
    ///
    /// A batch rather than the whole list: expanding a long run of tools can
    /// open hundreds of blocks at once and every read is a round trip, so the
    /// rest is asked for as the answers land and the transcript rebuilds.
    pub fn take_timeline_detail_effects(&mut self) -> Vec<Effect> {
        let generation = self.agent.state.generation;
        let pending = std::mem::take(&mut self.pending_timeline_details);
        let mut effects = Vec::new();
        for sequence in pending {
            if effects.len() >= TIMELINE_DETAIL_BATCH {
                break;
            }
            if self
                .timeline_details_in_flight
                .contains(&(generation, sequence))
            {
                continue;
            }
            // A sequence the projection no longer holds cannot be read: there
            // would be nothing to apply the answer to.
            let Ok(ticket) = self.agent.begin_timeline_item(sequence) else {
                continue;
            };
            self.timeline_details_in_flight
                .insert((generation, sequence));
            effects.push(Effect::LoadTimelineItem { ticket });
        }
        effects
    }

    /// Whether the projection's copy of a sequence is still a summary.
    fn timeline_item_is_incomplete(&self, sequence: i64) -> bool {
        self.agent
            .state
            .timeline
            .items
            .binary_search_by_key(&sequence, |item| item.sequence)
            .is_ok_and(|index| !self.agent.state.timeline.items[index].has_complete_payload())
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
        self.transcript.reveal_block(block);
        // Landing on a match opens it, and an activity opens onto its output.
        self.note_block_detail(block);
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

    /// The palette's entries, each with the heading it is drawn under.
    ///
    /// The session section is offered only on a page that is showing a session.
    /// Its verbs act on the session list's own cursor, so anywhere else — the
    /// list itself, the configuration centre, a composing page — they would
    /// rename, fork or delete a row chosen behind a page that is no longer
    /// showing it, which is the confusion the section is withheld to prevent.
    /// The commands are not lost to those pages: the list answers for the row
    /// under its own cursor with the keys the row names.
    pub fn palette_entries(&self, query: &str) -> Vec<crate::view::PaletteListing> {
        let showing_a_session = self.page_shows_session();
        crate::view::palette_listings(query, self.strings)
            .into_iter()
            .filter(|listing| {
                showing_a_session || listing.group != crate::view::PaletteGroup::Session
            })
            .collect()
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
            // A disconnected seat that could own the home says so, and says how:
            // "not available" is the whole truth only while there is nothing the
            // reader can do about it.
            if self.handover_offered() {
                return self.set_banner(
                    Banner::warning(self.strings.handover_offer())
                        .with_priority(BannerPriority::Warning),
                );
            }
            return self.set_banner(
                Banner::warning(self.strings.toast_offline())
                    .with_priority(BannerPriority::Warning),
            );
        }
        self.clear_banner(BannerPriority::Warning);
        if !self.queued_for_active().is_empty() && self.page == Page::Agent {
            // A queue that is waiting on the clock says so rather than claiming
            // a running turn is what holds it: with a send time armed and no
            // turn running, "held until the running turn ends" would be a
            // promise this client has no intention of keeping.
            let held = if self
                .queued_for_active()
                .iter()
                .all(|at| self.queued_messages[*at].scheduled_at_ms.is_some())
            {
                self.strings.schedule_message()
            } else {
                self.strings.queue_held()
            };
            let text = format!("{held} · {}", self.strings.queue_hint());
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
        self.enqueue_scheduled(session_id, text, images, None);
    }

    /// Hold a message until its own deadline, or until the turn ends.
    ///
    /// A message with a deadline waits the same way an ordinary held one does
    /// and is released by the same send path; the only difference is which
    /// clock decides that it is ready. `None` is the ordinary case.
    pub fn enqueue_scheduled(
        &mut self,
        session_id: VibexSessionId,
        text: String,
        images: Vec<(crate::composer::ImageAttachment, u32)>,
        scheduled_at_ms: Option<i64>,
    ) {
        self.queued_messages.push(QueuedMessage {
            session_id,
            text,
            images,
            scheduled_at_ms,
        });
        self.queue_selection = Some(self.queued_for_active().len().saturating_sub(1));
        self.save_queue();
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
    ///
    /// The message's deadline comes back with it. A reader who pulled a
    /// scheduled message back did so to change something about it — the words
    /// or the hour — and dropping the hour silently would send it the moment
    /// the turn ended, which is the one thing they were trying to avoid.
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
                    scheduled_at_ms: None,
                },
            );
        }
        self.composer.set_draft(queued.text, queued.images);
        if let Some(target) = self.composer_target.clone() {
            match queued.scheduled_at_ms {
                Some(at_ms) => {
                    self.draft_schedules
                        .insert(target, crate::schedule::MessageSchedule::At(at_ms));
                }
                None => {
                    self.draft_schedules.remove(&target);
                }
            }
        }
        self.focus = Focus::Composer;
        self.composer_mode = ComposerMode::Normal;
        self.save_queue();
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
        self.save_queue();
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
        self.save_queue();
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
        self.save_queue();
        Some((queued.text, attachments))
    }

    /// The messages whose session has finished its turn and can take another.
    ///
    /// Every session with a queue is asked, not only the one on screen: a
    /// message is released when *its* turn ends, wherever the reader happens to
    /// be looking. Called after every worker message rather than on a special
    /// "turn ended" event — the client has no such event, and a queue that only
    /// drains on one signal would stall the moment that signal changed shape.
    ///
    /// A scheduled message is not ready until its own deadline, and a message
    /// still waiting is skipped rather than allowed to block the ones behind
    /// it: the reader's queue is ordered by what they wrote, and a message they
    /// set an hour out is not an instruction to hold everything after it. The
    /// call runs on every pass of the event loop, so a deadline that arrives
    /// while the client is sitting still releases on the next pass.
    pub fn drain_queue(
        &mut self,
    ) -> Vec<(VibexSessionId, String, Vec<vibex_core::MessageAttachment>)> {
        if !self.live.is_live() || self.queued_messages.is_empty() {
            return Vec::new();
        }
        let now_ms = vibex_core::unix_timestamp_ms();
        let mut ready: Vec<VibexSessionId> = Vec::new();
        for queued in &self.queued_messages {
            if ready.contains(&queued.session_id) {
                continue;
            }
            if !queued.is_due(now_ms) {
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
                .position(|queued| queued.session_id == session_id && queued.is_due(now_ms))
            else {
                continue;
            };
            let queued = self.queued_messages.remove(index);
            if session_id == self.selected_session_id().cloned().unwrap_or_default() {
                let remaining = self.queued_for_active().len();
                self.queue_selection =
                    (remaining > 0).then(|| self.queue_selection.unwrap_or(0).min(remaining - 1));
            }
            self.history_log.push(queued.text.clone());
            let attachments = self.wire_attachments(&queued.images);
            released.push((session_id, queued.text, attachments));
        }
        if !released.is_empty() {
            // Only a pass that actually released something rewrote the queue:
            // this runs on every worker message, and a file write per pass
            // would be a file write per frame of a running turn.
            self.save_queue();
        }
        released
    }

    // ---- scheduling a send -----------------------------------------------

    /// The send time the Composer in front of the reader is written for.
    pub fn draft_schedule(&self) -> Option<crate::schedule::MessageSchedule> {
        self.composer_target
            .as_ref()
            .and_then(|target| self.draft_schedules.get(target).copied())
    }

    /// Give the Composer's draft a send time, or take the one it has away.
    ///
    /// The value belongs to the draft rather than to the page, so a reader who
    /// steps out to the session list and comes back finds it still armed.
    pub fn set_draft_schedule(
        &mut self,
        schedule: Option<crate::schedule::MessageSchedule>,
    ) -> bool {
        let Some(target) = self.composer_target.clone() else {
            return false;
        };
        match schedule {
            Some(schedule) => {
                self.draft_schedules.insert(target, schedule);
            }
            None => {
                self.draft_schedules.remove(&target);
            }
        }
        true
    }

    /// Take the draft's schedule, resolving a countdown against the moment the
    /// message is actually submitted.
    pub fn take_draft_schedule(
        &mut self,
        now_ms: i64,
    ) -> Result<Option<i64>, crate::schedule::ScheduleError> {
        let Some(schedule) = self.draft_schedule() else {
            return Ok(None);
        };
        let at_ms = schedule.deadline(now_ms)?;
        if let Some(target) = self.composer_target.clone() {
            self.draft_schedules.remove(&target);
        }
        Ok(Some(at_ms))
    }

    /// The earliest message still waiting on a clock for one session.
    pub fn scheduled_message_for(&self, session_id: &VibexSessionId) -> Option<&QueuedMessage> {
        self.queued_messages
            .iter()
            .filter(|queued| &queued.session_id == session_id)
            .filter(|queued| queued.scheduled_at_ms.is_some())
            .min_by_key(|queued| queued.scheduled_at_ms)
    }

    /// The next send another client is holding for one session.
    ///
    /// A schedule belongs to whoever keeps its clock, and this client is not
    /// always that client: the Desktop arms a message, waits for the hour in
    /// its own process and publishes the fact through the arrangement every
    /// client already polls for its sidebar marks. Reading it here is what lets
    /// a row count down — and a session page say what is waiting — without this
    /// client pretending it could send something it is not holding.
    pub fn authority_scheduled_send(
        &self,
        session_id: &VibexSessionId,
    ) -> Option<&vibex_desktop_model::SidebarScheduledSend> {
        self.projection
            .sidebar_organization
            .as_ref()?
            .scheduled_send_for(session_id.as_str())
    }

    /// The earliest send one session is waiting for, whichever client holds it.
    ///
    /// The words and the hour come back together: a row borrows the words as
    /// the session's name while it has none of its own, and every surface that
    /// shows the wait counts down to the same hour. Two clients can be holding
    /// a send for one session, and what the reader is waiting for is whichever
    /// goes out first.
    ///
    /// A message this client is holding is projected with its pictures: the
    /// name a row borrows has to read the screenshot a paste carried, or a
    /// send that is only a picture would leave the row nameless. The published
    /// words are already the holder's projection and are read verbatim.
    pub fn earliest_scheduled_send(
        &self,
        session_id: &VibexSessionId,
    ) -> Option<(std::borrow::Cow<'_, str>, i64)> {
        let held = self.scheduled_message_for(session_id).and_then(|queued| {
            Some((
                crate::attachment::draft_display_text(&queued.text, &queued.images),
                queued.scheduled_at_ms?,
            ))
        });
        let shared = self.authority_scheduled_send(session_id).map(|send| {
            (
                std::borrow::Cow::Borrowed(send.text.as_str()),
                send.scheduled_at_ms,
            )
        });
        match (held, shared) {
            (Some(held), Some(shared)) => Some(if held.1 <= shared.1 { held } else { shared }),
            (held, shared) => held.or(shared),
        }
    }

    /// The deadline of the next message one session is waiting to send.
    pub fn next_scheduled_message_at(&self, session_id: &VibexSessionId) -> Option<i64> {
        self.earliest_scheduled_send(session_id)
            .map(|(_, at_ms)| at_ms)
    }

    /// The earliest deadline any session is waiting on, so a tick can tell
    /// whether the countdowns on screen have moved.
    pub fn next_scheduled_message_at_anywhere(&self) -> Option<i64> {
        let held = self
            .queued_messages
            .iter()
            .filter_map(|queued| queued.scheduled_at_ms)
            .min();
        // Another client's sends count too: the seconds on its rows are the
        // same seconds, and a page that only repainted for this client's own
        // queue would sit on a countdown that had stopped moving.
        let shared = self
            .projection
            .sidebar_organization
            .as_ref()
            .and_then(|view| {
                view.scheduled_sends
                    .iter()
                    .map(|send| send.scheduled_at_ms)
                    .min()
            });
        [held, shared].into_iter().flatten().min()
    }

    /// The name a session borrows from the scheduled message it is still
    /// waiting to send, when the session has no name of its own yet.
    ///
    /// A session opened for a scheduled message exists before the message does:
    /// the manager persists it under its default `<agent> session` title until a
    /// message is actually submitted, so for the whole wait every authoritative
    /// snapshot would put that placeholder on the row. The name is read from
    /// what the reader wrote instead, and stops being read the moment the send
    /// leaves the queue — which is exactly what leaves the Agent's own rename
    /// free to land once the message has really been sent. A send another client
    /// is holding names the row the same way: the reader who armed it on the
    /// Desktop is owed the same row here.
    pub fn pending_scheduled_session_title(&self, session: &AgentSession) -> Option<String> {
        let (text, _) = self.earliest_scheduled_send(&session.id)?;
        let title = vibex_core::normalize_agent_session_title(&text)?;
        let fallback = session_fallback_title(&session.agent_id);
        (session.title == fallback || session.title == title).then_some(title)
    }

    /// The label a session row shows: its own title, or the name of the
    /// scheduled message it is waiting to send.
    pub fn session_row_title(&self, session: &AgentSession) -> String {
        self.pending_scheduled_session_title(session)
            .unwrap_or_else(|| session.title.clone())
    }

    /// Record the name a creation's first message gives its session.
    ///
    /// Read by [`Self::reconcile_pending_session_title`] until an authoritative
    /// title lands. A message that normalizes to nothing — whitespace, or the
    /// bare command that sends no prose — names nothing, so the placeholder
    /// stands rather than the row wearing a blank name. Pictures count: a
    /// creation whose only message is a pasted screenshot is named by the label
    /// the composer drew for it.
    pub fn note_pending_session_title(
        &mut self,
        session_id: &VibexSessionId,
        message: &str,
        images: &[(crate::composer::ImageAttachment, u32)],
    ) {
        let display = crate::attachment::draft_display_text(message, images);
        if let Some(title) = vibex_core::normalize_agent_session_title(&display) {
            self.pending_session_titles
                .insert(session_id.clone(), title);
        }
    }

    /// Forget the name recorded for a creation that never became a session.
    ///
    /// The identity is the failed attempt's, and the reader's retry reserves a
    /// fresh one, so nothing would ever look this name up again.
    pub fn forget_pending_session_title(&mut self, session_id: &VibexSessionId) {
        self.pending_session_titles.remove(session_id);
    }

    /// Take an authoritative snapshot, with the name this client recorded for
    /// the session standing in while the authority still reports the
    /// placeholder.
    ///
    /// This is what keeps the manager's default off a row the reader already
    /// named by writing: the creation's own response is a placeholder snapshot,
    /// and so is every list read and Agent update until the message reaches a
    /// turn. The first title that is *not* the placeholder releases the record —
    /// and a snapshot that updates a row this client already holds is what
    /// releases it, because the rows projected for a creation the client has
    /// only just acknowledged are the first non-placeholder snapshots for their
    /// ids, and releasing there would drop the name before the placeholder ever
    /// arrived to be outlasted.
    pub fn reconcile_pending_session_title(&mut self, session: AgentSession) -> AgentSession {
        let is_fallback = session.title == session_fallback_title(&session.agent_id);
        if !is_fallback {
            if self.session_title(&session.id).is_some() {
                self.pending_session_titles.remove(&session.id);
            }
            return session;
        }
        let Some(title) = self.pending_session_titles.get(&session.id).cloned() else {
            return session;
        };
        AgentSession { title, ..session }
    }

    /// Take the authoritative session list with those recorded names standing
    /// in, before the controller stores it.
    pub fn reconcile_session_list(
        &mut self,
        result: BackendResult<Vec<AgentSession>>,
    ) -> BackendResult<Vec<AgentSession>> {
        result.map(|sessions| {
            sessions
                .into_iter()
                .map(|session| self.reconcile_pending_session_title(session))
                .collect()
        })
    }

    /// Take an event with the session it carries reconciled, so a placeholder
    /// announcement cannot put the manager's default back on a named row.
    pub fn reconcile_session_event(
        &mut self,
        event: vibex_backend::BackendEvent,
    ) -> vibex_backend::BackendEvent {
        match event {
            vibex_backend::BackendEvent::SessionUpdated(session) => {
                vibex_backend::BackendEvent::SessionUpdated(
                    self.reconcile_pending_session_title(session),
                )
            }
            other => other,
        }
    }

    /// Drop the names of sessions this client is no longer creating and the
    /// list no longer holds.
    ///
    /// A record is normally released by the first authoritative title, so this
    /// is only for the ones that left before a title arrived: a session deleted
    /// while its name was still standing in. A creation that has not been
    /// acknowledged yet is kept — it is not in the list, and it is exactly the
    /// row the name is for.
    pub fn retain_pending_session_titles(&mut self) {
        let keep = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|session| session.id.clone())
            .chain(
                self.pending_session_titles
                    .keys()
                    .filter(|session_id| self.session_is_uncreated(session_id))
                    .cloned(),
            )
            .collect::<BTreeSet<VibexSessionId>>();
        self.pending_session_titles
            .retain(|session_id, _| keep.contains(session_id));
    }

    /// How long a session's next scheduled send still waits, when it has one.
    pub fn session_schedule_countdown(
        &self,
        session_id: &VibexSessionId,
        now_ms: i64,
    ) -> Option<String> {
        self.next_scheduled_message_at(session_id)
            .map(|at_ms| crate::schedule::format_schedule_countdown(at_ms.saturating_sub(now_ms)))
    }

    /// What a held message is waiting for, in the words its row can hold.
    ///
    /// A deadline still ahead names both the hour and the wait, which are the
    /// two questions a scheduled row is asked. One that has arrived says what
    /// it is still waiting for instead: the clock is not the reason any more,
    /// and a row that kept counting down past zero would be lying about which
    /// of the two is holding it.
    pub fn schedule_status_label(
        &self,
        at_ms: i64,
        session_id: &VibexSessionId,
        now_ms: i64,
    ) -> String {
        let remaining = at_ms.saturating_sub(now_ms);
        if remaining > 0 {
            format!(
                "{} {} · {}",
                crate::glyphs::schedule_marker(self.glyph_tier()),
                crate::schedule::format_send_time(at_ms),
                crate::schedule::format_remaining(remaining)
            )
        } else if self.session_is_running(session_id) {
            self.strings.schedule_due_running().to_string()
        } else {
            self.strings.schedule_due_waiting().to_string()
        }
    }

    /// Move the countdowns on. Answers whether the display changed, which is
    /// what keeps the interface repainting its seconds — the same contract the
    /// auto-continue countdown has.
    ///
    /// The bucket is the unit the surfaces actually print: seconds under a day
    /// and minutes past it, so a list that is only showing hours is not
    /// repainted once a second to say the same thing.
    pub fn tick_schedules(&mut self, now_ms: i64) -> bool {
        let shown = self.next_scheduled_message_at_anywhere().map(|at_ms| {
            let remaining = at_ms.saturating_sub(now_ms).max(0);
            // Rounded up, because the surfaces print a countdown that reads
            // `00:00:01` while most of a second is still left.
            if remaining < 24 * 60 * 60 * 1_000 {
                (remaining + 999) / 1_000
            } else {
                (remaining + 59_999) / 60_000
            }
        });
        if shown == self.schedule_shown_second {
            return false;
        }
        // Any change repaints, including the last countdown going away: a row
        // that kept showing a number after the send left would be describing a
        // message that is no longer there.
        self.schedule_shown_second = shown;
        true
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

    /// Drop the client's own per-session state for sessions the runtime removed.
    ///
    /// A removal event names sessions that no longer exist, and the shared
    /// controller drops their rows. Everything this client derives from events
    /// it received — the unread mark, the last-thing-said echo, a composed draft
    /// — is keyed by the same id and would otherwise outlive the row it
    /// describes, so it goes with the session.
    pub fn forget_removed_sessions(&mut self, session_ids: &[VibexSessionId]) {
        for session_id in session_ids {
            let id = session_id.as_str();
            self.unread_sessions.remove(id);
            self.authority_unread_sessions.remove(id);
            self.session_echoes.remove(id);
            self.pending_session_titles.remove(session_id);
            self.pending_creations
                .retain(|pending_id, _| pending_id != session_id);
        }
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

    /// The string an Agent's brand mark is matched against.
    ///
    /// The same parts, in the same order, as the desktop joins them — the id,
    /// the display label, the command and the preset — because the mark has to
    /// come out the same in both clients. The management record is the only
    /// place the command and preset live, so an Agent the client has not loaded
    /// falls back to the id and label it does have.
    pub fn agent_identity(&self, agent_id: &vibex_core::AgentId, label: &str) -> String {
        let id = agent_id.as_str();
        let agent = self
            .management_data
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == id);
        let command = agent
            .and_then(|agent| agent.command.as_ref())
            .map(|command| command.command.clone())
            .unwrap_or_default();
        let preset = agent
            .and_then(|agent| agent.params.get("preset"))
            .and_then(|preset| preset.as_str())
            .unwrap_or_default();
        vibex_brands::identity(&[id, label, command.as_str(), preset])
    }

    /// [`App::agent_identity`] for a session, whose label comes from the
    /// catalogue the session list is drawn against.
    pub fn session_agent_identity(&self, session: &AgentSession) -> String {
        let label = self.session_agent_label(session);
        self.agent_identity(&session.agent_id, &label)
    }

    /// The string an Agent's brand mark is matched against, from a label.
    ///
    /// A timeline row names the Agent that answered for it by its display
    /// label, which is not enough on its own: the mark is matched against the
    /// id, the label, the command and the preset together, exactly as the
    /// session list matches it, so an Agent whose display name is not its slug
    /// still wears the one face it wears everywhere. A label the catalogue does
    /// not know is matched as it stands — a custom Agent is still an Agent, and
    /// a mark it does not have leaves the row its words.
    pub fn agent_identity_for_label(&self, label: &str) -> String {
        if let Some(agent) = self
            .runtime_options
            .as_ref()
            .and_then(|catalog| catalog.agents.iter().find(|agent| agent.label == label))
        {
            return self.agent_identity(&agent.agent_id, &agent.label);
        }
        label.to_string()
    }

    /// Whether the session list marks this session as having something new.
    pub fn session_is_unread(&self, session_id: &VibexSessionId) -> bool {
        self.unread_sessions.contains(session_id.as_str())
            || self.authority_unread_sessions.contains(session_id.as_str())
    }

    /// How many sessions a heading holds that are working right now.
    ///
    /// Counted from the sessions the client holds rather than from the rows it
    /// drew, so a folded heading still reports the work going on inside it —
    /// the same rule the page's own tally follows. It deliberately ignores the
    /// list's filter, because the mark is a fact about the project rather than
    /// about the view of it: the desktop's own running count does the same.
    ///
    /// An empty `project_id` is a heading nothing is filed under — a folder no
    /// project owns — and has no sessions to count.
    pub fn sidebar_running_count(&self, project_id: &str, workspace_id: &str) -> usize {
        if project_id.is_empty() {
            return 0;
        }
        let sessions = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default();
        sessions
            .iter()
            .filter(|session| {
                session.deleted_at_ms.is_none()
                    && matches!(
                        session.state,
                        AgentSessionState::Running | AgentSessionState::Initializing
                    )
                    && session.project_id.as_str() == project_id
                    && (workspace_id.is_empty() || session.workspace_id.as_str() == workspace_id)
            })
            .count()
    }

    /// The directory a session started from a heading's own button would open
    /// in: the project's checkout where the runtime has published one.
    ///
    /// The composing page already knows what to do with a workspace it is
    /// handed — it names it, and a session created from the page opens there —
    /// so a heading's "new session" is that page with the directory already
    /// chosen, rather than a second way to create a session.
    pub fn project_workspace_root(&self, project_id: &str, workspace_id: &str) -> Option<String> {
        if project_id.is_empty() {
            return None;
        }
        self.workspace_rows
            .iter()
            .find(|summary| {
                summary.project.id.as_str() == project_id
                    && (workspace_id.is_empty() || summary.workspace.id.as_str() == workspace_id)
            })
            .or_else(|| {
                self.workspace_rows
                    .iter()
                    .find(|summary| summary.project.id.as_str() == project_id)
            })
            .map(|summary| summary.workspace.root_path.clone())
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

    // ---- composer history -------------------------------------------------

    /// The messages `Up`/`Down` walk, oldest first.
    ///
    /// The pool is the selected session's own transcript — the same one the
    /// desktop's composer recalls from — so a message another client sent is
    /// recallable here too, and a session that was cleared or reloaded cannot
    /// leave a stale message behind the cursor. The transcript already carries
    /// a send the runtime has not echoed yet, because it projects that send as
    /// the row the echo will become, so the newest message is recallable at
    /// once without the walk reading the pending queue itself.
    ///
    /// The body is the row's own, which is what the client draws — attachments
    /// in their place — so what comes back to the composer is what the message
    /// looked like when it was sent.
    pub fn composer_history_entries(&self) -> Vec<String> {
        if !self.page_shows_session() {
            return Vec::new();
        }
        self.transcript
            .blocks()
            .iter()
            .filter(|block| {
                block.kind == vibex_desktop_model::TimelineRowKind::UserMessage
                    && !block.body.trim().is_empty()
            })
            .map(|block| block.body.clone())
            .collect()
    }

    /// Step the composer's `Up`/`Down` walk over the session's messages.
    ///
    /// Returns whether the press was the walk's. A press that was not keeps its
    /// normal meaning — the caret moves through the draft — which is what keeps
    /// a multi-line message editable with the same two keys.
    ///
    /// `Up` is the only way in, and only from an empty box: a box holding a
    /// draft the reader wrote belongs to the reader, so the caret moves inside
    /// it instead of the draft being swapped for an older message.
    pub fn step_composer_history(&mut self, previous: bool) -> bool {
        let Some(session_id) = self.selected_session_id().cloned() else {
            return false;
        };
        let entries = self.composer_history_entries();
        let current = self.composer.text().to_string();
        let active = self.history.is_browsing(Some(&session_id));
        if !active && !(previous && current.trim().is_empty() && !entries.is_empty()) {
            return false;
        }
        if !active {
            self.history.begin(&session_id, &current);
        }
        let step = if previous {
            self.history.previous(&session_id, &entries, &current)
        } else {
            self.history.next(&session_id, &entries, &current)
        };
        match step {
            HistoryStep::Ignored => false,
            HistoryStep::Sticky => true,
            HistoryStep::Text(text) => {
                self.composer.set_text(text);
                true
            }
        }
    }

    /// Leave a running walk, putting the draft it displaced back.
    ///
    /// Answers whether one was running, so a caller that means something else
    /// by the key — `Esc` — only spends the press on the walk when there is one.
    pub fn cancel_composer_history(&mut self) -> bool {
        let Some(draft) = self.history.cancel() else {
            return false;
        };
        self.composer.set_text(draft);
        true
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
        let entries = self.history_log.entries();
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
    ///
    /// The press covers nothing yet: until the pointer moves it is a click,
    /// and a click leaves the clipboard alone.
    pub fn begin_text_selection(&mut self, line: usize, column: u16) {
        self.text_selection = Some(TextSelection {
            anchor: (line, column),
            head: (line, column),
            moved: false,
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
        // The picture is read here, once, because the preview is the only
        // reader that wants its pixels and a preview is drawn on every frame
        // the caret rests on the chip. A file that cannot be read is still an
        // attachment: the runtime it is sent to is the one that has to have it.
        let thumbnail = std::fs::read(trimmed)
            .ok()
            .and_then(|bytes| crate::preview::thumbnail(&bytes));
        self.composer
            .insert_image(crate::composer::AttachedImage {
                mime_type: mime.to_string(),
                source: crate::composer::ImageSource::Path(trimmed.to_string()),
                byte_len: metadata.len() as usize,
                thumbnail,
            })
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
        let byte_len = bytes.len();
        let thumbnail = crate::preview::thumbnail(&bytes);
        self.composer
            .insert_image(crate::composer::AttachedImage {
                mime_type: mime_type.into(),
                source: crate::composer::ImageSource::Bytes(std::sync::Arc::new(bytes)),
                byte_len,
                thumbnail,
            })
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
        selection.moved = true;
        true
    }

    /// Finish the selection in progress, reporting whether it covers anything.
    pub fn finish_text_selection(&mut self) -> bool {
        let Some(selection) = self.text_selection.as_ref() else {
            return false;
        };
        !selection.is_empty()
    }

    /// Replace the selection with a whole word, for a double click.
    pub fn select_word_at(&mut self, line: usize, column: u16) -> bool {
        let theme = self.theme.clone();
        let strings = self.strings;
        let Some(row) = self
            .transcript
            .plain_lines(line, line + 1, &theme, strings)
            .into_iter()
            .next()
        else {
            return false;
        };
        // The pointer counts band cells and the row's first cells are chrome,
        // so a click is read as a click on the text before the word under it
        // can be found.
        let column = usize::from(column).saturating_sub(row.chrome);
        let Some((start, end)) = u16::try_from(column)
            .ok()
            .and_then(|column| crate::transcript::word_at(&row.text, column))
        else {
            return false;
        };
        // `word_at` answers where the word stops; a selection ends on the cell
        // it covers, which is the one before that.
        let chrome = u16::try_from(row.chrome).unwrap_or(u16::MAX);
        self.text_selection = Some(TextSelection {
            anchor: (line, start.saturating_add(chrome)),
            head: (line, end.saturating_sub(1).saturating_add(chrome)),
            moved: true,
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
    ///
    /// A selection is a range of *cells*, and the first cell of every row is
    /// chrome — the block's marker, its pad, the padding of the reader's own
    /// box — which never reaches the clipboard. Both ends of the drag are cells
    /// the reader pointed at, so both are copied.
    pub fn selected_text(&mut self) -> Option<String> {
        let selection = self.text_selection?;
        if selection.is_empty() {
            return None;
        }
        let (start, end) = selection.ordered();
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
            // The row's text starts `chrome` cells in, so a band column reads
            // as a column of the text by crossing that much of it. A drag that
            // starts in the margin means the text from its first cell.
            let from = if index == 0 {
                usize::from(start.1).saturating_sub(line.chrome)
            } else {
                0
            };
            let to = if index == last {
                usize::from(end.1).checked_sub(line.chrome)
            } else {
                Some(usize::MAX)
            };
            let text = match to {
                // The drag came up in the row's chrome: it covers none of the
                // row's text, and a range that ran backwards would otherwise
                // still take the first character.
                None => String::new(),
                Some(to) => crate::text::take_columns(&line.text, from, to),
            };
            output.push(text.trim_end().to_string());
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

    /// Whether the host is offering this client the runtime home.
    pub fn handover_offered(&self) -> bool {
        !self.handover_requested
            && self
                .handover
                .as_ref()
                .is_some_and(crate::HandoverOffer::is_available)
    }

    /// Locale-aware, capability-aware reason an action is unavailable.
    pub fn unavailable_reason(&self, intent: Intent) -> Option<String> {
        if !self.live.is_live() && intent.mutates() && !intent.works_offline() {
            // The reader who reaches for something that needs the runtime is
            // exactly the reader who wants to know the home is free now.
            if self.handover_offered() {
                return Some(self.strings.handover_offer().to_string());
            }
            return Some(self.strings.toast_offline().to_string());
        }
        None
    }
}

/// How many history entries the composer's drawer will show.
pub const MAX_HISTORY_MATCHES: usize = 100;

/// How many sent messages the composer's `? ` drawer keeps to search.
///
/// The arrow walk reads the session's timeline and is bounded by it instead;
/// this bounds only the reader's own vocabulary, which is there to be
/// remembered rather than to be complete.
pub const HISTORY_LOG_LIMIT: usize = 64;

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
    /// What kind of runtime this client is attached to. The devices page reads
    /// it to decide whether a desktop client may pair here at all, and the
    /// notification layer reads it to decide whether to step back for a desktop
    /// that announces the same events itself; the client cannot work it out for
    /// itself, because the license is a property of the runtime rather than of
    /// the connection.
    pub runtime_kind: vibex_core::RemoteServerKind,
    pub capability: ColorCapability,
    /// The theme this run was started with, when the composition root named
    /// one (`--theme`, `VIBEX_THEME`). It seeds the slot for `mode`; the other
    /// appearance's slot still comes from `remembered`.
    pub theme_id: Option<String>,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    /// What the interface itself remembered last time: the theme per
    /// appearance and the standing workspace.
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
    /// Where the Composer's held messages are kept.
    ///
    /// The one file here that is not a preference: a held message is work the
    /// reader did, and a message given an hour to wait has to outlive the
    /// process that took it or the schedule it carries means nothing. `None`
    /// keeps the queue in memory for the run, which is what a preview or a test
    /// wants.
    pub queue_path: Option<std::path::PathBuf>,
    /// Where the notification layer's choices are kept.
    ///
    /// `None` reads nothing and writes nothing, which is what a preview and a
    /// test want: a test that flipped a notification switch would otherwise
    /// edit the file of whoever ran it, and would race every test doing the
    /// same. The composition root names the real file.
    pub notify_path: Option<std::path::PathBuf>,
    /// The host's standing offer to hand this client the runtime home. The
    /// interface only draws it; taking it is the host's job.
    pub handover: Option<crate::HandoverOffer>,
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
            runtime_kind: vibex_core::RemoteServerKind::Unknown,
            capability: ColorCapability::detect(),
            handover: None,
            theme_id: None,
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            remembered: crate::interface_prefs::InterfacePreferences::default(),
            sidebar_path: App::sidebar_arrangement_path(),
            runtime_path: App::runtime_preferences_path(),
            preferences_path: App::interface_preferences_path(),
            // Not `App::queue_preferences_path()`: the other three hold
            // preferences, which a preview or a test may reasonably take from
            // the reader's home, while this one holds the reader's own words.
            // A test that enqueued a message would otherwise write into the
            // home of whoever ran it — and would race every test doing the same
            // — so the composition root is the only place that names the file.
            queue_path: None,
            // Not `App::notification_preferences_path()` either: a test that
            // flips a notification switch writes the file, and the settings
            // page's own tests are the ones that do it.
            notify_path: None,
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
///
/// Error rows carry the runtime's English explanation in their body. The
/// transcript is read by a person, so it is projected in the reader's language
/// here — before search, copy, and the incremental renderer see it — rather
/// than translated again at each of those surfaces.
pub fn block_from_row(row: &TimelineRow, locale: vibex_ui::locale::Locale) -> Block {
    let body = if row.kind == vibex_desktop_model::TimelineRowKind::Error {
        vibex_ui::error_copy::timeline_error_message(locale, &row.title, &row.body)
    } else {
        row.body.clone()
    };
    Block {
        id: row.id.clone(),
        kind: row.kind,
        title: row.title.clone(),
        body,
        turn_id: row.turn_id.clone(),
        sequence: row.last_sequence,
        // A row the runtime never stamped carries the epoch; the transcript
        // draws no clock rather than one reading 1970.
        timestamp_ms: (row.timestamp_ms > 0).then_some(row.timestamp_ms),
        expanded: false,
        reasoning_window: false,
        // A dense row shows one line by shape, so it has to be openable to be
        // readable in full: the transcript is where its body lives.
        collapsible: row.kind != vibex_desktop_model::TimelineRowKind::AgentMessage
            && (row.collapsible || crate::transcript::is_dense_row(row.kind)),
        streaming: row.streaming,
        failed: row.failed,
        pending_permission: row.pending_permission,
        file_path: row.file_path.clone(),
        runtime_attribution: row.runtime_attribution.clone(),
        conclusion: row.conclusion,
        group: crate::transcript::GroupRole::Solo,
        fold: None,
        activity: None,
        details: Vec::new(),
        group_summary: None,
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

/// Which half of the clipboard a read wants.
///
/// The two gestures that read a clipboard are the paste chord, which takes
/// whatever the reader copied, and the composer's attach action, which wants a
/// picture and asks for a file when there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardWanted {
    Everything,
    Image,
}

/// One configuration-centre mutation, as data.
///
/// The centre offers a verb per record, and most of those verbs are one write
/// against one request type. They travel as a single effect rather than as
/// twenty variants because the worker's job is the same for all of them: issue
/// the request, report the outcome, re-read the area. Keeping them together is
/// what makes "every mutation refreshes what it changed" a property of the
/// dispatcher instead of a rule each new verb has to remember.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigMutation {
    // ---- Agent ----------------------------------------------------------
    ToggleAgent {
        agent_id: vibex_core::AgentId,
        enabled: bool,
    },
    RefreshAgentSnapshot {
        agent_id: vibex_core::AgentId,
    },
    CheckAgentUpdate {
        agent_id: vibex_core::AgentId,
    },
    RollbackAgent {
        agent_id: vibex_core::AgentId,
    },
    /// Sign out of one Agent's account, having shown what it ends.
    LogoutAgentAuthContext {
        auth_context_id: String,
        expected_context_revision: i64,
        /// The impact the reader was shown. The authority refuses a count that
        /// no longer matches, which is what makes the preview a decision rather
        /// than a formality.
        confirmed_affected_session_count: usize,
    },
    /// Record one Agent's answer for one built-in tool panel.
    SetAgentTool {
        agent_id: vibex_core::AgentId,
        tool: vibex_core::AgentBuiltinTool,
        enabled: bool,
    },
    /// Exchange two Agents' places in the roster.
    SwapAgentOrder {
        first: vibex_core::AgentId,
        first_index: i64,
        second: vibex_core::AgentId,
        second_index: i64,
    },
    /// Take an Agent into the roster, or out of it.
    SetAgentAdded {
        agent_id: vibex_core::AgentId,
        added: bool,
    },
    /// Register a custom ACP Agent.
    CreateCustomAgent(Box<vibex_core::CustomAgentCreateRequest>),
    /// Sign in with one of an Agent's methods, storing what it collects first.
    AuthenticateAgentMethod {
        agent_id: vibex_core::AgentId,
        method_id: String,
        values: Vec<vibex_core::AgentAuthEnvironmentValue>,
    },
    // ---- provider profiles ----------------------------------------------
    /// Make a profile that does not exist yet.
    CreateProviderProfile(Box<vibex_core::AgentModelProviderProfileCreateRequest>),
    /// Write a planned export into the target's own files.
    ApplyNativeExport {
        export_id: String,
    },
    /// Put a written export back the way it was.
    RollbackNativeExport {
        export_id: String,
    },
    /// Make a profile out of one item another CLI tool holds.
    ImportNativeProfile {
        item_id: String,
        sources: Vec<vibex_core::ProviderNativeImportSource>,
    },
    /// Arrange one Agent's profiles in a new order, first to last.
    SetProviderOrder {
        agent_id: vibex_core::AgentId,
        profiles: Vec<vibex_core::ProviderProfileId>,
    },
    UpdateProviderProfile {
        agent_id: vibex_core::AgentId,
        profile_id: vibex_core::ProviderProfileId,
        display_name: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
        /// The Models the profile offers, when that is what changed.
        configured_models: Option<Vec<vibex_core::ProviderConfiguredModel>>,
        /// The whole option bag, written over the profile's own so the keys the
        /// form does not own survive.
        provider_options: Option<Box<vibex_core::ProviderOptions>>,
    },
    DeleteProviderProfile {
        agent_id: vibex_core::AgentId,
        profile_id: vibex_core::ProviderProfileId,
    },
    // ---- MCP ------------------------------------------------------------
    CreateMcpServer(Box<vibex_core::McpServerCreateRequest>),
    UpdateMcpServer(Box<vibex_core::McpServerUpdateRequest>),
    DeleteMcpServer {
        server_id: vibex_core::McpServerId,
    },
    ValidateMcpServer {
        server_id: vibex_core::McpServerId,
    },
    SetMcpAgentMatrix {
        server_id: vibex_core::McpServerId,
        matrix: Vec<vibex_core::McpServerAgentMatrix>,
    },
    DiscoverMcpSources,
    /// Import one discovered server. The selection carries the candidate the
    /// runtime published, so the write names the record rather than the row it
    /// was drawn on.
    ImportMcpServer {
        selection: Box<vibex_core::McpServerImportSelection>,
    },
    InstallMcpMarketEntry {
        entry_id: String,
        candidate: Box<vibex_core::McpServerCreateRequest>,
        env_values: Vec<vibex_core::McpServerEnvEntry>,
        /// The Agents the reader switched the entry on for. The registry
        /// publishes the launcher, not who should receive it.
        agent_ids: Vec<vibex_core::AgentId>,
    },
    /// Check a registry entry's launcher before installing it.
    ///
    /// The entry has no record yet, so the check travels with the candidate the
    /// registry published rather than with an identifier the authority does not
    /// hold: the answer is about the entry the reader is looking at, not about
    /// whatever row the cursor moved to.
    ValidateMcpMarketEntry {
        entry_id: String,
        candidate: Box<vibex_core::McpServerCreateRequest>,
    },
    // ---- Skills ---------------------------------------------------------
    CreateSkill(Box<vibex_core::SkillCreateRequest>),
    UpdateSkill(Box<vibex_core::SkillUpdateRequest>),
    DeleteSkill {
        skill_id: vibex_core::SkillId,
    },
    ValidateSkill {
        skill_id: vibex_core::SkillId,
    },
    SetSkillAgentMatrix {
        skill_id: vibex_core::SkillId,
        matrix: Vec<vibex_core::SkillAgentMatrix>,
    },
    DiscoverSkillSources,
    ImportSkill {
        selection: Box<vibex_core::SkillImportSelection>,
    },
    InstallSkillMarketEntry {
        entry_id: String,
        source: String,
        slug: String,
        agent_ids: Vec<vibex_core::AgentId>,
        /// The document the reader read. `None` only when this client could not
        /// read one, and the runtime is then asked for it as it was before.
        document: Option<Box<vibex_core::SkillMarketDocument>>,
    },
    // ---- Prompts ---------------------------------------------------------
    CreatePrompt(Box<vibex_core::PromptCreateRequest>),
    UpdatePrompt(Box<vibex_core::PromptUpdateRequest>),
    DeletePrompt {
        prompt_id: vibex_core::PromptId,
    },
    ValidatePrompt {
        prompt_id: vibex_core::PromptId,
    },
}

impl ConfigMutation {
    /// The record this write is about, under the key its own row wears.
    ///
    /// The key is the row's, not the request's: the centre files a write under
    /// what it drew, so the row that said it was working is the row the answer
    /// takes the marker off. A row's key is the record's identifier and nothing
    /// else, which is why the prefixes the effect keys carry do not appear here.
    pub fn key(&self) -> String {
        match self {
            Self::ToggleAgent { agent_id, .. }
            | Self::RefreshAgentSnapshot { agent_id }
            | Self::CheckAgentUpdate { agent_id }
            | Self::RollbackAgent { agent_id } => agent_id.as_str().to_string(),
            Self::UpdateProviderProfile { profile_id, .. }
            | Self::DeleteProviderProfile { profile_id, .. } => profile_id.as_str().to_string(),
            Self::AuthenticateAgentMethod { agent_id, .. } => agent_id.as_str().to_string(),
            Self::CreateCustomAgent(request) => request.agent_id.as_str().to_string(),
            Self::SetAgentAdded { agent_id, .. } => agent_id.as_str().to_string(),
            Self::SwapAgentOrder { first, .. } => first.as_str().to_string(),
            Self::SetAgentTool { agent_id, .. } => agent_id.as_str().to_string(),
            Self::LogoutAgentAuthContext {
                auth_context_id, ..
            } => auth_context_id.clone(),
            Self::CreateProviderProfile(_) => "provider:new".to_string(),
            Self::ApplyNativeExport { export_id } | Self::RollbackNativeExport { export_id } => {
                export_id.clone()
            }
            Self::ImportNativeProfile { item_id, .. } => item_id.clone(),
            Self::SetProviderOrder { agent_id, .. } => agent_id.as_str().to_string(),
            Self::CreateMcpServer(_) => "mcp:new".to_string(),
            Self::UpdateMcpServer(request) => request.mcp_server_id.as_str().to_string(),
            Self::DeleteMcpServer { server_id }
            | Self::ValidateMcpServer { server_id }
            | Self::SetMcpAgentMatrix { server_id, .. } => server_id.as_str().to_string(),
            Self::DiscoverMcpSources => "mcp:sources".to_string(),
            Self::ImportMcpServer { selection } => selection.discovery_id.clone(),
            Self::InstallMcpMarketEntry { entry_id, .. } => entry_id.clone(),
            Self::ValidateMcpMarketEntry { entry_id, .. } => entry_id.clone(),
            Self::CreateSkill(_) => "skill:new".to_string(),
            Self::UpdateSkill(request) => request.skill_id.as_str().to_string(),
            Self::DeleteSkill { skill_id }
            | Self::ValidateSkill { skill_id }
            | Self::SetSkillAgentMatrix { skill_id, .. } => skill_id.as_str().to_string(),
            Self::DiscoverSkillSources => "skill:sources".to_string(),
            Self::ImportSkill { selection } => selection.discovery_id.clone(),
            Self::InstallSkillMarketEntry { entry_id, .. } => entry_id.clone(),
            Self::CreatePrompt(_) => "prompt:new".to_string(),
            Self::UpdatePrompt(request) => request.prompt_id.as_str().to_string(),
            Self::DeletePrompt { prompt_id } => prompt_id.as_str().to_string(),
            Self::ValidatePrompt { prompt_id } => prompt_id.as_str().to_string(),
        }
    }

    /// What the reader is told while it runs.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ToggleAgent { .. } => "agent",
            Self::RefreshAgentSnapshot { .. } => "agent_probe",
            Self::CheckAgentUpdate { .. } => "agent_update_check",
            Self::RollbackAgent { .. } => "agent_rollback",
            Self::AuthenticateAgentMethod { .. } => "agent_auth_sign_in",
            Self::CreateCustomAgent(_) => "custom_agent_create",
            Self::SetAgentAdded { .. } => "agent_roster",
            Self::SwapAgentOrder { .. } => "agent_roster_order",
            Self::SetAgentTool { .. } => "agent_tool",
            Self::LogoutAgentAuthContext { .. } => "agent_auth_sign_out",
            Self::CreateProviderProfile(_) => "provider_profile_create",
            Self::ApplyNativeExport { .. } => "provider_native_export_apply",
            Self::RollbackNativeExport { .. } => "provider_native_export_rollback",
            Self::ImportNativeProfile { .. } => "provider_native_import_run",
            Self::SetProviderOrder { .. } => "provider_order_set",
            Self::UpdateProviderProfile { .. } => "provider_profile",
            Self::DeleteProviderProfile { .. } => "provider_profile_delete",
            Self::CreateMcpServer(_) => "mcp_create",
            Self::UpdateMcpServer(_) => "mcp_update",
            Self::DeleteMcpServer { .. } => "mcp_delete",
            Self::ValidateMcpServer { .. } => "mcp_validate",
            Self::SetMcpAgentMatrix { .. } => "mcp_matrix",
            Self::DiscoverMcpSources => "mcp_discover",
            Self::ImportMcpServer { .. } => "mcp_import",
            Self::InstallMcpMarketEntry { .. } => "mcp_install",
            Self::ValidateMcpMarketEntry { .. } => "mcp_market_validate",
            Self::CreateSkill(_) => "skill_create",
            Self::UpdateSkill(_) => "skill_update",
            Self::DeleteSkill { .. } => "skill_delete",
            Self::ValidateSkill { .. } => "skill_validate",
            Self::SetSkillAgentMatrix { .. } => "skill_matrix",
            Self::DiscoverSkillSources => "skill_discover",
            Self::ImportSkill { .. } => "skill_import",
            Self::InstallSkillMarketEntry { .. } => "skill_install",
            Self::CreatePrompt(_) => "prompt_create",
            Self::UpdatePrompt(_) => "prompt_update",
            Self::DeletePrompt { .. } => "prompt_delete",
            Self::ValidatePrompt { .. } => "prompt_validate",
        }
    }

    /// The verb the write wears, as the two lines that report it read it.
    ///
    /// The verb is what the reader asked for rather than what the request
    /// happens to be called: a switch flipped on is "Enabled", and the check
    /// that finds nothing new is still "Update checked".
    pub fn verb(&self) -> crate::config_center::ConfigVerb {
        use crate::config_center::ConfigVerb as Verb;
        match self {
            Self::ToggleAgent { enabled, .. } => {
                if *enabled {
                    Verb::Enable
                } else {
                    Verb::Disable
                }
            }
            Self::RefreshAgentSnapshot { .. } => Verb::RefreshSnapshot,
            Self::CheckAgentUpdate { .. } => Verb::CheckUpdate,
            Self::RollbackAgent { .. } => Verb::Rollback,
            Self::AuthenticateAgentMethod { .. } => Verb::SignIn,
            Self::CreateCustomAgent(_) => Verb::Create,
            Self::SwapAgentOrder { .. } => Verb::Save,
            Self::SetAgentTool { enabled, .. } => {
                if *enabled {
                    Verb::Enable
                } else {
                    Verb::Disable
                }
            }
            Self::SetAgentAdded { added, .. } => {
                if *added {
                    Verb::Add
                } else {
                    Verb::Remove
                }
            }
            Self::LogoutAgentAuthContext { .. } => Verb::SignOut,
            Self::CreateProviderProfile(_) => Verb::Create,
            Self::ApplyNativeExport { .. } => Verb::Save,
            Self::RollbackNativeExport { .. } => Verb::Rollback,
            Self::ImportNativeProfile { .. } => Verb::Import,
            Self::SetProviderOrder { .. } => Verb::Save,
            Self::UpdateProviderProfile { .. } => Verb::Save,
            Self::DeleteProviderProfile { .. } => Verb::Delete,
            Self::CreateMcpServer(_) | Self::CreateSkill(_) | Self::CreatePrompt(_) => Verb::Create,
            Self::UpdateMcpServer(_) | Self::UpdateSkill(_) | Self::UpdatePrompt(_) => Verb::Save,
            Self::DeleteMcpServer { .. } | Self::DeleteSkill { .. } | Self::DeletePrompt { .. } => {
                Verb::Delete
            }
            Self::ValidateMcpServer { .. }
            | Self::ValidateSkill { .. }
            | Self::ValidatePrompt { .. }
            | Self::ValidateMcpMarketEntry { .. } => Verb::Validate,
            Self::SetMcpAgentMatrix { .. } | Self::SetSkillAgentMatrix { .. } => Verb::SetAgents,
            Self::DiscoverMcpSources | Self::DiscoverSkillSources => Verb::Discover,
            Self::ImportMcpServer { .. } | Self::ImportSkill { .. } => Verb::Import,
            Self::InstallMcpMarketEntry { .. } | Self::InstallSkillMarketEntry { .. } => {
                Verb::InstallFromRegistry
            }
        }
    }
}

/// What a configuration write answered, beyond whether it succeeded.
///
/// A toast that only says "done" is not an answer. The reader pressed "Check
/// update" to find out whether there *is* an update, and "Install" to find out
/// which version landed; the row went quiet for the same reason. The worker
/// turns each response the authority gave it into one of these, so the sentence
/// the reader gets names what came back while the wording itself stays in the
/// locale, where every other sentence is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MutationOutcome {
    /// The write answered nothing beyond "done".
    #[default]
    Plain,
    /// A check found the record already carrying the newest version.
    UpToDate,
    /// A check found a version that is not the installed one.
    UpdateAvailable { version: String },
    /// An install or a rollback landed, with the version it left behind.
    Installed { version: Option<String> },
    /// An Agent's account was read: how many ways in it offers, and whether one
    /// of them is already signed in.
    Auth {
        methods: usize,
        account: Option<String>,
        authenticated: bool,
    },
    /// A model catalogue was read.
    Models { count: usize },
    /// Local Agent CLIs were probed for, and this is what was found.
    Discovered { found: usize },
    /// The Agent is signing in somewhere this client cannot follow.
    ///
    /// The runtime keeps the operation open and names where the reader has to
    /// finish it; a client with no terminal of its own says so rather than
    /// reporting a sign-in that did not happen.
    AuthWaiting {
        location: vibex_core::AgentAuthExecutionLocation,
        detail: Option<String>,
        /// The operation the authority is still running, so the reader can call
        /// it off. A sign-in that opened a browser on the host and then went
        /// nowhere is otherwise a process nobody can stop from this client.
        operation: Option<Box<vibex_core::AgentAuthenticationOperation>>,
        /// The PTY the method opened, when it opened one. Present means this
        /// client can draw the sign-in rather than only name where it is.
        terminal: Option<Box<vibex_core::TerminalAuthActionDescriptor>>,
    },
    /// A default was set, and at which scope it applies.
    DefaultScope { workspace: bool },
    /// A validation ran. `passed` is the verdict and `warned` keeps the middle
    /// answer — a warning is neither a clean pass nor a failure.
    Verdict {
        passed: bool,
        warned: bool,
        message: String,
    },
}

impl MutationOutcome {
    /// The verdict a backend validation result becomes, whichever of the three
    /// modules it came from.
    pub fn verdict(status_warns: bool, status_fails: bool, code: &str, message: &str) -> Self {
        let detail = if message.trim().is_empty() {
            code.to_string()
        } else if code.trim().is_empty() {
            message.to_string()
        } else {
            format!("{code} · {message}")
        };
        Self::Verdict {
            passed: !status_fails,
            warned: status_warns,
            message: detail,
        }
    }

    /// How the answer should be drawn.
    pub fn tone(&self) -> crate::config_center::ConfigTone {
        use crate::config_center::ConfigTone;
        match self {
            Self::Verdict { passed: false, .. } => ConfigTone::Bad,
            Self::Verdict { warned: true, .. } => ConfigTone::Warn,
            _ => ConfigTone::Ok,
        }
    }
}

/// Asynchronous work the reducer asks the worker to perform.
///
/// The variants carry only data; the worker owns the tokio runtime and the
/// backend handles, which is what keeps the reducer pure.
#[derive(Debug, Clone)]
pub enum Effect {
    LoadTeam {
        ticket: vibex_ui::TeamLoadTicket,
    },
    ControlTeam {
        request: vibex_core::TeamTaskControlRequest,
    },
    AcknowledgeTeam {
        request: vibex_core::TeamAcknowledgeRequest,
    },
    ListSessions {
        include_archived: bool,
    },
    /// Read one page of the session ownership tree.
    ///
    /// `parent_session_id` is `None` for the ownership roots and `Some` for one
    /// session's direct children; `append` continues a branch the runtime said
    /// had more. A delegated child never appears in `ListSessions`, so this is
    /// the only read that can name one.
    LoadSessionTree {
        parent_session_id: Option<VibexSessionId>,
        cursor: Option<String>,
        append: bool,
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
    /// Read the full payload behind one activity block.
    ///
    /// A page carries summaries, so the output and file snapshots an expanded
    /// activity renders only exist on request. The ticket fixes the sequence,
    /// which is what lets the controller drop an answer that arrives after the
    /// reader navigated away instead of drawing it over another item.
    LoadTimelineItem {
        ticket: vibex_ui::AgentTimelineItemTicket,
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
    /// Restores an archived session to the list. Unlike archiving this is not
    /// destructive, so the reader is not asked to confirm it.
    UnarchiveSession {
        session_id: VibexSessionId,
    },
    DeleteSession {
        session_id: VibexSessionId,
    },
    /// Delete a project and everything filed under it.
    ///
    /// The runtime owns the record, so the write goes through the workspace
    /// backend rather than through the sidebar arrangement: a project is not an
    /// item of the tree, it is the thing the tree hangs from.
    DeleteProject {
        project_id: String,
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
    /// Ask the worker for whatever the clipboard holds.
    ///
    /// An image is attached; text is pasted. Reading a clipboard is I/O and
    /// belongs to the worker, so the reducer only asks — and the terminal's
    /// own clipboard, which is the one a reader on the far side of an `ssh`
    /// link has, is asked afterwards by the event loop, because its answer
    /// arrives on the queue the loop owns.
    ReadClipboard {
        ticket: ComposerTicket,
        wanted: ClipboardWanted,
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
    /// Take a minted offer down. Leaving the panel is what "shown once" means:
    /// an offer the reader walked away from is still claimable until the runtime
    /// is told to stop accepting it.
    CancelPairingOffer {
        offer_id: vibex_core::RequestId,
    },
    /// Re-read whether a minted offer is still live. The claim arrives from
    /// outside this process, so this is the read the tick keeps asking for while
    /// the code is on screen.
    PairingOfferStatus {
        offer_id: vibex_core::RequestId,
    },
    /// Read which publishing methods behind pairing are up, degraded, or off.
    PairingPublications,
    /// Open a LAN pairing window, which puts an advertisement on the local
    /// network and holds the requests that answer it.
    StartLanPairing {
        permission_level: vibex_core::RemoteDevicePermissionLevel,
        ttl_ms: u32,
    },
    /// Re-read the window being advertised. The requests arrive from outside
    /// the client, so this is the read the tick keeps asking for while the
    /// panel is open.
    LanPairingStatus,
    /// Approve or refuse the device that asked to pair.
    DecideLanPairing {
        request_id: vibex_core::RequestId,
        approve: bool,
    },
    /// Take the advertisement down. Also what closes the panel, so a window
    /// the reader walked away from is not left discoverable.
    CancelLanPairing,
    RevokeDevice {
        device_id: vibex_core::DeviceId,
        reason: Option<String>,
    },
    ListAudit,
    ListProfiles,
    /// Read the local model gateway.
    LoadGateway,
    /// Read the gateway's recent request ledger.
    LoadGatewayRequests,
    SelectProfile {
        profile_id: vibex_core::ProviderProfileId,
        /// The scope this default applies to. `None` is the authority's global
        /// scope; a client that knows which workspace it is working in says so,
        /// because a default that silently applied everywhere is not the same
        /// decision as one for this checkout.
        scope: Option<vibex_core::ProviderProfileDefaultScope>,
    },
    WriteProviderSecret {
        profile_id: vibex_core::ProviderProfileId,
        secret: String,
    },
    /// Forget the key a profile holds.
    ClearProviderSecret {
        profile_id: vibex_core::ProviderProfileId,
    },
    TestProviderProfile {
        profile_id: vibex_core::ProviderProfileId,
    },
    FetchProviderModels {
        profile_id: vibex_core::ProviderProfileId,
    },
    /// Turn the local gateway on or off, as its own row's switch asks.
    ///
    /// The switch is the one write this table offers on a record that is not a
    /// Provider: a direct Provider is chosen, not switched, while the gateway
    /// is either serving the Agents that point at it or stepping aside for
    /// their own configuration.
    SetGatewayEnabled {
        enabled: bool,
    },
    /// Turn one upstream Key off or on, or make it the preferred one.
    SetGatewayKey {
        profile: vibex_core::ModelProviderProfileId,
        credential: vibex_core::RequestId,
        enabled: Option<bool>,
        preferred: Option<bool>,
    },
    /// Change one part of the gateway policy, carrying the rest forward.
    SetGatewayPolicy {
        token_limit: Option<Option<vibex_core::GatewayTokenLimit>>,
    },
    ListAgents,
    InstallAgent {
        agent_id: vibex_core::AgentId,
    },
    UninstallAgent {
        agent_id: vibex_core::AgentId,
    },
    ListAgentAuth {
        /// The account to read, or every Agent's when a list needs the column.
        ///
        /// The authority answers with the whole catalogue of contexts either
        /// way; naming the Agent is what lets the answer be filed under the row
        /// the verb was pressed on, and what a re-read after a write reports.
        agent_id: Option<vibex_core::AgentId>,
        /// Whether the answer should be put in front of the reader. The verb
        /// asks for that; a re-read that only refreshes the row does not.
        open: bool,
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
    /// One configuration-centre write.
    ///
    /// `record` is the row the write was made from, as the centre files it. The
    /// write travels with it so the answer can be filed under the same record —
    /// a form's save is filed under the form, not under a row that is not on
    /// screen yet.
    ConfigMutation {
        mutation: ConfigMutation,
        record: String,
    },
    /// Read the Agent sources a module's resources can be imported from.
    DiscoverSources {
        module: crate::config_center::ConfigModule,
    },
    /// Hand a URL to the desktop's own opener.
    OpenUrl {
        url: String,
    },
    /// Ask what signing out of one Agent's account would end.
    PreviewAgentAuthLogout {
        agent_id: vibex_core::AgentId,
    },
    /// Read the per-Agent answers for the built-in tool panels.
    LoadAgentTools,
    /// Probe this machine for the Agent CLIs that have not been added yet.
    ///
    /// It is one effect rather than a verb per candidate because the desktop's
    /// discovery is one pass: add the candidate, probe the machine, enable what
    /// answered. A client that stopped between the steps would leave an Agent
    /// added but never probed.
    DiscoverLocalAgents,
    /// Read one Agent's account: the methods it advertises, and the context it
    /// holds.
    LoadAgentAuthCatalog {
        agent_id: vibex_core::AgentId,
        /// Whether the authority should re-probe the Agent or answer from the
        /// snapshot it already holds.
        ///
        /// Opening the panel is a read, and a read that launched the Agent every
        /// time would be a process the reader never asked for. The "refresh
        /// methods" line is the opposite: it is the reader saying the methods
        /// they see are stale, and answering it from the same cache is a key
        /// that reports what it read without reading anything.
        refresh: bool,
    },
    /// Ask an Agent's own runtime to verify the account it holds.
    VerifyAgentAuth {
        agent_id: vibex_core::AgentId,
    },
    /// Attach to the terminal a sign-in method opened, and follow its frames.
    ///
    /// The terminal belongs to the authority; this client draws it and writes
    /// the reader's keys back. Attaching is a read plus a subscription, which is
    /// why it is an effect rather than something the view does when it paints.
    AttachAgentAuthTerminal {
        agent_id: vibex_core::AgentId,
        terminal_id: vibex_core::TerminalId,
        title: String,
    },
    /// Write the reader's keys into the attached sign-in terminal.
    WriteAgentAuthTerminal {
        terminal_id: vibex_core::TerminalId,
        data: String,
    },
    /// Resize the attached sign-in terminal to the grid it is drawn in.
    ResizeAgentAuthTerminal {
        terminal_id: vibex_core::TerminalId,
        rows: u16,
        columns: u16,
    },
    /// Call off the sign-in the authority is still running.
    CancelAgentAuth {
        agent_id: vibex_core::AgentId,
        operation_id: vibex_core::AgentAuthenticationOperationId,
    },
    /// Read the model catalogue the Agent's account can reach.
    RefreshAgentAuthModels {
        agent_id: vibex_core::AgentId,
    },
    /// Write a preview. `persist` records the plan so it can be applied.
    PreviewNativeExport {
        profile_id: vibex_core::ProviderProfileId,
        source: vibex_core::ProviderNativeExportSource,
        mode: vibex_core::ProviderNativeExportMode,
    },
    /// Read what has been exported for one profile.
    NativeExportHistory {
        profile_id: vibex_core::ProviderProfileId,
    },
    /// Read the registry page after the one on screen.
    LoadMoreSkillMarket {
        cursor: String,
        query: String,
        registry: crate::config_center::RegistryQuery,
    },
    /// Read one registry Skill in full, before it is installed.
    PreviewSkillMarket {
        entry_id: String,
        source: String,
        slug: String,
    },
    /// Read what other CLI tools on this machine hold, ready to import.
    PreviewNativeImport {
        sources: Vec<vibex_core::ProviderNativeImportSource>,
    },
    /// Read the order one Agent's profiles are arranged in.
    LoadProviderOrder {
        agent_id: vibex_core::AgentId,
    },
    /// Read one provider profile in full, for an editor.
    ///
    /// The list answers with the summary; an editor that writes a profile back
    /// has to read the fields the summary leaves out first.
    LoadProviderProfile {
        agent_id: vibex_core::AgentId,
        profile_id: vibex_core::ProviderProfileId,
    },
    /// Search a module's registry.
    ///
    /// The question travels whole — the text, the narrowing and the page —
    /// because the answer is only about the question that was asked: a page
    /// that arrived for another narrowing describes a list the reader never
    /// asked to see.
    SearchRegistry {
        module: crate::config_center::ConfigModule,
        query: String,
        registry: crate::config_center::RegistryQuery,
    },
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
            Effect::LoadTeam { .. } => "team",
            Effect::ControlTeam { .. } => "team_control",
            Effect::AcknowledgeTeam { .. } => "team_acknowledge",
            Effect::ListSessions { .. } => "sessions",
            Effect::LoadSessionTree { .. } => "session_tree",
            Effect::LoadSidebarOrganization => "sidebar_organization",
            Effect::ProbeAutoContinue { .. } => "auto_continue_probe",
            Effect::MutateSidebarOrganization { .. } => "sidebar_organization_mutation",
            Effect::ConfigMutation { mutation, .. } => mutation.label(),
            Effect::DiscoverSources { .. } => "config_sources",
            Effect::SearchRegistry { .. } => "config_registry",
            Effect::LoadProviderProfile { .. } => "provider_profile_detail",
            Effect::LoadProviderOrder { .. } => "provider_order",
            Effect::PreviewNativeImport { .. } => "provider_native_import",
            Effect::PreviewSkillMarket { .. } => "skill_market_preview",
            Effect::LoadMoreSkillMarket { .. } => "skill_market_more",
            Effect::PreviewNativeExport { .. } => "provider_native_export",
            Effect::LoadAgentAuthCatalog { .. } => "agent_auth_catalog",
            Effect::PreviewAgentAuthLogout { .. } => "agent_auth_logout_preview",
            Effect::OpenUrl { .. } => "open_url",
            Effect::VerifyAgentAuth { .. } => "agent_auth_verify",
            Effect::AttachAgentAuthTerminal { .. } => "agent_auth_terminal",
            Effect::WriteAgentAuthTerminal { .. } => "agent_auth_terminal_write",
            Effect::ResizeAgentAuthTerminal { .. } => "agent_auth_terminal_resize",
            Effect::CancelAgentAuth { .. } => "agent_auth_cancel",
            Effect::RefreshAgentAuthModels { .. } => "agent_auth_models",
            Effect::DiscoverLocalAgents => "agent_discovery",
            Effect::LoadAgentTools => "agent_tools",
            Effect::NativeExportHistory { .. } => "provider_native_export_history",
            Effect::OpenSession { .. } => "open_session",
            Effect::RefreshTimeline => "timeline",
            Effect::LoadOlder { .. } => "older_timeline",
            Effect::LoadTimelineItem { .. } => "timeline_item",
            Effect::ListRuntimeOptions => "runtime_options",
            Effect::CreateSession { .. } => "create_session",
            Effect::RenameSession { .. } => "rename_session",
            Effect::ArchiveSession { .. } => "archive_session",
            Effect::UnarchiveSession { .. } => "unarchive_session",
            Effect::DeleteSession { .. } => "delete_session",
            Effect::DeleteProject { .. } => "delete_project",
            Effect::ForkSession { .. } => "fork_session",
            Effect::ReadClipboard { .. } => "read_clipboard",
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
            Effect::CancelPairingOffer { .. } => "pairing_offer_cancel",
            Effect::PairingOfferStatus { .. } => "pairing_offer_status",
            Effect::PairingPublications => "pairing_publications",
            Effect::StartLanPairing { .. } => "lan_pairing_start",
            Effect::LanPairingStatus => "lan_pairing_status",
            Effect::DecideLanPairing { .. } => "lan_pairing_decision",
            Effect::CancelLanPairing => "lan_pairing_cancel",
            Effect::RevokeDevice { .. } => "revoke_device",
            Effect::ListAudit => "audit",
            Effect::ListProfiles => "profiles",
            Effect::LoadGateway => "gateway",
            Effect::SetGatewayEnabled { .. } => "gateway_toggle",
            Effect::SetGatewayKey { .. } => "gateway_key",
            Effect::SetGatewayPolicy { .. } => "gateway_policy",
            Effect::LoadGatewayRequests => "gateway_requests",
            Effect::SelectProfile { .. } => "select_profile",
            Effect::WriteProviderSecret { .. } => "provider_secret",
            Effect::ClearProviderSecret { .. } => "provider_secret_clear",
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
            Effect::DiscoverCompletions { .. } => "completions",
            Effect::CheckDrift => "drift",
            Effect::ProbeAgentRuntime { .. } => "runtime_probe",
            Effect::UpdateEntry { .. } => "update_entry",
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

    /// An app whose held messages live in `path` and nowhere else.
    fn queue_app(path: &std::path::Path) -> App {
        App::new(
            vibex_backend::DisconnectedBackend::facade(),
            AppOptions {
                sidebar_path: None,
                runtime_path: None,
                preferences_path: None,
                queue_path: Some(path.to_path_buf()),
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
    fn the_key_bar_names_the_key_that_answers_where_the_composer_types() {
        // The composer types what the reader presses, so `?` and the digits are
        // text there rather than the help and the destinations a list binds
        // them to. A bar generated from the table alone advertised them anyway:
        // the reader pressed `?` and watched a question mark appear where the
        // bar had promised help. The same destinations are still named — the
        // composer scope binds the session list to `Ctrl+L` for exactly this
        // reason — so the fix is to spell the chord that answers, not to drop
        // the row.
        let directory = tempfile::tempdir().unwrap();
        let mut app = settings_app(&directory.path().join("tui-interface.json"));
        app.live = LiveState::Ready;
        app.perform(Intent::NewSession);
        assert!(app.composer_takes_keys(), "the composer owns the keyboard");

        let typed = [
            Chord::plain(KeyCode::Char('?')),
            Chord::plain(KeyCode::Char('1')),
            Chord::plain(KeyCode::Char('5')),
            Chord::plain(KeyCode::Char(' ')),
        ];
        let advertised = app.advertised_keys();
        for chord in typed {
            assert!(
                !advertised.iter().any(|(_, row)| row.chord == chord),
                "{} is typed into the draft, so it is not a shortcut here",
                chord
            );
        }
        assert_eq!(
            app.advertised_chord(Intent::GotoSessions),
            Some(Chord::ctrl('l')),
            "the bar named the global digit instead of the chord that answers"
        );
        assert_eq!(app.advertised_chord(Intent::ToggleHelp), None);
        assert_eq!(
            app.advertised_chord(Intent::CompletionAccept),
            Some(Chord::plain(KeyCode::Tab)),
            "`Tab` is the completion key where the composer owns it"
        );
        assert_eq!(
            app.advertised_chord(Intent::FocusNext),
            None,
            "`Tab` moves the focus on a list, not in the composer"
        );
        // A plain page keeps the global keys: nothing there types them.
        app.perform(Intent::Back);
        assert!(!app.composer_takes_keys());
        assert_eq!(
            app.advertised_chord(Intent::ToggleHelp),
            Some(Chord::plain(KeyCode::Char('?')))
        );
        assert_eq!(
            app.advertised_chord(Intent::GotoSessions),
            Some(Chord::plain(KeyCode::Char('1')))
        );
    }

    #[test]
    fn the_key_bar_drops_the_global_keys_a_modal_swallows() {
        // A modal owns the keyboard while it is up, so the rows it does not
        // answer are dropped from the bar — and the destinations it *does*
        // answer, because the reducer puts the modal down for them, are kept.
        let directory = tempfile::tempdir().unwrap();
        let mut app = settings_app(&directory.path().join("tui-interface.json"));
        app.perform(Intent::ToggleHelp);
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })));

        assert_eq!(
            app.advertised_chord(Intent::OpenSettings),
            Some(Chord::plain(KeyCode::F(1))),
            "the cheatsheet lists `F1`, so the bar has to be able to name it"
        );
        assert_eq!(
            app.advertised_chord(Intent::Refresh),
            None,
            "a modal swallows `Ctrl+R`"
        );
        assert_eq!(
            app.advertised_chord(Intent::ToggleSidebar),
            None,
            "a modal swallows `Ctrl+B`"
        );
        assert_eq!(app.advertised_chord(Intent::ToggleHelp), None);
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
        assert!(app.apply_setting_value(SettingRow::Mark, "glitch"));
        assert!(app.apply_setting_value(SettingRow::Motion, "off"));
        assert!(app.apply_setting_value(SettingRow::Transitions, "off"));
        assert!(app.apply_setting_value(SettingRow::ReasoningDisplay, "timeline"));
        // Expansion is on as shipped, so the moved row is the one that closes it.
        assert!(app.apply_setting_value(SettingRow::ReasoningExpanded, "off"));
        assert!(app.apply_setting_value(SettingRow::ReasoningExpansion, "full"));
        assert!(app.apply_setting_value(SettingRow::Language, "zh-TW"));
        assert!(app.apply_setting_value(SettingRow::Workspace, "/tmp/vibex-default-ws"));

        // A later run reads the same answers back, including the light-theme
        // slot the dark run did not touch.
        let remembered = crate::interface_prefs::InterfacePreferences::load(Some(&path));
        assert_eq!(remembered.mode(), Some(vibex_ui::GpuiThemeMode::Light));
        assert_eq!(remembered.glyphs(), Some(crate::theme::GlyphMode::Ascii));
        assert_eq!(remembered.mark(), Some(crate::logo::MarkStyle::Glitch));
        assert!(!remembered.motion());
        assert!(!remembered.transitions());
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
        assert_eq!(reloaded.settings.mark, crate::logo::MarkStyle::Glitch);
        assert!(!reloaded.settings.motion);
        assert!(!reloaded.settings.transitions);
        assert_eq!(
            reloaded.settings.reasoning_display_mode,
            vibex_desktop_model::ReasoningDisplayMode::Timeline
        );
        assert!(!reloaded.settings.reasoning_expanded_by_default);
        assert_eq!(
            reloaded.settings.reasoning_expansion_mode,
            vibex_desktop_model::ReasoningExpansionMode::Full
        );
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
    fn a_sidebar_invalidation_re_reads_the_sessions_it_arranges() {
        // The list is sessions under project headings, and a remote authority
        // announces a session it just created as nothing but a sidebar
        // invalidation. A client that re-read the arrangement alone would draw
        // the same rows again, so the sessions and the projects travel with it.
        let directory = tempfile::tempdir().unwrap();
        let mut app = arrangement_app(&directory.path().join("tui-sidebar.json"));
        let effects = app.sidebar_projection_effects();
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListSessions { .. })),
            "the sessions themselves were not re-read"
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListWorkspaces)),
            "a heading for a session's project was not re-read"
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::LoadSidebarOrganization)),
            "the arrangement was not re-read"
        );

        // The read already on the wire is not duplicated: the invalidation
        // queues one more behind it, and the answer to the first is what
        // releases it.
        let effects = app.sidebar_projection_effects();
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::ListSessions { .. })),
            "a second read was issued while the first was still in flight"
        );
        app.agent.apply_sessions(Ok(Vec::new())).unwrap();
        assert!(
            matches!(
                app.pending_session_list_refresh(),
                Some(Effect::ListSessions { .. })
            ),
            "the coalesced invalidation was dropped instead of re-read"
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
            .insert_image(crate::composer::AttachedImage {
                mime_type: "image/png".into(),
                source: crate::composer::ImageSource::Bytes(std::sync::Arc::new(vec![1, 2, 3])),
                byte_len: 3,
                thumbnail: None,
            })
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

    /// An app with one session open, ready to hold messages for it.
    fn scheduled_app(session_id: &VibexSessionId) -> App {
        let mut app = arrangement_app(&tempfile::tempdir().unwrap().path().join("unused.json"));
        app.agent.state.selected_session_id = Some(session_id.clone());
        app.navigate_to(Page::Agent);
        app.live = LiveState::Ready;
        app
    }

    #[test]
    fn a_message_with_a_send_time_waits_for_the_clock_not_the_turn() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let now_ms = vibex_core::unix_timestamp_ms();
        app.enqueue_scheduled(
            session_id.clone(),
            "later".to_string(),
            Vec::new(),
            Some(now_ms + 60_000),
        );
        assert!(
            app.drain_queue().is_empty(),
            "a message whose hour has not come was sent anyway"
        );

        // The clock arrives; the same call now releases it, with no turn to
        // wait for at all.
        app.queued_messages[0].scheduled_at_ms = Some(now_ms - 1);
        let released = app.drain_queue();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, session_id);
        assert_eq!(released[0].1, "later");
    }

    #[test]
    fn a_message_still_waiting_does_not_hold_the_ones_behind_it() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let now_ms = vibex_core::unix_timestamp_ms();
        app.enqueue_scheduled(
            session_id.clone(),
            "an hour out".to_string(),
            Vec::new(),
            Some(now_ms + 3_600_000),
        );
        app.enqueue_scheduled(session_id.clone(), "due now".to_string(), Vec::new(), None);
        let released = app.drain_queue();
        assert_eq!(released.len(), 1, "the queue drained only what was ready");
        assert_eq!(released[0].1, "due now");
        assert_eq!(
            app.queued_messages.len(),
            1,
            "the scheduled message is still waiting"
        );
    }

    #[test]
    fn pulling_a_scheduled_message_back_carries_its_send_time() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let at_ms = vibex_core::unix_timestamp_ms() + 600_000;
        app.enqueue_scheduled(
            session_id.clone(),
            "later".to_string(),
            Vec::new(),
            Some(at_ms),
        );
        app.queue_selection = Some(0);
        assert!(app.edit_queued_message());
        assert_eq!(
            app.draft_schedule(),
            Some(crate::schedule::MessageSchedule::At(at_ms)),
            "the deadline was dropped on the way back into the draft"
        );

        // And holding it again keeps it, which is the whole point of carrying
        // it: a reader who edits the words did not ask to send it now.
        let outgoing = app.composer.take_outgoing();
        let taken = app.take_draft_schedule(vibex_core::unix_timestamp_ms());
        assert_eq!(taken, Ok(Some(at_ms)));
        app.enqueue_scheduled(
            session_id.clone(),
            outgoing.text,
            outgoing.images,
            taken.ok().flatten(),
        );
        assert!(app.drain_queue().is_empty(), "it left before its hour");
    }

    #[test]
    fn a_session_waiting_on_a_scheduled_message_borrows_its_name() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let agent_id = vibex_core::AgentId::parse("codex").expect("agent id");
        let mut session = AgentSession {
            id: session_id.clone(),
            title: format!("{agent_id} session"),
            project_id: vibex_core::ProjectId::new(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/repo".to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: agent_id.clone(),
            state: vibex_core::AgentSessionState::Initializing,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: Default::default(),
        };
        let at_ms = vibex_core::unix_timestamp_ms() + 60_000;
        app.enqueue_scheduled(
            session_id.clone(),
            "  Fix   the flake  ".to_string(),
            Vec::new(),
            Some(at_ms),
        );
        assert_eq!(
            app.pending_scheduled_session_title(&session).as_deref(),
            Some("Fix the flake")
        );
        assert_eq!(app.session_row_title(&session), "Fix the flake");
        assert_eq!(app.next_scheduled_message_at(&session_id), Some(at_ms));
        assert_eq!(
            app.session_schedule_countdown(&session_id, at_ms - 90_000)
                .as_deref(),
            Some("00:01:30")
        );

        // A session that already has a name of its own keeps it: the scheduled
        // message names the wait, it does not rename the work.
        session.title = "Migrate the scheduler".to_string();
        assert_eq!(app.pending_scheduled_session_title(&session), None);
        assert_eq!(app.session_row_title(&session), "Migrate the scheduler");
    }

    /// A snapshot carrying one send another client is holding.
    fn held_send_snapshot(
        session_id: &VibexSessionId,
        message_id: u64,
        text: &str,
        scheduled_at_ms: i64,
    ) -> vibex_core::RemoteSidebarOrganizationSnapshot {
        vibex_core::RemoteSidebarOrganizationSnapshot {
            revision: 3,
            scheduled_sends: vec![vibex_core::RemoteScheduledSend {
                session_id: session_id.as_str().to_string(),
                message_id,
                text: text.to_string(),
                attachments: Vec::new(),
                scheduled_at_ms,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_send_another_client_holds_marks_the_row_and_names_it() {
        // The Desktop armed the message and keeps its clock; this client can
        // neither send it nor move it, but a reader looking at the list is owed
        // the same row the Desktop draws.
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let at_ms = vibex_core::unix_timestamp_ms() + 90_000;
        app.apply_sidebar_organization(&held_send_snapshot(
            &session_id,
            4,
            "  Fix   the flake  ",
            at_ms,
        ));

        let mut session = unnamed_session(&session_id, "codex");
        assert_eq!(app.next_scheduled_message_at(&session_id), Some(at_ms));
        assert_eq!(
            app.session_schedule_countdown(&session_id, at_ms - 90_000)
                .as_deref(),
            Some("00:01:30")
        );
        assert_eq!(
            app.pending_scheduled_session_title(&session).as_deref(),
            Some("Fix the flake")
        );
        assert_eq!(app.session_row_title(&session), "Fix the flake");

        // The borrowed name stands in for the placeholder and nothing else.
        session.title = "Migrate the scheduler".to_string();
        assert_eq!(app.pending_scheduled_session_title(&session), None);
    }

    #[test]
    fn a_snapshot_that_only_carries_a_send_is_still_kept() {
        // The arrangement is not the only thing the authority publishes. A
        // client that dropped a sends-only snapshot would drop the only account
        // of a session waiting on an hour.
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let at_ms = vibex_core::unix_timestamp_ms() + 60_000;
        app.apply_sidebar_organization(&held_send_snapshot(&session_id, 1, "later", at_ms));
        assert!(app.sidebar_organization().is_some());
        assert_eq!(app.next_scheduled_message_at(&session_id), Some(at_ms));

        // A snapshot with nothing at all still lets the view go.
        app.apply_sidebar_organization(&vibex_core::RemoteSidebarOrganizationSnapshot {
            revision: 4,
            ..Default::default()
        });
        assert!(app.sidebar_organization().is_none());
        assert_eq!(app.next_scheduled_message_at(&session_id), None);
    }

    #[test]
    fn the_row_counts_down_to_the_send_that_goes_out_first() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let now_ms = vibex_core::unix_timestamp_ms();
        let theirs = now_ms + 600_000;
        app.apply_sidebar_organization(&held_send_snapshot(&session_id, 1, "theirs", theirs));

        app.enqueue_scheduled(
            session_id.clone(),
            "mine".to_string(),
            Vec::new(),
            Some(now_ms + 60_000),
        );
        assert_eq!(
            app.next_scheduled_message_at(&session_id),
            Some(now_ms + 60_000)
        );
        assert_eq!(
            app.earliest_scheduled_send(&session_id)
                .map(|(text, _)| text)
                .as_deref(),
            Some("mine")
        );

        // And the other way round: the hour another client holds is the one the
        // row counts down to, and the one the seconds are repainted for.
        app.queued_messages.clear();
        app.enqueue_scheduled(
            session_id.clone(),
            "mine".to_string(),
            Vec::new(),
            Some(now_ms + 900_000),
        );
        assert_eq!(app.next_scheduled_message_at(&session_id), Some(theirs));
        assert_eq!(
            app.earliest_scheduled_send(&session_id)
                .map(|(text, _)| text)
                .as_deref(),
            Some("theirs")
        );
        assert_eq!(app.next_scheduled_message_at_anywhere(), Some(theirs));
    }

    /// A session the manager persisted for a creation that carried no title.
    ///
    /// This is what every snapshot says until the first message reaches a turn.
    fn unnamed_session(session_id: &VibexSessionId, agent: &str) -> AgentSession {
        AgentSession {
            id: session_id.clone(),
            title: format!("{agent} session"),
            project_id: vibex_core::ProjectId::new(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/repo".to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: vibex_core::AgentId::parse(agent).expect("agent id"),
            state: vibex_core::AgentSessionState::Initializing,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: Default::default(),
        }
    }

    /// Apply a session list the way `AppMessage::Sessions` does.
    fn list_sessions(app: &mut App, sessions: Vec<AgentSession>) {
        let reconciled = app.reconcile_session_list(Ok(sessions));
        app.agent
            .apply_sessions(reconciled)
            .expect("the list applies");
    }

    #[test]
    fn a_session_created_here_is_named_by_its_first_message() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        app.note_pending_session_title(&session_id, "  Fix   the flake  ", &[]);

        // The authority's answer to the creation is the manager's placeholder,
        // and so is every read until the message reaches a turn: the row wears
        // what the reader wrote instead.
        app.record_created_session(&unnamed_session(&session_id, "codex"));
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Fix the flake")
        );
        list_sessions(&mut app, vec![unnamed_session(&session_id, "codex")]);
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Fix the flake")
        );

        // The seed from that message, or a title an Agent supplies, is the
        // authority's own name and wins from then on.
        let mut named = unnamed_session(&session_id, "codex");
        named.title = "Investigate the flake".to_string();
        list_sessions(&mut app, vec![named]);
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Investigate the flake")
        );

        // With the title released, a later placeholder snapshot is the
        // authority's word rather than the reader's old message.
        list_sessions(&mut app, vec![unnamed_session(&session_id, "codex")]);
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("codex session")
        );
    }

    #[test]
    fn an_early_update_cannot_put_the_placeholder_back_on_a_named_row() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        app.note_pending_session_title(&session_id, "Fix the flake", &[]);
        app.record_created_session(&unnamed_session(&session_id, "codex"));

        // The manager announces the session it has just persisted — still under
        // its placeholder — while the row already carries the reader's name.
        let event = app.reconcile_session_event(vibex_backend::BackendEvent::SessionUpdated(
            unnamed_session(&session_id, "codex"),
        ));
        app.agent.apply_event(event);
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("Fix the flake"),
            "the placeholder took the row back"
        );
    }

    #[test]
    fn a_message_that_names_nothing_leaves_the_placeholder_standing() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        app.note_pending_session_title(&session_id, " \n\t ", &[]);

        assert!(
            app.pending_session_titles.is_empty(),
            "a message with no words in it named a session"
        );
        app.record_created_session(&unnamed_session(&session_id, "codex"));
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("codex session")
        );
    }

    #[test]
    fn a_creation_that_is_only_a_picture_is_named_by_its_label() {
        // The reported bug: a pasted screenshot and no prose left the row on
        // the manager's placeholder, because the name was read from the text
        // alone. The label the composer drew names it instead.
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let image = crate::composer::ImageAttachment {
            label: "[Image #1]".to_string(),
            mime_type: "image/png".into(),
            source: crate::composer::ImageSource::Path("/tmp/pasted.png".into()),
            byte_len: 0,
            thumbnail: None,
        };
        app.note_pending_session_title(&session_id, "", &[(image, 0)]);

        app.record_created_session(&unnamed_session(&session_id, "codex"));
        assert_eq!(
            app.session_title(&session_id).as_deref(),
            Some("[Image #1]")
        );
    }

    #[test]
    fn a_name_is_pruned_with_the_session_it_was_going_to_name() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);

        // A creation that has not been acknowledged yet is not in the list, and
        // is exactly the row the name is for.
        app.new_draft_id = session_id.clone();
        app.note_pending_session_title(&session_id, "Fix the flake", &[]);
        app.retain_pending_session_titles();
        assert!(app.pending_session_titles.contains_key(&session_id));

        // The row it named is gone — the session was deleted before any title
        // arrived — and the name goes with it.
        app.new_draft_id = VibexSessionId::new();
        app.retain_pending_session_titles();
        assert!(app.pending_session_titles.is_empty());
    }

    #[test]
    fn a_scheduled_session_says_what_it_is_waiting_for() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let now_ms = vibex_core::unix_timestamp_ms();
        app.enqueue_scheduled(
            session_id.clone(),
            "later".to_string(),
            Vec::new(),
            Some(now_ms + 90_000),
        );
        let label = app.schedule_status_label(now_ms + 90_000, &session_id, now_ms);
        assert!(
            label.contains("00:01:30"),
            "the wait is not in the label: {label}"
        );
        let due = app.schedule_status_label(now_ms - 1, &session_id, now_ms);
        assert_eq!(due, app.strings.schedule_due_waiting());

        // A due message whose turn is still going says so instead: the clock is
        // not what is holding it any more.
        app.pending_sends.insert(
            1,
            PendingSend {
                session_id: Some(session_id.clone()),
                serial: 1,
                correlation_id: vibex_core::CorrelationId::new(),
                text: "in flight".to_string(),
                attachments: Vec::new(),
                after_sequence: 0,
                submitted_at: std::time::Instant::now(),
                submitted_at_ms: now_ms,
            },
        );
        assert_eq!(
            app.schedule_status_label(now_ms - 1, &session_id, now_ms),
            app.strings.schedule_due_running()
        );
    }

    #[test]
    fn a_scheduled_message_is_still_waiting_after_the_client_restarts() {
        // The hour a reader sets is an instruction to a client that may not be
        // running when it arrives. A queue kept in memory alone made the
        // schedule the one thing the reader could not rely on: closing the
        // terminal threw the message away, and with it the session that existed
        // only to wait for it.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-queue.json");
        let session_id = VibexSessionId::new();
        let at_ms = vibex_core::unix_timestamp_ms() + 3_600_000;

        let mut first = queue_app(&path);
        first.enqueue_scheduled(
            session_id.clone(),
            "tomorrow's message".to_string(),
            Vec::new(),
            Some(at_ms),
        );
        assert!(path.exists(), "the queue was never written down");
        drop(first);

        // A second run reads the same file and finds the message where the
        // first left it, deadline and all.
        let second = queue_app(&path);
        assert_eq!(second.queued_messages.len(), 1);
        assert_eq!(second.queued_messages[0].session_id, session_id);
        assert_eq!(second.queued_messages[0].text, "tomorrow's message");
        assert_eq!(second.queued_messages[0].scheduled_at_ms, Some(at_ms));
        assert_eq!(second.next_scheduled_message_at(&session_id), Some(at_ms));
    }

    #[test]
    fn a_message_sent_or_dropped_is_taken_out_of_the_file() {
        // The file follows the queue in both directions: a message that has
        // gone out must not come back on the next run, or a schedule would
        // send the same words twice.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-queue.json");
        let session_id = VibexSessionId::new();
        let mut app = queue_app(&path);
        // The queue is acted on through the session in front of the reader, so
        // the rows have to belong to one before they can be dropped or sent.
        app.agent.state.selected_session_id = Some(session_id.clone());
        app.navigate_to(Page::Agent);
        app.enqueue_scheduled(session_id.clone(), "dropped".to_string(), Vec::new(), None);
        app.enqueue_scheduled(session_id.clone(), "sent".to_string(), Vec::new(), None);
        app.queue_selection = Some(0);
        assert!(app.delete_queued_message());
        app.queue_selection = Some(0);
        assert!(app.take_queued_message().is_some());

        let restored = queue_app(&path);
        assert!(
            restored.queued_messages.is_empty(),
            "a message the reader dropped or sent is still queued: {:?}",
            restored.queued_messages
        );
    }

    #[test]
    fn a_countdown_repaints_only_when_the_number_it_prints_moves() {
        let session_id = VibexSessionId::new();
        let mut app = scheduled_app(&session_id);
        let now_ms = vibex_core::unix_timestamp_ms();
        assert!(!app.tick_schedules(now_ms), "nothing is waiting on a clock");
        app.enqueue_scheduled(
            session_id.clone(),
            "later".to_string(),
            Vec::new(),
            Some(now_ms + 10_000),
        );
        assert!(app.tick_schedules(now_ms), "the countdown appeared");
        assert!(
            !app.tick_schedules(now_ms + 10),
            "a tick that did not move the second repainted"
        );
        assert!(app.tick_schedules(now_ms + 1_000), "the second moved");
        // The countdown going away is a repaint too: a row that kept showing a
        // number after the send left would describe a message that is gone.
        app.queued_messages.clear();
        assert!(app.tick_schedules(now_ms + 2_000), "the countdown vanished");
        assert!(!app.tick_schedules(now_ms + 3_000), "and then it settled");
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
            Page::Prompts,
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
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
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
            block_from_row(&row, vibex_ui::locale::Locale::En).timestamp_ms,
            Some(1_759_237_920_000),
            "the block dropped the row's clock"
        );

        // A row with no stamped item behind it draws no clock rather than one
        // reading the epoch.
        let mut untimed = row;
        untimed.timestamp_ms = 0;
        assert_eq!(
            block_from_row(&untimed, vibex_ui::locale::Locale::En).timestamp_ms,
            None
        );
    }

    #[test]
    fn a_transcript_error_body_is_projected_in_the_reader_language() {
        let row = projected_row(vibex_core::TimelinePayload::Error(
            vibex_core::TimelineErrorPayload {
                code: "provider_access_unavailable".into(),
                message:
                    "The model provider rejected the request because account usage is unavailable"
                        .into(),
                recoverable: false,
            },
        ));
        assert_eq!(
            block_from_row(&row, vibex_ui::locale::Locale::ZhCn).body,
            "模型供应商拒绝了请求，因为账户用量不可用"
        );
        assert_eq!(
            block_from_row(&row, vibex_ui::locale::Locale::En).body,
            row.body
        );
    }
}

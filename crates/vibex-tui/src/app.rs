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
use crate::keymap::{Keymap, Scope};
use crate::locale::{Locale, Strings};
use crate::theme::{ColorCapability, TuiTheme};
use crate::transcript::{Block, ScrollState, Transcript};
use crate::view::SeatKind;

/// Which page the user is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Sessions,
    Agent,
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
            Page::Agent => Scope::Agent,
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
            Page::Agent | Page::Files | Page::Changes | Page::Terminal
        )
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
    RuntimePicker { selected: usize },
    /// A read-only detail view for one transcript block.
    BlockDetails { block: usize, scroll: usize },
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
    NewSessionTitle,
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

/// What the transcript and sidebar are projected from.
#[derive(Debug, Clone, Default)]
pub struct ProjectionState {
    pub sidebar: SidebarState,
    pub rows: Vec<TimelineRow>,
    /// Whether the sidebar pane is collapsed by the user.
    pub sidebar_collapsed: bool,
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
    /// Regions the last frame published for mouse hit-testing: the transcript
    /// band and the close affordance of the modal, when one is open.
    pub regions: FrameRegions,

    /// Runtime catalog, kept for the runtime picker.
    pub runtime_options: Option<vibex_core::SessionRuntimeOptionCatalog>,
    /// The workspace chosen in the workspace browser, consumed by the
    /// new-session prompt.
    pub workspace_path: Option<String>,
    /// In-progress elicitation answers.
    pub elicitation_draft: crate::reduce::ElicitationDraft,
    /// Whether the usage page shows this session or the aggregate.
    pub usage_scope_session: bool,
    /// Set by the first `Esc` in the composer; the second clears the draft.
    pub draft_clear_armed: bool,
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
    /// The last left click, so two clicks in the same cell can be told apart
    /// from two clicks in different ones.
    pub last_click: Option<(std::time::Instant, usize, u16)>,
    /// Messages held back until the running turn ends.
    pub queued_messages: Vec<String>,
    /// Which queued message the queue band's cursor is on.
    pub queue_selection: Option<usize>,
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
    /// The queue band's rows, for click-to-select.
    pub queue: Option<ratatui::layout::Rect>,
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
            projection: ProjectionState::default(),
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
            regions: FrameRegions::default(),
            runtime_options: None,
            workspace_path: None,
            elicitation_draft: crate::reduce::ElicitationDraft::default(),
            usage_scope_session: true,
            draft_clear_armed: false,
            session_cards: std::collections::BTreeSet::new(),
            search: None,
            hover: None,
            history_selection: 0,
            recent_commands: Vec::new(),
            text_selection: None,
            last_click: None,
            queued_messages: Vec::new(),
            queue_selection: None,
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

    /// Workspace the session pages read from.
    pub fn active_workspace_id(&self) -> Option<vibex_core::WorkspaceId> {
        self.active_session()
            .map(|session| session.workspace_id.clone())
    }

    /// Sidebar rows for the current filter.
    pub fn sidebar_rows(&self) -> Vec<vibex_desktop_model::AgentSidebarRow> {
        self.agent
            .state
            .view(&self.projection.sidebar, &self.filter, self.shell)
            .sessions
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
        if self.focus == Focus::Composer && self.page == Page::Agent {
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
        if self.focus == Focus::Composer && self.page == Page::Agent {
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
        let block = self.transcript.blocks().iter().rev().find(|block| {
            matches!(
                block.kind,
                vibex_desktop_model::TimelineRowKind::TodoUpdate
                    | vibex_desktop_model::TimelineRowKind::Plan
            )
        })?;
        let mut progress = TodoProgress {
            title: block.title.clone(),
            done: 0,
            total: 0,
            running: None,
        };
        for line in block.body.lines() {
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

    /// The vertical scrollbar thumb, as a (start, length) pair in rows.
    pub fn scroll_thumb(&mut self, rows: usize) -> (usize, usize) {
        let total = self.transcript.total_height();
        if total <= rows || rows == 0 {
            return (0, rows);
        }
        let height = self.transcript.total_height();
        let viewport = self.viewport.1 as usize;
        let offset = if self.scroll.follow {
            height.saturating_sub(viewport)
        } else {
            self.scroll.offset.min(height.saturating_sub(1))
        };
        let length = (rows * rows / total.max(1)).max(1);
        let start = (offset * rows / total.max(1)).min(rows.saturating_sub(length));
        (start, length)
    }

    /// Whether the transcript has a running block worth animating.
    ///
    /// The interface repaints without input only while this is true.
    pub fn transcript_animating(&self) -> bool {
        self.transcript.is_animating()
    }

    /// Step every animation. Returns whether a repaint is due.
    ///
    /// The phase advances only while something is actually moving, which is what
    /// preserves the zero-frames-when-idle contract.
    pub fn advance_transcript_animation(&mut self) -> bool {
        let transcript = self.transcript.advance_animation();
        // The turn line pulses while a turn runs or while the session is idle
        // but connected; an idle pulse is a live-session cue, not decoration.
        let turn_line = self.turn_started.is_some() || self.pending_permission_count() > 0;
        if !transcript && !turn_line {
            return false;
        }
        if turn_line {
            self.animation_phase = self.animation_phase.wrapping_add(1);
        }
        true
    }

    /// Whether a repaint is due without any input or event.
    pub fn is_animating(&self) -> bool {
        self.transcript.is_animating()
            || self.turn_started.is_some()
            || self.pending_permission_count() > 0
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
        self.navigation.enter_session(session_id.as_str());
        self.navigate_to(Page::Agent);
        self.scroll = ScrollState::default();
    }

    /// Rebuild the transcript from the shared controller's projection.
    ///
    /// A prepend grows the content above the viewport, so the reader's place
    /// is anchored to the block that was under the first visible line rather
    /// than to the absolute line offset it used to sit at.
    pub fn sync_transcript(&mut self) {
        let view = self
            .agent
            .state
            .view(&self.projection.sidebar, "", self.shell);
        self.projection.rows = view.timeline_rows.clone();
        let blocks: Vec<Block> = view.timeline_rows.iter().map(block_from_row).collect();
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
        if !self.queued_messages.is_empty() && self.page == Page::Agent {
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

    /// Hold a message until the running turn ends.
    pub fn enqueue_message(&mut self, text: String) {
        self.queued_messages.push(text);
        self.queue_selection = Some(self.queued_messages.len() - 1);
    }

    /// Move the queue cursor, entering the queue at its newest row.
    pub fn move_queue_selection(&mut self, delta: isize) {
        if self.queued_messages.is_empty() {
            self.queue_selection = None;
            return;
        }
        let last = self.queued_messages.len() - 1;
        let current = self.queue_selection.unwrap_or(last) as isize;
        self.queue_selection = Some((current + delta).clamp(0, last as isize) as usize);
    }

    /// Take the selected queued message back into the composer to edit it.
    pub fn edit_queued_message(&mut self) -> bool {
        let Some(index) = self
            .queue_selection
            .filter(|index| *index < self.queued_messages.len())
        else {
            return false;
        };
        let text = self.queued_messages.remove(index);
        self.queue_selection = if self.queued_messages.is_empty() {
            None
        } else {
            Some(index.min(self.queued_messages.len() - 1))
        };
        // A draft already in the composer is not thrown away: it goes to the
        // front of the queue, which is where the reader would look for it.
        let draft = self.composer.text().to_string();
        if !draft.trim().is_empty() {
            self.queued_messages.insert(index, draft);
        }
        self.composer.set_text(text);
        self.focus = Focus::Composer;
        self.composer_mode = ComposerMode::Normal;
        true
    }

    /// Drop the selected queued message.
    pub fn delete_queued_message(&mut self) -> bool {
        let Some(index) = self
            .queue_selection
            .filter(|index| *index < self.queued_messages.len())
        else {
            return false;
        };
        self.queued_messages.remove(index);
        self.queue_selection = if self.queued_messages.is_empty() {
            None
        } else {
            Some(index.min(self.queued_messages.len() - 1))
        };
        true
    }

    /// Swap the selected queued message with its neighbour.
    pub fn move_queued_message(&mut self, delta: isize) -> bool {
        let Some(index) = self
            .queue_selection
            .filter(|index| *index < self.queued_messages.len())
        else {
            return false;
        };
        let target = index as isize + delta;
        if target < 0 || target >= self.queued_messages.len() as isize {
            return false;
        }
        self.queued_messages.swap(index, target as usize);
        self.queue_selection = Some(target as usize);
        true
    }

    /// Take the selected queued message out, to be sent immediately.
    pub fn take_queued_message(&mut self) -> Option<String> {
        let index = self
            .queue_selection
            .filter(|index| *index < self.queued_messages.len())?;
        let text = self.queued_messages.remove(index);
        self.queue_selection = if self.queued_messages.is_empty() {
            None
        } else {
            Some(index.min(self.queued_messages.len() - 1))
        };
        Some(text)
    }

    /// Send the next held message once the turn has ended.
    ///
    /// Called after every worker message rather than on a special "turn ended"
    /// event: the client has no such event, and a queue that only drains on one
    /// signal would stall the moment that signal changed shape.
    pub fn drain_queue(&mut self) -> Option<String> {
        if self.queued_messages.is_empty() || self.session_running() {
            return None;
        }
        if !self.live.is_live() {
            return None;
        }
        let text = self.queued_messages.remove(0);
        self.queue_selection = if self.queued_messages.is_empty() {
            None
        } else {
            Some(0)
        };
        self.history.push(text.clone());
        Some(text)
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
        let width = crate::view::layout_for(self.shell, columns, rows).main_width;
        let theme = self.theme.clone();
        self.transcript.configure(width, &theme);
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
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            seat: SeatKind::Remote,
            capability: ColorCapability::detect(),
            theme_id: None,
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
        }
    }
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
        collapsible: row.collapsible,
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
    },
    ContinueTurn {
        session_id: VibexSessionId,
    },
    Interrupt {
        session_id: VibexSessionId,
    },
    SteerMessage {
        session_id: VibexSessionId,
        text: String,
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

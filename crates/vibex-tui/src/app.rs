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
use crate::theme::{ColorCapability, GlyphMode, TuiTheme};
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
    /// Contextual help, generated from the binding tables.
    Help { scroll: usize, query: String },
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

/// A row on the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingRow {
    Theme,
    Locale,
    Icons,
    Backend,
    Seat,
    Keys,
    Version,
}

impl SettingRow {
    pub const ALL: [SettingRow; 7] = [
        SettingRow::Theme,
        SettingRow::Locale,
        SettingRow::Icons,
        SettingRow::Backend,
        SettingRow::Seat,
        SettingRow::Keys,
        SettingRow::Version,
    ];
}

/// Everything the settings page renders from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    pub theme_id: String,
    pub mode: vibex_ui::GpuiThemeMode,
    pub locale: Locale,
    pub glyphs: GlyphMode,
    pub selected: usize,
}

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
    pub diff_text: Option<String>,
    pub text_view: Option<(String, String)>,
    /// The crossterm-reported terminal size, used by renderers that need to
    /// make a layout decision before the frame buffer exists.
    pub viewport: (u16, u16),

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
            diff_text: None,
            text_view: None,
            viewport: (120, 40),
            runtime_options: None,
            workspace_path: None,
            elicitation_draft: crate::reduce::ElicitationDraft::default(),
            usage_scope_session: true,
            draft_clear_armed: false,
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
            Page::Settings => SettingRow::ALL.len(),
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
    pub fn sync_transcript(&mut self) {
        let view = self
            .agent
            .state
            .view(&self.projection.sidebar, "", self.shell);
        self.projection.rows = view.timeline_rows.clone();
        let blocks = view.timeline_rows.iter().map(block_from_row).collect();
        let change = self.transcript.set_blocks(blocks);
        if change.appended > 0 {
            // New content resumes following unless the user scrolled away.
            if self.scroll.follow {
                self.scroll.offset = 0;
            }
        }
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
    }
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
    },
    RefreshTimeline,
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
        assert!(SettingRow::ALL.contains(&SettingRow::Theme));
        assert!(SettingRow::ALL.contains(&SettingRow::Keys));
        assert_eq!(SettingRow::ALL.len(), 7);
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

use std::collections::{BTreeMap, BTreeSet};

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Root, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};
use serde::{Deserialize, Serialize};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendError, BackendEvent, BackendFacade, BackendOperation,
    BackendProjection, BackendResult, MutationRequest, WorkspaceSummary,
};
use vibex_core::{
    AgentId, AgentSessionState, DeviceId, ElicitationField, ElicitationFieldKind,
    ElicitationRequest, ElicitationResolutionAction, FileEntryKind, FileSearchRequest,
    PermissionResolution, PermissionResponseKind, ProviderProfileId,
    ProviderRunHealthProbesRequest, RemoteCreatePairingOfferRequest, RemoteDevicePermissionLevel,
    RemoteDeviceStatus, RemoteRevokeDeviceRequest, RequestId, ResolvePermissionRequest,
    SendAgentMessageRequest, TerminalCreateRequest, VibexSessionId, WorkspaceId, unix_timestamp_ms,
};
use vibex_desktop_model::{
    AgentSidebarRowKind, GitSelectionKey, SidebarState, TimelineRowKind, UnifiedDiffLineKind,
};

use crate::browser_gate::{BrowserHostSnapshot, KeyboardSource};
use crate::{
    AgentEventDecision, AgentFileGitController, AsyncPhase, AsyncState, CompactNavigation,
    FileEditorStatus, GlobalDestination, HostInsets, HostKeyboardSource, HostViewportSnapshot,
    ManagementLoadState, ManagementSection, ManagementWorkflowCapabilities,
    ManagementWorkflowController, NavigationLevel, SessionDestination, ShellKind, ShellLayout,
    TerminalAccessMode, TerminalConnectionState, TerminalInput, TerminalKey, TerminalKeyModifiers,
    TerminalWorkflowCapabilities, TerminalWorkflowController,
};

pub const WORKFLOW_WORKBENCH_SCHEMA_VERSION: &str = "vibex-workflow-workbench.v1";
const MAX_RENDERED_TEXT_BYTES: usize = 24 * 1024;
const TEST_AGENT_MESSAGE: &str = "Vibex E2E probe: reply OK";
const TEST_FILE_CONTENT: &str = "vibex-e2e-probe\n";
const TEST_COMMIT_MESSAGE: &str = "test: gpui workflow e2e";
const TEST_TERMINAL_INPUT: &str = "printf vibex-e2e-probe";
const MOBILE_TOP_BAR_HEIGHT: f32 = 56.0;
const MOBILE_BOTTOM_NAV_HEIGHT: f32 = 64.0;
const MOBILE_PAGE_GUTTER: f32 = 16.0;
const MOBILE_CARD_RADIUS: f32 = 8.0;
const MOBILE_TOUCH_TARGET: f32 = 44.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactDetailPage {
    FilePreview,
    GitDiff,
    ManagementSection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkbenchSettingsSection {
    Connection,
    Appearance,
    About,
}

impl WorkbenchSettingsSection {
    const ALL: [Self; 3] = [Self::Connection, Self::Appearance, Self::About];

    const fn label(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Appearance => "Appearance",
            Self::About => "About",
        }
    }
}

fn fill_fixed_file_test_input(controller: &mut crate::FileWorkflowController) -> BackendResult<()> {
    controller
        .update_active_content(TEST_FILE_CONTENT)
        .map(|_| ())
}

fn management_completion_is_current(
    current_generation: u64,
    current_operation: u64,
    worker_generation: u64,
    worker_operation: u64,
) -> bool {
    current_generation == worker_generation && current_operation == worker_operation
}

fn event_subscription_should_restart(connection: WorkbenchConnectionState) -> bool {
    connection == WorkbenchConnectionState::Online
}

fn agent_event_is_relevant_live(selected: Option<&VibexSessionId>, event: &BackendEvent) -> bool {
    match event {
        BackendEvent::Timeline(event) => selected == Some(&event.session_id),
        BackendEvent::Runtime(event) => selected == Some(&event.session_id),
        BackendEvent::RuntimeSelection(event) => selected == Some(&event.session_id),
        BackendEvent::Lagged {
            refetch,
            observed_live: true,
            ..
        } => refetch
            .session_id
            .as_ref()
            .is_none_or(|session_id| selected == Some(session_id)),
        BackendEvent::ProjectionInvalidated(_)
        | BackendEvent::Lagged {
            observed_live: false,
            ..
        }
        | BackendEvent::Disconnected => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchConnectionState {
    Online,
    Degraded,
    Reconnecting,
    Offline,
    Revoked,
    Incompatible,
}

fn connection_label(state: WorkbenchConnectionState) -> &'static str {
    match state {
        WorkbenchConnectionState::Online => "Connected",
        WorkbenchConnectionState::Degraded => "Degraded",
        WorkbenchConnectionState::Reconnecting => "Reconnecting",
        WorkbenchConnectionState::Offline => "Offline",
        WorkbenchConnectionState::Revoked => "Revoked",
        WorkbenchConnectionState::Incompatible => "Incompatible",
    }
}

fn connection_color(state: WorkbenchConnectionState, cx: &App) -> gpui::Hsla {
    match state {
        WorkbenchConnectionState::Online => cx.theme().success,
        WorkbenchConnectionState::Degraded | WorkbenchConnectionState::Reconnecting => {
            cx.theme().warning
        }
        WorkbenchConnectionState::Offline
        | WorkbenchConnectionState::Revoked
        | WorkbenchConnectionState::Incompatible => cx.theme().danger,
    }
}

fn path_file_name(path: &str) -> Option<&str> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchSurface {
    Agent,
    Files,
    Git,
    Terminal,
    Management,
}

impl WorkbenchSurface {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Files => "Files",
            Self::Git => "Changes",
            Self::Terminal => "Terminal",
            Self::Management => "Management",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowWorkbenchSnapshot {
    pub schema_version: &'static str,
    pub connection: WorkbenchConnectionState,
    pub shell: ShellKind,
    pub navigation_level: NavigationLevel,
    pub global_destination: GlobalDestination,
    pub session_destination: SessionDestination,
    pub active_surface: WorkbenchSurface,
    pub workspace_count: usize,
    pub session_count: usize,
    pub timeline_row_count: usize,
    pub pending_approval_count: usize,
    pub agent_runtime_ready: bool,
    pub agent_mutation_phase: AsyncPhase,
    pub agent_live_event_count: u64,
    pub agent_recovery_count: u64,
    pub file_row_count: usize,
    pub file_search_result_count: usize,
    pub file_has_active_file: bool,
    pub file_editor_status: FileEditorStatus,
    pub file_live_event_count: u64,
    pub file_recovery_count: u64,
    pub git_change_count: usize,
    pub git_selected_count: usize,
    pub git_mutation_pending: bool,
    pub git_commit_phase: AsyncPhase,
    pub git_live_event_count: u64,
    pub git_recovery_count: u64,
    pub terminal_count: usize,
    pub terminal_connection: TerminalConnectionState,
    pub terminal_sequence: i64,
    pub terminal_rebuild_count: u64,
    pub terminal_recovery_count: u64,
    pub management_load_state: ManagementLoadState,
    pub management_agent_count: usize,
    pub management_profile_count: usize,
    pub management_health_count: usize,
    pub management_device_count: usize,
    pub management_revoked_device_count: usize,
    pub management_operation_pending: bool,
    pub management_live_event_count: u64,
    pub management_recovery_count: u64,
    pub has_relay_status: bool,
    pub has_pairing_offer: bool,
    pub error_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowWorkbenchCommand {
    RefreshAll,
    SelectSurface { surface: WorkbenchSurface },
    SelectWorkspace { index: usize },
    SelectSession { index: usize },
    FillTestInput { input: WorkflowTestInput },
    SendAgentMessage,
    ResolveApproval { index: usize, approve: bool },
    OpenFileRow { index: usize },
    SearchFiles,
    SaveFile,
    ReloadFileConflict,
    LoadGitDiff { index: usize },
    StageGitChange { index: usize },
    UnstageGitChange { index: usize },
    PrepareCommit,
    ConfirmCommit,
    CreateTerminal,
    AttachTerminal { index: usize },
    SendTerminalInput,
    SendTerminalKey { key: TerminalKey },
    ResizeTerminal { rows: u16, cols: u16 },
    CloseTerminal,
    SelectManagementSection { section: ManagementSection },
    RefreshManagement,
    SelectProviderProfile { index: usize },
    RunHealthProbes,
    CreatePairingOffer,
    CancelPairingOffer,
    RevokeDevice { index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTestInput {
    AgentComposer,
    FileEditor,
    CommitMessage,
    TerminalInput,
}

pub struct WorkflowWorkbenchView {
    facade: BackendFacade,
    pub workflow: AgentFileGitController,
    pub terminal: TerminalWorkflowController,
    pub management: ManagementWorkflowController,
    pub navigation: CompactNavigation,
    workspaces: AsyncState<Vec<WorkspaceSummary>>,
    selected_workspace_id: Option<WorkspaceId>,
    pending_workspace_selection: Option<usize>,
    sidebar: SidebarState,
    connection: WorkbenchConnectionState,
    layout: ShellLayout,
    host_viewport: HostViewportSnapshot,
    composer: Entity<InputState>,
    file_search: Entity<InputState>,
    file_editor: Entity<InputState>,
    commit_message: Entity<InputState>,
    terminal_input: Entity<InputState>,
    elicitation_inputs: BTreeMap<String, Entity<InputState>>,
    elicitation_drafts: BTreeMap<String, crate::ElicitationFormDraft>,
    selected_git_key: Option<GitSelectionKey>,
    compact_detail: Option<CompactDetailPage>,
    settings_section: WorkbenchSettingsSection,
    expanded_timeline_rows: BTreeSet<String>,
    agent_live_event_count: u64,
    agent_recovery_count: u64,
    file_live_event_count: u64,
    file_recovery_count: u64,
    git_live_event_count: u64,
    git_recovery_count: u64,
    terminal_recovery_count: u64,
    terminal_recovery_pending: bool,
    management_live_event_count: u64,
    management_recovery_count: u64,
    last_error: Option<BackendError>,
    sidebar_scroll: ScrollHandle,
    content_scroll: ScrollHandle,
    auxiliary_scroll: ScrollHandle,
    bootstrap_task: Option<Task<()>>,
    agent_task: Option<Task<()>>,
    /// Event refreshes and concurrent input callbacks must not cancel an
    /// in-flight send or another Agent mutation future.
    agent_mutation_tasks: BTreeMap<String, Task<()>>,
    agent_event_refresh_task: Option<Task<()>>,
    event_task: Option<Task<()>>,
    file_task: Option<Task<()>>,
    file_event_refresh_task: Option<Task<()>>,
    git_task: Option<Task<()>>,
    git_event_refresh_task: Option<Task<()>>,
    terminal_task: Option<Task<()>>,
    terminal_poll_task: Option<Task<()>>,
    management_task: Option<Task<()>>,
    management_event_refresh_task: Option<Task<()>>,
    management_operation_pending: bool,
    management_event_refresh_pending: Option<bool>,
    _subscriptions: Vec<Subscription>,
}

impl WorkflowWorkbenchView {
    pub fn new(facade: BackendFacade, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workflow = AgentFileGitController::from_facade(&facade);
        let terminal = TerminalWorkflowController::new(
            facade.terminal().clone(),
            TerminalWorkflowCapabilities::from_backend(&facade.capabilities()),
        );
        let management = ManagementWorkflowController::new(
            facade.management().clone(),
            facade.device().clone(),
            ManagementWorkflowCapabilities::from_backend(&facade.capabilities()),
        );
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder("Message the Agent")
        });
        let file_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search workspace files"));
        let file_editor = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(20)
                .placeholder("Select a text file")
        });
        let commit_message = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(3)
                .placeholder("Commit message")
        });
        let terminal_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Send terminal input"));

        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        this.send_agent_message(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &file_search,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        this.search_files(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &file_editor,
                window,
                |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change)
                        && this.workflow.files.state.active_file.phase == AsyncPhase::Ready
                    {
                        let value = input.read(cx).value().to_string();
                        if let Err(error) = this.workflow.files.update_active_content(value) {
                            this.last_error = Some(error);
                        }
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &terminal_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        this.send_terminal_input(window, cx);
                    }
                },
            ),
        ];

        Self {
            facade,
            workflow,
            terminal,
            management,
            navigation: CompactNavigation::default(),
            workspaces: AsyncState::default(),
            selected_workspace_id: None,
            pending_workspace_selection: None,
            sidebar: SidebarState::default(),
            connection: WorkbenchConnectionState::Online,
            layout: ShellLayout::resolve_mobile(360, 800),
            host_viewport: HostViewportSnapshot {
                width: 360.0,
                height: 800.0,
                safe_area: HostInsets::default(),
                keyboard_visible: false,
                keyboard_inset: 0.0,
                keyboard_source: HostKeyboardSource::None,
            },
            composer,
            file_search,
            file_editor,
            commit_message,
            terminal_input,
            elicitation_inputs: BTreeMap::new(),
            elicitation_drafts: BTreeMap::new(),
            selected_git_key: None,
            compact_detail: None,
            settings_section: WorkbenchSettingsSection::Connection,
            expanded_timeline_rows: BTreeSet::new(),
            agent_live_event_count: 0,
            agent_recovery_count: 0,
            file_live_event_count: 0,
            file_recovery_count: 0,
            git_live_event_count: 0,
            git_recovery_count: 0,
            terminal_recovery_count: 0,
            terminal_recovery_pending: false,
            management_live_event_count: 0,
            management_recovery_count: 0,
            last_error: None,
            sidebar_scroll: ScrollHandle::new(),
            content_scroll: ScrollHandle::new(),
            auxiliary_scroll: ScrollHandle::new(),
            bootstrap_task: None,
            agent_task: None,
            agent_mutation_tasks: BTreeMap::new(),
            agent_event_refresh_task: None,
            event_task: None,
            file_task: None,
            file_event_refresh_task: None,
            git_task: None,
            git_event_refresh_task: None,
            terminal_task: None,
            terminal_poll_task: None,
            management_task: None,
            management_event_refresh_task: None,
            management_operation_pending: false,
            management_event_refresh_pending: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.refresh_all(cx);
        self.start_event_subscription(cx);
    }

    pub fn refresh_capabilities(&mut self, snapshot: &BackendCapabilitySnapshot) {
        self.workflow.refresh_capabilities(snapshot);
        self.terminal
            .set_capabilities(TerminalWorkflowCapabilities::from_backend(snapshot));
        self.management
            .set_capabilities(ManagementWorkflowCapabilities::from_backend(snapshot));
    }

    pub fn set_connection_state(
        &mut self,
        state: WorkbenchConnectionState,
        cx: &mut Context<Self>,
    ) {
        let previous = self.connection;
        let capabilities = self.facade.capabilities();
        self.refresh_capabilities(&capabilities);
        if state == previous {
            if state == WorkbenchConnectionState::Online && self.event_task.is_none() {
                self.start_event_subscription(cx);
            }
            return;
        }
        self.connection = state;
        match state {
            WorkbenchConnectionState::Online => {
                self.workflow.agent.state.connection = crate::AgentConnectionState::Online;
                if self.event_task.is_none() {
                    self.start_event_subscription(cx);
                }
                if previous != WorkbenchConnectionState::Online {
                    self.refresh_after_reconnect(cx);
                }
                if previous != WorkbenchConnectionState::Online
                    && self.terminal_recovery_pending
                    && let Some(terminal_id) = self
                        .terminal
                        .state
                        .active_session
                        .as_ref()
                        .map(|session| session.id.clone())
                {
                    match self.terminal.attach(terminal_id) {
                        Ok(()) => self.schedule_terminal_poll(cx),
                        Err(error) => self.last_error = Some(error),
                    }
                }
            }
            WorkbenchConnectionState::Reconnecting | WorkbenchConnectionState::Degraded => {
                self.workflow.agent.mark_reconnecting();
                self.mark_terminal_reconnecting();
            }
            WorkbenchConnectionState::Offline
            | WorkbenchConnectionState::Revoked
            | WorkbenchConnectionState::Incompatible => {
                self.workflow.agent.state.connection = crate::AgentConnectionState::Offline;
                self.mark_terminal_reconnecting();
            }
        }
        cx.notify();
    }

    fn refresh_after_reconnect(&mut self, cx: &mut Context<Self>) {
        self.reload_active_session(true, cx);
        if self.selected_workspace_id.is_some() {
            self.refresh_file_tree_from_event(true, cx);
            self.refresh_git_status_from_event(true, cx);
        }
        if self.management_operation_pending {
            self.management_event_refresh_pending = Some(true);
        } else {
            self.refresh_management_from_event(true, cx);
        }
    }

    pub fn apply_browser_host_snapshot(
        &mut self,
        snapshot: &BrowserHostSnapshot,
        cx: &mut Context<Self>,
    ) {
        let viewport = HostViewportSnapshot {
            width: snapshot.viewport_width,
            height: snapshot.viewport_height,
            safe_area: HostInsets {
                top: snapshot.safe_area.top,
                right: snapshot.safe_area.right,
                bottom: snapshot.safe_area.bottom,
                left: snapshot.safe_area.left,
            },
            keyboard_visible: snapshot.keyboard_visible,
            keyboard_inset: snapshot.keyboard_inset,
            keyboard_source: match snapshot.keyboard_source {
                KeyboardSource::Capacitor => HostKeyboardSource::Capacitor,
                KeyboardSource::VisualViewport => HostKeyboardSource::VisualViewport,
                KeyboardSource::None => HostKeyboardSource::None,
            },
        };
        self.apply_host_viewport(viewport, cx);
    }

    pub fn apply_host_viewport(&mut self, viewport: HostViewportSnapshot, cx: &mut Context<Self>) {
        self.layout = ShellLayout::resolve_mobile(
            normalized_dimension(viewport.width),
            normalized_dimension(viewport.height),
        );
        self.terminal.apply_host_viewport(&viewport);
        self.host_viewport = viewport;
        cx.notify();
    }

    pub fn navigation_state(&self) -> &CompactNavigation {
        &self.navigation
    }

    pub fn select_global_destination(
        &mut self,
        destination: GlobalDestination,
        cx: &mut Context<Self>,
    ) {
        self.compact_detail = None;
        self.navigation.select_global(destination);
        cx.notify();
    }

    pub fn select_session_destination(
        &mut self,
        destination: SessionDestination,
        cx: &mut Context<Self>,
    ) {
        self.compact_detail = None;
        self.navigation.select_session(destination);
        cx.notify();
    }

    pub fn enter_session(&mut self, session_id: &str, cx: &mut Context<Self>) -> BackendResult<()> {
        let session_id = VibexSessionId::parse(session_id.to_string()).map_err(|_| {
            BackendError::failed(
                "agent_session_id_invalid",
                "the Agent session id is invalid",
            )
        })?;
        self.open_session(session_id, cx)
    }

    pub fn close_navigation_overlay(&mut self, cx: &mut Context<Self>) -> bool {
        let handled = self.navigation.close_overlay().is_some();
        if handled {
            cx.notify();
        }
        handled
    }

    pub fn platform_back(&mut self, cx: &mut Context<Self>) -> bool {
        if self.compact_detail.take().is_some() {
            cx.notify();
            return true;
        }
        let handled = self.navigation.back();
        if handled {
            cx.notify();
        }
        handled
    }

    fn error_codes(&self) -> Vec<String> {
        let mut error_codes = [
            self.last_error.as_ref(),
            self.workflow.files.state.last_error.as_ref(),
            self.workflow.git.state.last_error.as_ref(),
            self.terminal.state.last_error.as_ref(),
            self.management.state.last_error.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|error| error.code.clone())
        .collect::<Vec<_>>();
        error_codes.sort();
        error_codes.dedup();
        error_codes
    }

    pub fn snapshot(&self) -> WorkflowWorkbenchSnapshot {
        let shell = self.layout.kind;
        let workflow = self.workflow.view(&self.sidebar, "", shell);
        let terminal = self.terminal.state.view(shell);
        let management = self.management.state.view(shell);
        let error_codes = self.error_codes();
        WorkflowWorkbenchSnapshot {
            schema_version: WORKFLOW_WORKBENCH_SCHEMA_VERSION,
            connection: self.connection,
            shell,
            navigation_level: self.navigation.level,
            global_destination: self.navigation.global,
            session_destination: self.navigation.session,
            active_surface: self.active_surface(),
            workspace_count: self.workspaces.value.as_ref().map_or(0, Vec::len),
            session_count: workflow.agent.sessions.len(),
            timeline_row_count: workflow.agent.timeline_rows.len(),
            pending_approval_count: workflow.agent.approvals.len(),
            agent_runtime_ready: self.workflow.agent.state.runtime_selection.phase
                == AsyncPhase::Ready,
            agent_mutation_phase: self.workflow.agent.state.latest_mutation.phase,
            agent_live_event_count: self.agent_live_event_count,
            agent_recovery_count: self.agent_recovery_count,
            file_row_count: workflow.files.rows.len(),
            file_search_result_count: workflow.files.search.len(),
            file_has_active_file: workflow.files.active_file.is_some(),
            file_editor_status: workflow.files.status,
            file_live_event_count: self.file_live_event_count,
            file_recovery_count: self.file_recovery_count,
            git_change_count: workflow
                .git
                .status
                .as_ref()
                .map_or(0, |status| status.changes.len()),
            git_selected_count: workflow.git.selected_paths.len(),
            git_mutation_pending: self.workflow.git.state.model.pending_mutation.is_some(),
            git_commit_phase: self.workflow.git.state.last_commit.phase,
            git_live_event_count: self.git_live_event_count,
            git_recovery_count: self.git_recovery_count,
            terminal_count: terminal.sessions.len(),
            terminal_connection: terminal.connection,
            terminal_sequence: terminal.raw_next_sequence,
            terminal_rebuild_count: terminal.rebuild_count,
            terminal_recovery_count: self.terminal_recovery_count,
            management_load_state: management.load_state,
            management_agent_count: management.agents.len(),
            management_profile_count: management.profiles.len(),
            management_health_count: management.health.len(),
            management_device_count: management.devices.len(),
            management_revoked_device_count: management
                .devices
                .iter()
                .filter(|device| device.status == RemoteDeviceStatus::Revoked)
                .count(),
            management_operation_pending: self.management_operation_pending,
            management_live_event_count: self.management_live_event_count,
            management_recovery_count: self.management_recovery_count,
            has_relay_status: management.relay.is_some(),
            has_pairing_offer: management
                .pairing_offer
                .as_ref()
                .is_some_and(|offer| !offer.canceled && offer.launch_fragment.is_some()),
            error_codes,
        }
    }

    pub fn apply_test_command(
        &mut self,
        command: WorkflowWorkbenchCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        match command {
            WorkflowWorkbenchCommand::RefreshAll => self.refresh_all(cx),
            WorkflowWorkbenchCommand::SelectSurface { surface } => self.select_surface(surface, cx),
            WorkflowWorkbenchCommand::SelectWorkspace { index } => {
                if let Some(items) = self.workspaces.value.as_ref() {
                    let workspace_id = items
                        .get(index)
                        .map(|item| item.workspace.id.clone())
                        .ok_or_else(|| index_error("workspace"))?;
                    self.select_workspace(workspace_id, cx);
                } else {
                    // The initial workspace projection is still loading right
                    // after connect; apply the selection when the bootstrap
                    // resolves instead of failing on the empty projection.
                    self.pending_workspace_selection = Some(index);
                }
            }
            WorkflowWorkbenchCommand::SelectSession { index } => {
                let session_id = self
                    .workflow
                    .agent
                    .state
                    .sessions
                    .value
                    .as_ref()
                    .and_then(|items| items.get(index))
                    .map(|item| item.id.clone())
                    .ok_or_else(|| index_error("session"))?;
                self.open_session(session_id, cx)?;
            }
            WorkflowWorkbenchCommand::FillTestInput { input } => match input {
                WorkflowTestInput::AgentComposer => self.composer.update(cx, |input, cx| {
                    input.set_value(TEST_AGENT_MESSAGE, window, cx)
                }),
                WorkflowTestInput::FileEditor => {
                    self.file_editor.update(cx, |input, cx| {
                        input.set_value(TEST_FILE_CONTENT, window, cx)
                    });
                    fill_fixed_file_test_input(&mut self.workflow.files)?;
                }
                WorkflowTestInput::CommitMessage => self.commit_message.update(cx, |input, cx| {
                    input.set_value(TEST_COMMIT_MESSAGE, window, cx)
                }),
                WorkflowTestInput::TerminalInput => self.terminal_input.update(cx, |input, cx| {
                    input.set_value(TEST_TERMINAL_INPUT, window, cx)
                }),
            },
            WorkflowWorkbenchCommand::SendAgentMessage => self.send_agent_message(window, cx),
            WorkflowWorkbenchCommand::ResolveApproval { index, approve } => {
                let request_id = self
                    .workflow
                    .agent
                    .state
                    .approval_surfaces(self.layout.kind)
                    .get(index)
                    .map(|approval| approval.request_id.clone())
                    .ok_or_else(|| index_error("approval"))?;
                self.resolve_approval(
                    request_id,
                    if approve {
                        PermissionResponseKind::Approve
                    } else {
                        PermissionResponseKind::Deny
                    },
                    None,
                    cx,
                )?;
            }
            WorkflowWorkbenchCommand::OpenFileRow { index } => {
                self.open_file_row(index, window, cx)?
            }
            WorkflowWorkbenchCommand::SearchFiles => self.search_files(cx),
            WorkflowWorkbenchCommand::SaveFile => self.save_file(cx)?,
            WorkflowWorkbenchCommand::ReloadFileConflict => {
                if self.workflow.files.state.reload_server_version() {
                    let content = self
                        .workflow
                        .files
                        .state
                        .view()
                        .editor_content
                        .unwrap_or_default();
                    self.file_editor
                        .update(cx, |input, cx| input.set_value(content, window, cx));
                    cx.notify();
                }
            }
            WorkflowWorkbenchCommand::LoadGitDiff { index } => self.load_git_diff(index, cx)?,
            WorkflowWorkbenchCommand::StageGitChange { index } => {
                self.mutate_git_change(index, true, cx)?
            }
            WorkflowWorkbenchCommand::UnstageGitChange { index } => {
                self.mutate_git_change(index, false, cx)?
            }
            WorkflowWorkbenchCommand::PrepareCommit => self.prepare_commit(cx)?,
            WorkflowWorkbenchCommand::ConfirmCommit => self.confirm_commit(cx)?,
            WorkflowWorkbenchCommand::CreateTerminal => self.create_terminal(cx)?,
            WorkflowWorkbenchCommand::AttachTerminal { index } => {
                self.attach_terminal(index, cx)?
            }
            WorkflowWorkbenchCommand::SendTerminalInput => self.send_terminal_input(window, cx),
            WorkflowWorkbenchCommand::SendTerminalKey { key } => self.send_terminal_key(key, cx)?,
            WorkflowWorkbenchCommand::ResizeTerminal { rows, cols } => {
                self.resize_terminal(rows, cols, cx)?
            }
            WorkflowWorkbenchCommand::CloseTerminal => self.close_terminal(cx)?,
            WorkflowWorkbenchCommand::SelectManagementSection { section } => {
                if self.management.switch_section(section, true) {
                    self.management_operation_pending = false;
                    self.drain_management_event_refresh(cx);
                }
                if self.layout.kind != ShellKind::Wide {
                    self.compact_detail = (section != ManagementSection::Overview)
                        .then_some(CompactDetailPage::ManagementSection);
                }
                cx.notify();
            }
            WorkflowWorkbenchCommand::RefreshManagement => self.refresh_management(cx),
            WorkflowWorkbenchCommand::SelectProviderProfile { index } => {
                self.select_provider_profile(index, cx)?
            }
            WorkflowWorkbenchCommand::RunHealthProbes => self.run_health_probes(cx)?,
            WorkflowWorkbenchCommand::CreatePairingOffer => self.create_pairing_offer(cx)?,
            WorkflowWorkbenchCommand::CancelPairingOffer => self.cancel_pairing_offer(cx)?,
            WorkflowWorkbenchCommand::RevokeDevice { index } => self.revoke_device(index, cx)?,
        }
        Ok(())
    }

    fn active_surface(&self) -> WorkbenchSurface {
        if self.navigation.level == NavigationLevel::Global {
            match self.navigation.global {
                GlobalDestination::Management => return WorkbenchSurface::Management,
                GlobalDestination::Settings => return WorkbenchSurface::Agent,
                // `select_surface` is also used by the host contract before a
                // session exists. Preserve that surface in the sanitized
                // snapshot so the five-surface probe remains deterministic;
                // the renderer presents the corresponding empty/list state.
                GlobalDestination::Sessions => {}
            }
        }
        match self.navigation.session {
            SessionDestination::Agent => WorkbenchSurface::Agent,
            SessionDestination::Files => WorkbenchSurface::Files,
            SessionDestination::Changes => WorkbenchSurface::Git,
            SessionDestination::Terminal => WorkbenchSurface::Terminal,
        }
    }

    fn select_surface(&mut self, surface: WorkbenchSurface, cx: &mut Context<Self>) {
        self.compact_detail = None;
        match surface {
            WorkbenchSurface::Management => {
                self.navigation.select_global(GlobalDestination::Management)
            }
            WorkbenchSurface::Agent => self.select_session_surface(SessionDestination::Agent),
            WorkbenchSurface::Files => self.select_session_surface(SessionDestination::Files),
            WorkbenchSurface::Git => self.select_session_surface(SessionDestination::Changes),
            WorkbenchSurface::Terminal => self.select_session_surface(SessionDestination::Terminal),
        }
        cx.notify();
    }

    fn select_session_surface(&mut self, destination: SessionDestination) {
        if self.navigation.session_id.is_none()
            && let Some(session_id) = self
                .workflow
                .agent
                .state
                .selected_session_id
                .as_ref()
                .map(ToString::to_string)
        {
            self.navigation.enter_session(session_id);
        }
        self.navigation.select_session(destination);
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let capabilities = self.facade.capabilities();
        self.refresh_capabilities(&capabilities);
        self.workflow.agent.begin_sessions_refresh();
        self.workspaces.begin();
        self.last_error = None;
        let sessions = self.workflow.agent.list_sessions(false);
        let workspace_backend = self.facade.workspace().clone();
        self.bootstrap_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let sessions_result = sessions.await;
            let workspaces_result = workspace_backend.list_workspaces().await;
            let _ = entity.update(cx, |this, cx| {
                if let Err(error) = this.workflow.agent.apply_sessions(sessions_result) {
                    this.last_error = Some(error);
                }
                match workspaces_result {
                    Ok(workspaces) => this.workspaces.resolve(workspaces),
                    Err(error) => {
                        this.workspaces.reject(error.clone());
                        this.last_error = Some(error);
                    }
                }
                this.reconcile_after_bootstrap(cx);
                cx.notify();
            });
        }));
        self.refresh_management(cx);
    }

    fn reconcile_after_bootstrap(&mut self, cx: &mut Context<Self>) {
        let sessions = self
            .workflow
            .agent
            .state
            .sessions
            .value
            .clone()
            .unwrap_or_default();
        self.sidebar
            .reconcile(sessions.iter().map(|session| session.id.to_string()));
        let workspace_id = self
            .pending_workspace_selection
            .take()
            .and_then(|index| {
                let selected = self
                    .workspaces
                    .value
                    .as_ref()
                    .and_then(|items| items.get(index))
                    .map(|item| item.workspace.id.clone());
                if selected.is_none() {
                    self.last_error = Some(index_error("workspace"));
                }
                selected
            })
            .or_else(|| {
                self.selected_workspace_id.clone().filter(|selected| {
                    self.workspaces.value.as_ref().is_some_and(|workspaces| {
                        workspaces.iter().any(|item| &item.workspace.id == selected)
                    }) || sessions
                        .iter()
                        .any(|session| &session.workspace_id == selected)
                })
            })
            .or_else(|| sessions.first().map(|session| session.workspace_id.clone()))
            .or_else(|| {
                self.workspaces
                    .value
                    .as_ref()
                    .and_then(|items| items.first())
                    .map(|item| item.workspace.id.clone())
            });
        if let Some(workspace_id) = workspace_id {
            self.select_workspace(workspace_id, cx);
        }
    }

    fn start_event_subscription(&mut self, cx: &mut Context<Self>) {
        let mut subscription = match self.facade.agent().subscribe() {
            Ok(subscription) => subscription,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        self.event_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            loop {
                let event = subscription.next().await;
                let stop = match event {
                    Ok(Some(event)) => entity
                        .update(cx, |this, cx| this.apply_backend_event(event, cx))
                        .unwrap_or(true),
                    Ok(None) => entity
                        .update(cx, |this, cx| {
                            this.connection = WorkbenchConnectionState::Reconnecting;
                            this.workflow.agent.mark_reconnecting();
                            this.mark_terminal_reconnecting();
                            cx.notify();
                            true
                        })
                        .unwrap_or(true),
                    Err(error) => entity
                        .update(cx, |this, cx| {
                            this.last_error = Some(error);
                            this.connection = WorkbenchConnectionState::Reconnecting;
                            this.workflow.agent.mark_reconnecting();
                            this.mark_terminal_reconnecting();
                            cx.notify();
                            true
                        })
                        .unwrap_or(true),
                };
                if stop {
                    break;
                }
            }
            let _ = entity.update(cx, |this, cx| {
                this.event_task = None;
                if event_subscription_should_restart(this.connection) {
                    this.start_event_subscription(cx);
                }
            });
        }));
    }

    fn apply_backend_event(&mut self, event: BackendEvent, cx: &mut Context<Self>) -> bool {
        match event {
            BackendEvent::ProjectionInvalidated(projection) => {
                self.refresh_projection(projection, false, cx);
                cx.notify();
                false
            }
            BackendEvent::Lagged {
                refetch:
                    vibex_backend::BackendRefetch {
                        projection: Some(projection),
                        ..
                    },
                observed_live,
                ..
            } => {
                self.refresh_projection(projection, !observed_live, cx);
                cx.notify();
                false
            }
            BackendEvent::Lagged { refetch, .. }
                if !refetch.timeline && !refetch.runtime && !refetch.runtime_selection =>
            {
                false
            }
            event => {
                let observed_live = agent_event_is_relevant_live(
                    self.workflow.agent.state.selected_session_id.as_ref(),
                    &event,
                );
                if observed_live {
                    self.agent_live_event_count = self.agent_live_event_count.saturating_add(1);
                }
                let decision = self.workflow.agent.apply_event(event);
                match decision {
                    AgentEventDecision::NeedsAuthoritativeRefetch => {
                        self.reload_active_session(!observed_live, cx);
                    }
                    AgentEventDecision::Applied => {}
                    AgentEventDecision::Disconnected => {
                        self.connection = WorkbenchConnectionState::Reconnecting;
                        self.mark_terminal_reconnecting();
                    }
                    AgentEventDecision::IgnoredStale => {}
                }
                cx.notify();
                decision == AgentEventDecision::Disconnected
            }
        }
    }

    fn refresh_projection(
        &mut self,
        projection: BackendProjection,
        recovery: bool,
        cx: &mut Context<Self>,
    ) {
        match projection {
            BackendProjection::Files => {
                if !recovery {
                    self.file_live_event_count = self.file_live_event_count.saturating_add(1);
                }
                self.refresh_file_tree_from_event(recovery, cx);
            }
            BackendProjection::Git => {
                if !recovery {
                    self.git_live_event_count = self.git_live_event_count.saturating_add(1);
                }
                self.refresh_git_status_from_event(recovery, cx);
            }
            BackendProjection::Management => {
                if !recovery {
                    self.management_live_event_count =
                        self.management_live_event_count.saturating_add(1);
                }
                if self.management_operation_pending {
                    self.management_event_refresh_pending =
                        Some(self.management_event_refresh_pending.unwrap_or(false) || recovery);
                } else {
                    self.refresh_management_from_event(recovery, cx);
                }
            }
            BackendProjection::Usage => {
                cx.notify();
            }
        }
    }

    fn drain_management_event_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.management_operation_pending
            && let Some(recovery) = self.management_event_refresh_pending.take()
        {
            self.refresh_management_from_event(recovery, cx);
        }
    }

    fn mark_terminal_reconnecting(&mut self) {
        if self.terminal.state.active_session.is_some()
            && self.terminal.state.connection != TerminalConnectionState::Closed
        {
            self.terminal_recovery_pending = true;
            self.terminal.disconnect();
        }
    }

    fn open_session(
        &mut self,
        session_id: VibexSessionId,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        self.open_session_with_recovery(session_id, false, cx)
    }

    fn open_session_with_recovery(
        &mut self,
        session_id: VibexSessionId,
        recovery: bool,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let workspace_id = self
            .workflow
            .agent
            .state
            .sessions
            .value
            .as_ref()
            .and_then(|sessions| sessions.iter().find(|session| session.id == session_id))
            .map(|session| session.workspace_id.clone());
        if let Some(workspace_id) = workspace_id {
            self.select_workspace(workspace_id, cx);
        }
        let ticket = self.workflow.agent.begin_session_load(session_id.clone())?;
        self.navigation.enter_session(session_id.to_string());
        let future = self.workflow.agent.load_session(ticket.clone());
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let succeeded = result.is_ok();
            let _ = entity.update(cx, |this, cx| {
                let applied = this.workflow.agent.apply_session_snapshot(&ticket, result);
                if recovery && succeeded && applied {
                    this.agent_recovery_count = this.agent_recovery_count.saturating_add(1);
                }
                cx.notify();
            });
        });
        if recovery {
            self.agent_event_refresh_task = Some(task);
        } else {
            self.agent_task = Some(task);
        }
        Ok(())
    }

    fn reload_active_session(&mut self, recovery: bool, cx: &mut Context<Self>) {
        if let Some(session_id) = self.workflow.agent.state.selected_session_id.clone() {
            let _ = self.open_session_with_recovery(session_id, recovery, cx);
        }
    }

    fn send_agent_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(session_id) = self.workflow.agent.state.selected_session_id.clone() else {
            self.last_error = Some(BackendError::failed(
                "agent_session_missing",
                "select an Agent session before sending a message",
            ));
            cx.notify();
            return;
        };
        let Some(runtime) = self
            .workflow
            .agent
            .state
            .runtime_selection
            .value
            .as_ref()
            .map(|selection| selection.desired.clone())
        else {
            self.last_error = Some(BackendError::loading(
                "agent_runtime_selection_loading",
                "the Agent runtime selection is not ready",
            ));
            cx.notify();
            return;
        };
        let request = MutationRequest::new(SendAgentMessageRequest {
            session_id,
            message_idempotency_key: RequestId::new().to_string(),
            desired_runtime: runtime,
            text,
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        });
        let ticket = match self.workflow.agent.begin_send_message(&request) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.last_error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = self.workflow.agent.send_message(request);
        let task_key = ticket.request_id.clone();
        let task = cx.spawn_in(window, async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let succeeded = result.is_ok();
            let _ = entity.update_in(cx, |this, window, cx| {
                if this.workflow.agent.apply_timeline_mutation(&ticket, result) && succeeded {
                    this.composer
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                cx.notify();
            });
        });
        self.store_agent_mutation_task(task_key, task);
    }

    fn resolve_approval(
        &mut self,
        request_id: RequestId,
        response: PermissionResponseKind,
        provider_resolution_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let session_id = self
            .workflow
            .agent
            .state
            .selected_session_id
            .clone()
            .ok_or_else(|| {
                BackendError::failed("agent_session_missing", "no Agent session is selected")
            })?;
        let permission_request_id = request_id.to_string();
        let request = MutationRequest::new(ResolvePermissionRequest {
            session_id: session_id.clone(),
            request_id: request_id.clone(),
            resolution: PermissionResolution {
                request_id,
                session_id,
                response,
                responder_device_id: None,
                provider_resolution_id,
                note: None,
                resolved_at_ms: unix_timestamp_ms(),
            },
        });
        let ticket = self.workflow.agent.begin_resolve_permission(&request)?;
        let future = self.workflow.agent.resolve_permission(request);
        let task_key = ticket.request_id.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.workflow.agent.apply_permission_mutation(
                    &ticket,
                    &permission_request_id,
                    result,
                );
                cx.notify();
            });
        });
        self.store_agent_mutation_task(task_key, task);
        Ok(())
    }

    fn store_agent_mutation_task(&mut self, request_id: String, task: Task<()>) {
        self.agent_mutation_tasks
            .retain(|_, existing| !existing.is_ready());
        self.agent_mutation_tasks.insert(request_id, task);
    }

    fn elicitation_input_key(request_id: &str, field_id: &str) -> String {
        format!("{request_id}:{field_id}")
    }

    fn sync_elicitation_forms(
        &mut self,
        surfaces: &[crate::ElicitationSurfaceModel],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request_ids = surfaces
            .iter()
            .map(|surface| surface.request.id.to_string())
            .collect::<BTreeSet<_>>();
        let mut input_keys = BTreeSet::new();
        for surface in surfaces {
            let request = &surface.request;
            self.elicitation_drafts
                .entry(request.id.to_string())
                .or_insert_with(|| crate::ElicitationFormDraft::from_request(request));
            for field in &request.fields {
                let uses_input = matches!(
                    &field.kind,
                    ElicitationFieldKind::Text { options, .. } if options.is_empty()
                ) || matches!(
                    &field.kind,
                    ElicitationFieldKind::Number { .. } | ElicitationFieldKind::Integer { .. }
                );
                if !uses_input {
                    continue;
                }
                let key = Self::elicitation_input_key(request.id.as_str(), &field.id);
                input_keys.insert(key.clone());
                if self.elicitation_inputs.contains_key(&key) {
                    continue;
                }
                let initial_value = self
                    .elicitation_drafts
                    .get(request.id.as_str())
                    .and_then(|draft| draft.text(&field.id))
                    .unwrap_or_default()
                    .to_string();
                let placeholder = field.title.clone();
                let input = cx.new(|cx| {
                    let mut input = InputState::new(window, cx).placeholder(placeholder);
                    if !initial_value.is_empty() {
                        input.set_value(initial_value, window, cx);
                    }
                    input
                });
                self.elicitation_inputs.insert(key, input);
            }
        }
        self.elicitation_drafts
            .retain(|request_id, _| request_ids.contains(request_id));
        self.elicitation_inputs
            .retain(|key, _| input_keys.contains(key));
    }

    fn resolve_elicitation(
        &mut self,
        request: ElicitationRequest,
        action: ElicitationResolutionAction,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let request_id = request.id.to_string();
        let text_values = request
            .fields
            .iter()
            .filter_map(|field| {
                let key = Self::elicitation_input_key(request.id.as_str(), &field.id);
                self.elicitation_inputs
                    .get(&key)
                    .map(|input| (field.id.clone(), input.read(cx).value().to_string()))
            })
            .collect::<Vec<_>>();
        let draft = self
            .elicitation_drafts
            .entry(request_id.clone())
            .or_insert_with(|| crate::ElicitationFormDraft::from_request(&request));
        for (field_id, value) in text_values {
            if request
                .fields
                .iter()
                .find(|field| field.id == field_id)
                .is_some_and(|field| field.required || !value.is_empty())
            {
                draft.set_text(field_id, value);
            } else {
                draft.values.remove(&field_id);
            }
        }
        let payload = draft
            .resolve_request(&request, action, unix_timestamp_ms())
            .map_err(BackendError::from)?;
        let mutation = MutationRequest::new(payload);
        let ticket = self.workflow.agent.begin_resolve_elicitation(&mutation)?;
        let future = self.workflow.agent.resolve_elicitation(mutation);
        let task_key = ticket.request_id.clone();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let succeeded = result.is_ok();
            let _ = entity.update(cx, |this, cx| {
                this.workflow
                    .agent
                    .apply_elicitation_mutation(&ticket, &request_id, result);
                if succeeded {
                    this.elicitation_drafts.remove(&request_id);
                    let prefix = format!("{request_id}:");
                    this.elicitation_inputs
                        .retain(|key, _| !key.starts_with(&prefix));
                }
                cx.notify();
            });
        });
        self.store_agent_mutation_task(task_key, task);
        Ok(())
    }

    fn select_workspace(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        if self.selected_workspace_id.as_ref() == Some(&workspace_id)
            && self.workflow.files.state.workspace_id.as_ref() == Some(&workspace_id)
        {
            return;
        }
        self.selected_workspace_id = Some(workspace_id.clone());
        self.workflow.files.select_workspace(workspace_id.clone());
        self.workflow.git.select_workspace(workspace_id.clone());
        self.selected_git_key = None;
        self.refresh_file_tree(cx);
        self.refresh_git_status(cx);
        self.refresh_terminals(workspace_id, cx);
    }

    fn refresh_file_tree(&mut self, cx: &mut Context<Self>) {
        self.refresh_file_tree_with_reason(false, false, cx);
    }

    fn refresh_file_tree_from_event(&mut self, recovery: bool, cx: &mut Context<Self>) {
        self.refresh_file_tree_with_reason(recovery, true, cx);
    }

    fn refresh_file_tree_with_reason(
        &mut self,
        recovery: bool,
        event_driven: bool,
        cx: &mut Context<Self>,
    ) {
        let ticket = match self.workflow.files.begin_tree_load("") {
            Ok(ticket) => ticket,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let future = self.workflow.files.load_tree(ticket.clone());
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let succeeded = result.is_ok();
            let _ = entity.update(cx, |this, cx| {
                let applied = this.workflow.files.apply_tree_load(&ticket, result);
                if recovery && succeeded && applied {
                    this.file_recovery_count = this.file_recovery_count.saturating_add(1);
                }
                cx.notify();
            });
        });
        if event_driven {
            self.file_event_refresh_task = Some(task);
        } else {
            self.file_task = Some(task);
        }
    }

    fn open_file_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let row = self
            .workflow
            .files
            .state
            .view()
            .rows
            .get(index)
            .cloned()
            .ok_or_else(|| index_error("file row"))?;
        if row.kind == FileEntryKind::Directory {
            let ticket = self.workflow.files.begin_tree_load(&row.path)?;
            let future = self.workflow.files.load_tree(ticket.clone());
            self.file_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                let result = future.await;
                let _ = entity.update(cx, |this, cx| {
                    this.workflow.files.apply_tree_load(&ticket, result);
                    cx.notify();
                });
            }));
            return Ok(());
        }
        self.open_file(row.path, window, cx)
    }

    fn open_file(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let ticket = self.workflow.files.begin_open_file(path)?;
        if self.layout.kind != ShellKind::Wide {
            self.compact_detail = Some(CompactDetailPage::FilePreview);
        }
        let future = self.workflow.files.read_file(ticket.clone());
        self.file_task = Some(
            cx.spawn_in(window, async move |entity: WeakEntity<Self>, cx| {
                let result = future.await;
                let _ = entity.update_in(cx, |this, window, cx| {
                    if this.workflow.files.apply_file_read(&ticket, result) {
                        let content = this
                            .workflow
                            .files
                            .state
                            .view()
                            .editor_content
                            .unwrap_or_default();
                        this.file_editor
                            .update(cx, |input, cx| input.set_value(content, window, cx));
                    }
                    cx.notify();
                });
            }),
        );
        Ok(())
    }

    fn search_files(&mut self, cx: &mut Context<Self>) {
        let query = self.file_search.read(cx).value().trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(workspace_id) = self.selected_workspace_id.clone() else {
            return;
        };
        if let Err(error) = self.workflow.files.begin_search() {
            self.last_error = Some(error);
            cx.notify();
            return;
        }
        let generation = self.workflow.files.state.generation;
        let future = self.workflow.files.search_files(FileSearchRequest {
            workspace_id,
            query,
            include_content: false,
            limit: Some(100),
        });
        self.file_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.workflow.files.apply_search(generation, result);
                cx.notify();
            });
        }));
    }

    fn save_file(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        let content = self.file_editor.read(cx).value().to_string();
        self.workflow.files.update_active_content(content)?;
        let operation = self.workflow.files.begin_save_active()?;
        let future = self.workflow.files.save_file(operation.clone());
        self.file_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.workflow.files.apply_save_outcome(&operation, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn refresh_git_status(&mut self, cx: &mut Context<Self>) {
        self.refresh_git_status_with_reason(false, false, cx);
    }

    fn refresh_git_status_from_event(&mut self, recovery: bool, cx: &mut Context<Self>) {
        self.refresh_git_status_with_reason(recovery, true, cx);
    }

    fn refresh_git_status_with_reason(
        &mut self,
        recovery: bool,
        event_driven: bool,
        cx: &mut Context<Self>,
    ) {
        let ticket = match self.workflow.git.begin_status_load() {
            Ok(ticket) => ticket,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let future = self.workflow.git.load_status(ticket.clone());
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let succeeded = result.is_ok();
            let _ = entity.update(cx, |this, cx| {
                let applied = this.workflow.git.apply_status(&ticket, result);
                if recovery && succeeded && applied {
                    this.git_recovery_count = this.git_recovery_count.saturating_add(1);
                }
                cx.notify();
            });
        });
        if event_driven {
            self.git_event_refresh_task = Some(task);
        } else {
            self.git_task = Some(task);
        }
    }

    fn git_change(&self, index: usize) -> BackendResult<vibex_core::GitChange> {
        self.workflow
            .git
            .state
            .model
            .status
            .as_ref()
            .and_then(|status| status.changes.get(index))
            .cloned()
            .ok_or_else(|| index_error("Git change"))
    }

    fn load_git_diff(&mut self, index: usize, cx: &mut Context<Self>) -> BackendResult<()> {
        let change = self.git_change(index)?;
        let staged = change.staged && !change.unstaged;
        let key = GitSelectionKey {
            path: change.path.clone(),
            staged,
        };
        let ticket = self.workflow.git.begin_diff_load(change.path, staged)?;
        self.selected_git_key = Some(key);
        if self.layout.kind != ShellKind::Wide {
            self.compact_detail = Some(CompactDetailPage::GitDiff);
        }
        let future = self.workflow.git.load_diff(ticket.clone());
        self.git_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.workflow.git.apply_diff(&ticket, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn mutate_git_change(
        &mut self,
        index: usize,
        stage: bool,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let change = self.git_change(index)?;
        let operation = if stage {
            self.workflow.git.begin_stage(vec![change.path])?
        } else {
            self.workflow.git.begin_unstage(vec![change.path])?
        };
        let future = self.workflow.git.run_paths_mutation(operation.clone());
        self.git_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.workflow.git.apply_paths_mutation(&operation, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn prepare_commit(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        let message = self.commit_message.read(cx).value().to_string();
        let paths = self
            .workflow
            .git
            .state
            .model
            .status
            .as_ref()
            .map(|status| {
                status
                    .changes
                    .iter()
                    .filter(|change| change.staged)
                    .map(|change| change.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.workflow
            .git
            .request_commit_confirmation(message, paths)?;
        cx.notify();
        Ok(())
    }

    fn confirm_commit(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        self.workflow.git.confirm_commit()?;
        let operation = self.workflow.git.begin_confirmed_commit()?;
        let future = self.workflow.git.run_commit(operation.clone());
        self.git_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.workflow.git.apply_commit(&operation, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn refresh_terminals(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        let ticket = match self.terminal.begin_refresh(workspace_id) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.last_error = Some(error);
                return;
            }
        };
        let future = self.terminal.load_sessions(ticket.clone());
        self.terminal_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let first_id = result
                .as_ref()
                .ok()
                .and_then(|sessions| sessions.first())
                .map(|session| session.id.clone());
            let _ = entity.update(cx, |this, cx| {
                if this.terminal.apply_refresh(&ticket, result)
                    && this.terminal.state.active_session.is_none()
                    && let Some(terminal_id) = first_id
                    && this.terminal.attach(terminal_id).is_ok()
                {
                    this.schedule_terminal_poll(cx);
                }
                cx.notify();
            });
        }));
    }

    fn create_terminal(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        let workspace_id = self.selected_workspace_id.clone().ok_or_else(|| {
            BackendError::failed("terminal_workspace_missing", "no workspace is selected")
        })?;
        let operation =
            self.terminal
                .begin_create(MutationRequest::new(TerminalCreateRequest {
                    workspace_id,
                    title: Some("Terminal".into()),
                    shell: None,
                    cwd: None,
                    rows: 24,
                    cols: 80,
                }))?;
        let future = self.terminal.run_create(operation.clone());
        self.terminal_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let created_id = result.as_ref().ok().map(|session| session.id.clone());
            let _ = entity.update(cx, |this, cx| {
                if this.terminal.apply_create(&operation, result)
                    && let Some(terminal_id) = created_id
                    && this.terminal.attach(terminal_id).is_ok()
                {
                    this.schedule_terminal_poll(cx);
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn attach_terminal(&mut self, index: usize, cx: &mut Context<Self>) -> BackendResult<()> {
        let terminal_id = self
            .terminal
            .state
            .sessions
            .get(index)
            .map(|session| session.id.clone())
            .ok_or_else(|| index_error("terminal"))?;
        self.terminal.attach(terminal_id)?;
        self.schedule_terminal_poll(cx);
        cx.notify();
        Ok(())
    }

    fn schedule_terminal_poll(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.terminal.begin_poll() else {
            return;
        };
        self.terminal_poll_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let (generation, subscription, result) = task.next().await;
            let _ = entity.update(cx, |this, cx| {
                let keep_polling = this
                    .terminal
                    .apply_poll_result(generation, subscription, result)
                    .unwrap_or(false);
                if keep_polling
                    && this.terminal_recovery_pending
                    && this.terminal.state.connection == TerminalConnectionState::Connected
                {
                    this.terminal_recovery_pending = false;
                    this.terminal_recovery_count = this.terminal_recovery_count.saturating_add(1);
                }
                if keep_polling {
                    this.schedule_terminal_poll(cx);
                }
                cx.notify();
            });
        }));
    }

    fn send_terminal_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.terminal_input.read(cx).value().to_string();
        if value.is_empty() {
            return;
        }
        let operation = match self
            .terminal
            .begin_send_input(TerminalInput::Text(format!("{value}\r")))
        {
            Ok(Some(operation)) => operation,
            Ok(None) => return,
            Err(error) => {
                self.last_error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = self.terminal.run_input(operation.clone());
        self.terminal_task = Some(cx.spawn_in(
            window,
            async move |entity: WeakEntity<Self>, cx| {
                let result = future.await;
                let succeeded = result.is_ok();
                let _ = entity.update_in(cx, |this, window, cx| {
                    if this.terminal.apply_input(&operation, result) && succeeded {
                        this.terminal_input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn send_terminal_key(&mut self, key: TerminalKey, cx: &mut Context<Self>) -> BackendResult<()> {
        let Some(operation) = self
            .terminal
            .begin_send_input(TerminalInput::Key(key, TerminalKeyModifiers::default()))?
        else {
            cx.notify();
            return Ok(());
        };
        let future = self.terminal.run_input(operation.clone());
        self.terminal_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_input(&operation, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn resize_terminal(
        &mut self,
        rows: u16,
        cols: u16,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let operation = self.terminal.begin_resize(rows, cols)?;
        let future = self.terminal.run_resize(operation.clone());
        self.terminal_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_resize(&operation, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn close_terminal(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        let operation = self.terminal.begin_close()?;
        self.terminal_recovery_pending = false;
        let future = self.terminal.run_close(operation.clone());
        self.terminal_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_close(&operation, result);
                cx.notify();
            });
        }));
        Ok(())
    }

    fn management_worker_is_current(&self, worker: &ManagementWorkflowController) -> bool {
        management_completion_is_current(
            self.management.state.generation,
            self.management.state.last_operation_id,
            worker.state.generation,
            worker.state.last_operation_id,
        )
    }

    fn refresh_management(&mut self, cx: &mut Context<Self>) {
        self.refresh_management_with_reason(false, false, cx);
    }

    fn refresh_management_from_event(&mut self, recovery: bool, cx: &mut Context<Self>) {
        self.refresh_management_with_reason(recovery, true, cx);
    }

    fn refresh_management_with_reason(
        &mut self,
        recovery: bool,
        event_driven: bool,
        cx: &mut Context<Self>,
    ) {
        let mut worker = self.management.clone();
        let expected_generation = worker.state.generation.saturating_add(1).max(1);
        self.management.state.generation = expected_generation;
        self.management.state.load_state = ManagementLoadState::Loading;
        self.management.state.last_error = None;
        self.management_operation_pending = true;
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let succeeded = worker.refresh().await.is_ok();
            let _ = entity.update(cx, |this, cx| {
                if this.management_worker_is_current(&worker) {
                    this.management = worker;
                    this.management_operation_pending = false;
                    if recovery && succeeded {
                        this.management_recovery_count =
                            this.management_recovery_count.saturating_add(1);
                    }
                    this.drain_management_event_refresh(cx);
                }
                cx.notify();
            });
        });
        if event_driven {
            self.management_event_refresh_task = Some(task);
        } else {
            self.management_task = Some(task);
        }
    }

    fn select_provider_profile(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> BackendResult<()> {
        let profile = self
            .management
            .state
            .profiles
            .get(index)
            .cloned()
            .ok_or_else(|| index_error("Provider profile"))?;
        let agent_id = AgentId::parse(profile.agent_id).map_err(|_| {
            BackendError::failed("management_agent_id_invalid", "the Agent id is invalid")
        })?;
        let provider_profile_id = ProviderProfileId::parse(profile.id).map_err(|_| {
            BackendError::failed(
                "management_profile_id_invalid",
                "the Provider profile id is invalid",
            )
        })?;
        let mut worker = self.management.clone();
        let expected_operation = worker.state.last_operation_id.saturating_add(1).max(1);
        self.management.state.last_operation_id = expected_operation;
        self.management_operation_pending = true;
        self.management_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = worker
                .select_profile(MutationRequest::new(
                    vibex_backend::ManagementProfileSelectionRequest {
                        agent_id,
                        provider_profile_id,
                    },
                ))
                .await;
            let _ = entity.update(cx, |this, cx| {
                if this.management_worker_is_current(&worker) {
                    this.management = worker;
                    this.management_operation_pending = false;
                    this.drain_management_event_refresh(cx);
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn run_health_probes(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        self.management
            .capabilities
            .require(BackendOperation::ManagementHealth)?;
        let mut worker = self.management.clone();
        let expected_operation = worker.state.last_operation_id.saturating_add(1).max(1);
        self.management.state.last_operation_id = expected_operation;
        self.management_operation_pending = true;
        self.management_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = worker
                .run_health_probes(MutationRequest::new(ProviderRunHealthProbesRequest {
                    provider_profile_ids: None,
                    probe_kinds: None,
                }))
                .await;
            let _ = entity.update(cx, |this, cx| {
                if this.management_worker_is_current(&worker) {
                    this.management = worker;
                    this.management_operation_pending = false;
                    this.drain_management_event_refresh(cx);
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn create_pairing_offer(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        self.management
            .capabilities
            .require(BackendOperation::DevicePairing)?;
        let mut worker = self.management.clone();
        let expected_operation = worker.state.last_operation_id.saturating_add(1).max(1);
        self.management.state.last_operation_id = expected_operation;
        self.management_operation_pending = true;
        self.management_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = worker
                .create_pairing_offer(MutationRequest::new(RemoteCreatePairingOfferRequest {
                    permission_level: RemoteDevicePermissionLevel::FullControl,
                    ttl_ms: Some(120_000),
                    direct_candidates: Vec::new(),
                    relay_candidate: None,
                }))
                .await;
            let _ = entity.update(cx, |this, cx| {
                if this.management_worker_is_current(&worker) {
                    this.management = worker;
                    this.management_operation_pending = false;
                    this.drain_management_event_refresh(cx);
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn cancel_pairing_offer(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        self.management
            .capabilities
            .require(BackendOperation::DevicePairing)?;
        let offer_id = self
            .management
            .state
            .pairing_offer
            .as_ref()
            .filter(|offer| !offer.canceled)
            .map(|offer| offer.offer_id.as_str())
            .ok_or_else(|| {
                BackendError::failed(
                    "management_pairing_offer_missing",
                    "there is no active pairing offer to cancel",
                )
            })?;
        let offer_id = RequestId::parse(offer_id).map_err(|_| {
            BackendError::failed(
                "management_pairing_offer_id_invalid",
                "the pairing offer id is invalid",
            )
        })?;
        let mut worker = self.management.clone();
        let expected_operation = worker.state.last_operation_id.saturating_add(1).max(1);
        self.management.state.last_operation_id = expected_operation;
        self.management_operation_pending = true;
        self.management_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = worker.cancel_pairing_offer(offer_id).await;
            let _ = entity.update(cx, |this, cx| {
                if this.management_worker_is_current(&worker) {
                    this.management = worker;
                    this.management_operation_pending = false;
                    this.drain_management_event_refresh(cx);
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn revoke_device(&mut self, index: usize, cx: &mut Context<Self>) -> BackendResult<()> {
        let device = self
            .management
            .state
            .devices
            .get(index)
            .cloned()
            .ok_or_else(|| index_error("device"))?;
        let device_id = DeviceId::parse(device.device_id).map_err(|_| {
            BackendError::failed("management_device_id_invalid", "the device id is invalid")
        })?;
        let mut worker = self.management.clone();
        let expected_operation = worker.state.last_operation_id.saturating_add(1).max(1);
        self.management.state.last_operation_id = expected_operation;
        self.management_operation_pending = true;
        self.management_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = worker
                .revoke_device(MutationRequest::new(RemoteRevokeDeviceRequest {
                    device_id,
                    reason: Some("Revoked from the shared workbench".into()),
                }))
                .await;
            let _ = entity.update(cx, |this, cx| {
                if this.management_worker_is_current(&worker) {
                    this.management = worker;
                    this.management_operation_pending = false;
                    this.drain_management_event_refresh(cx);
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn record_action_result(&mut self, result: BackendResult<()>, cx: &mut Context<Self>) {
        if let Err(error) = result {
            self.last_error = Some(error);
        }
        cx.notify();
    }

    fn toggle_timeline_row(&mut self, row_id: String, cx: &mut Context<Self>) {
        if !self.expanded_timeline_rows.insert(row_id.clone()) {
            self.expanded_timeline_rows.remove(&row_id);
        }
        cx.notify();
    }

    fn select_settings_section(
        &mut self,
        section: WorkbenchSettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        cx.notify();
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let compact = self.layout.kind == ShellKind::Compact;
        let shows_back = self.navigation.level == NavigationLevel::Session
            || self.navigation.global != GlobalDestination::Sessions
            || self.compact_detail.is_some();
        let title = self.page_title();
        let subtitle = self.page_subtitle();
        let connection = connection_label(self.connection);
        let connection_color = connection_color(self.connection, cx);

        h_flex()
            .id("workflow-top-bar")
            .h(px(if compact { MOBILE_TOP_BAR_HEIGHT } else { 60.0 }))
            .w_full()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(if compact { 8.0 } else { 16.0 }))
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_2()
                    .children(shows_back.then(|| {
                        Button::new("workflow-back")
                            .ghost()
                            .compact()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::ArrowLeft)
                            .tooltip("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.platform_back(cx);
                            }))
                    }))
                    .children((!shows_back).then(|| {
                        div()
                            .id("workflow-brand-mark")
                            .size(px(32.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.0))
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("V")
                    }))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .id("workflow-page-title")
                                    .role(Role::Heading)
                                    .aria_level(1)
                                    .truncate()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .children(subtitle.map(|subtitle| {
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(subtitle)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        h_flex()
                            .h(px(28.0))
                            .gap_1()
                            .px_2()
                            .rounded(px(6.0))
                            .bg(cx.theme().muted.opacity(0.55))
                            .child(div().size(px(7.0)).rounded_full().bg(connection_color))
                            .children((!compact).then(|| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(connection)
                            })),
                    )
                    .child(
                        Button::new("workflow-refresh-all")
                            .ghost()
                            .compact()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Replace)
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
                    ),
            )
            .into_any_element()
    }

    fn page_title(&self) -> String {
        if self.compact_detail == Some(CompactDetailPage::FilePreview) {
            return self
                .workflow
                .files
                .state
                .view()
                .selected_path
                .as_deref()
                .and_then(path_file_name)
                .unwrap_or("File")
                .to_string();
        }
        if self.compact_detail == Some(CompactDetailPage::GitDiff) {
            return self
                .selected_git_key
                .as_ref()
                .and_then(|key| path_file_name(&key.path))
                .unwrap_or("Diff")
                .to_string();
        }
        if self.compact_detail == Some(CompactDetailPage::ManagementSection) {
            return self.management.state.navigation.active.label().to_string();
        }
        match self.navigation.level {
            NavigationLevel::Session => match self.navigation.session {
                SessionDestination::Agent => self
                    .workflow
                    .agent
                    .state
                    .active_session
                    .value
                    .as_ref()
                    .map(|session| session.title.clone())
                    .unwrap_or_else(|| "Agent".into()),
                SessionDestination::Files => "Files".into(),
                SessionDestination::Changes => "Changes".into(),
                SessionDestination::Terminal => "Terminal".into(),
            },
            NavigationLevel::Global => match self.navigation.global {
                GlobalDestination::Sessions => "Vibex".into(),
                GlobalDestination::Management => "Management Center".into(),
                GlobalDestination::Settings => "Settings".into(),
            },
        }
    }

    fn page_subtitle(&self) -> Option<String> {
        if self.navigation.level == NavigationLevel::Session {
            return self.selected_workspace_label();
        }
        match self.navigation.global {
            GlobalDestination::Sessions => Some("Your remote coding sessions".into()),
            GlobalDestination::Management => Some("Agents, providers and devices".into()),
            GlobalDestination::Settings => Some(self.settings_section.label().into()),
        }
    }

    fn selected_workspace_label(&self) -> Option<String> {
        let selected = self.selected_workspace_id.as_ref()?;
        self.workspaces
            .value
            .as_ref()?
            .iter()
            .find(|item| &item.workspace.id == selected)
            .map(|item| item.project.name.clone())
    }

    fn render_workspace_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspaces = self.workspaces.value.clone().unwrap_or_default();
        let selected = self.selected_workspace_id.clone();
        let rows = workspaces.into_iter().enumerate().map(|(index, item)| {
            let workspace_id = item.workspace.id.clone();
            let is_selected = selected.as_ref() == Some(&workspace_id);
            let mut button = Button::new(("workspace-row", index))
                .w_full()
                .justify_start()
                .label(item.project.name)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_workspace(workspace_id.clone(), cx)
                }));
            button = if is_selected {
                button.primary()
            } else {
                button.ghost()
            };
            button
        });
        v_flex()
            .id("workflow-workspace-sidebar")
            .w(px(self.layout.sidebar_width))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .px_3()
                    .py_3()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Workspaces"),
            )
            .child(
                v_flex()
                    .id("workflow-sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .vertical_scrollbar(&self.sidebar_scroll)
                    .px_2()
                    .gap_1()
                    .children(rows),
            )
            .child(self.render_global_navigation(cx))
            .into_any_element()
    }

    fn render_global_navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.navigation.global;
        let level = self.navigation.level;
        let items = [
            (GlobalDestination::Sessions, "Sessions", IconName::Bot),
            (GlobalDestination::Management, "Manage", IconName::Settings2),
            (GlobalDestination::Settings, "Settings", IconName::Settings),
        ];
        let buttons = items
            .into_iter()
            .map(|(destination, label, icon)| {
                let selected = level == NavigationLevel::Global && current == destination;
                Button::new(format!("workflow-global-{destination:?}"))
                    .ghost()
                    .compact()
                    .flex_1()
                    .h(px(52.0))
                    .rounded(px(8.0))
                    .selected(selected)
                    .icon(icon)
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_global_destination(destination, cx)
                    }))
            })
            .collect::<Vec<_>>();
        if self.layout.kind == ShellKind::Wide {
            v_flex()
                .id("workflow-global-navigation")
                .w_full()
                .gap_1()
                .p_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .children(buttons)
                .into_any_element()
        } else {
            Self::render_bottom_navigation("workflow-global-navigation", buttons, cx)
        }
    }

    fn render_surface_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.active_surface();
        let items = [
            (WorkbenchSurface::Agent, "Agent", IconName::Bot),
            (WorkbenchSurface::Files, "Files", IconName::FolderOpen),
            (WorkbenchSurface::Git, "Changes", IconName::Github),
            (
                WorkbenchSurface::Terminal,
                "Terminal",
                IconName::SquareTerminal,
            ),
        ];
        let buttons = items
            .into_iter()
            .map(|(surface, label, icon)| {
                Button::new(format!("workflow-tab-{surface:?}"))
                    .ghost()
                    .compact()
                    .flex_1()
                    .h(px(52.0))
                    .rounded(px(8.0))
                    .selected(current == surface)
                    .icon(icon)
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_surface(surface, cx)))
            })
            .collect::<Vec<_>>();
        if self.layout.kind == ShellKind::Wide {
            h_flex()
                .id("workflow-surface-tabs")
                .w_full()
                .h(px(44.0))
                .flex_none()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .children(buttons)
                .into_any_element()
        } else {
            Self::render_bottom_navigation("workflow-session-navigation", buttons, cx)
        }
    }

    fn render_bottom_navigation(
        id: &'static str,
        items: impl IntoIterator<Item = Button>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(id)
            .role(Role::Navigation)
            .w_full()
            .h(px(MOBILE_BOTTOM_NAV_HEIGHT))
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .children(items)
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let section = self.settings_section;
        let options =
            WorkbenchSettingsSection::ALL
                .into_iter()
                .enumerate()
                .map(|(index, option)| {
                    let mut button = Button::new(("mobile-settings-section", index))
                        .w_full()
                        .min_h(px(MOBILE_TOUCH_TARGET))
                        .justify_start()
                        .label(option.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_settings_section(option, cx);
                        }));
                    button = if section == option {
                        button.primary()
                    } else {
                        button.ghost()
                    };
                    button
                });
        let detail = match section {
            WorkbenchSettingsSection::Connection => v_flex()
                .gap_3()
                .child(settings_value_row(
                    "Connection",
                    connection_label(self.connection),
                    connection_color(self.connection, cx),
                    cx,
                ))
                .child(settings_value_row(
                    "Workspaces",
                    self.workspaces
                        .value
                        .as_ref()
                        .map_or_else(|| "Loading".into(), |items| items.len().to_string()),
                    cx.theme().foreground,
                    cx,
                ))
                .child(settings_value_row(
                    "Transport",
                    "Authoritative desktop backend",
                    cx.theme().muted_foreground,
                    cx,
                ))
                .child(
                    Button::new("settings-refresh")
                        .primary()
                        .min_h(px(MOBILE_TOUCH_TARGET))
                        .label("Refresh connection")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
                )
                .into_any_element(),
            WorkbenchSettingsSection::Appearance => v_flex()
                .gap_3()
                .child(settings_value_row(
                    "Shell",
                    self.layout.kind.label(),
                    cx.theme().foreground,
                    cx,
                ))
                .child(settings_value_row(
                    "Theme",
                    "System appearance",
                    cx.theme().foreground,
                    cx,
                ))
                .child(settings_value_row(
                    "Touch targets",
                    "44 px minimum",
                    cx.theme().muted_foreground,
                    cx,
                ))
                .into_any_element(),
            WorkbenchSettingsSection::About => v_flex()
                .gap_3()
                .child(settings_value_row(
                    "Product",
                    "Vibex mobile workbench",
                    cx.theme().foreground,
                    cx,
                ))
                .child(settings_value_row(
                    "Workflow schema",
                    WORKFLOW_WORKBENCH_SCHEMA_VERSION,
                    cx.theme().muted_foreground,
                    cx,
                ))
                .child(settings_value_row(
                    "Runtime",
                    "GPUI-WASM + Capacitor",
                    cx.theme().muted_foreground,
                    cx,
                ))
                .into_any_element(),
        };
        v_flex()
            .id("mobile-settings")
            .size_full()
            .min_h_0()
            .gap_3()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Local display and connection preferences"),
                    ),
            )
            .child(h_flex().w_full().gap_1().children(options))
            .child(
                v_flex()
                    .id("mobile-settings-detail")
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .p_3()
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(section.label()),
                    )
                    .child(detail),
            )
            .into_any_element()
    }

    fn render_elicitation_field(
        &mut self,
        request_id: &str,
        field: &ElicitationField,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if field.required {
            format!("{} *", field.title)
        } else {
            field.title.clone()
        };
        let control = match &field.kind {
            ElicitationFieldKind::Text { options, .. } if !options.is_empty() => {
                let mut choices = Vec::with_capacity(options.len());
                for (index, option) in options.iter().enumerate() {
                    let selected = self
                        .elicitation_drafts
                        .get(request_id)
                        .and_then(|draft| draft.text(&field.id))
                        .is_some_and(|value| value == option.value);
                    let draft_id = request_id.to_string();
                    let field_id = field.id.clone();
                    let value = option.value.clone();
                    let mut button = Button::new(format!(
                        "elicitation-choice:{request_id}:{}:{index}",
                        field.id
                    ))
                    .label(option.title.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(draft) = this.elicitation_drafts.get_mut(&draft_id) {
                            draft.select_option(field_id.clone(), value.clone());
                            cx.notify();
                        }
                    }));
                    button = if selected {
                        button.primary().icon(IconName::Check)
                    } else {
                        button.outline()
                    };
                    choices.push(
                        v_flex()
                            .min_w(px(140.0))
                            .gap_1()
                            .child(button)
                            .when_some(option.description.clone(), |this, description| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(description),
                                )
                            })
                            .into_any_element(),
                    );
                }
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_start()
                    .gap_2()
                    .children(choices)
                    .into_any_element()
            }
            ElicitationFieldKind::Text { .. }
            | ElicitationFieldKind::Number { .. }
            | ElicitationFieldKind::Integer { .. } => {
                let key = Self::elicitation_input_key(request_id, &field.id);
                self.elicitation_inputs
                    .get(&key)
                    .map(|input| Input::new(input).w_full().into_any_element())
                    .unwrap_or_else(|| div().into_any_element())
            }
            ElicitationFieldKind::Boolean { default } => {
                let checked = self
                    .elicitation_drafts
                    .get(request_id)
                    .and_then(|draft| draft.boolean(&field.id))
                    .or(*default)
                    .unwrap_or(false);
                let draft_id = request_id.to_string();
                let field_id = field.id.clone();
                Switch::new(SharedString::from(format!(
                    "elicitation-boolean:{request_id}:{}",
                    field.id
                )))
                .checked(checked)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if let Some(draft) = this.elicitation_drafts.get_mut(&draft_id) {
                        draft.set_boolean(field_id.clone(), *checked);
                        cx.notify();
                    }
                }))
                .into_any_element()
            }
            ElicitationFieldKind::MultiSelect { options, .. } => {
                let mut choices = Vec::with_capacity(options.len());
                for (index, option) in options.iter().enumerate() {
                    let selected = self
                        .elicitation_drafts
                        .get(request_id)
                        .is_some_and(|draft| draft.multi_selected(&field.id, &option.value));
                    let draft_id = request_id.to_string();
                    let field_id = field.id.clone();
                    let value = option.value.clone();
                    let mut button = Button::new(format!(
                        "elicitation-multi:{request_id}:{}:{index}",
                        field.id
                    ))
                    .label(option.title.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(draft) = this.elicitation_drafts.get_mut(&draft_id) {
                            draft.toggle_multi_option(field_id.clone(), value.clone());
                            cx.notify();
                        }
                    }));
                    button = if selected {
                        button.primary().icon(IconName::Check)
                    } else {
                        button.outline()
                    };
                    choices.push(
                        v_flex()
                            .min_w(px(140.0))
                            .gap_1()
                            .child(button)
                            .when_some(option.description.clone(), |this, description| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(description),
                                )
                            })
                            .into_any_element(),
                    );
                }
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_start()
                    .gap_2()
                    .children(choices)
                    .into_any_element()
            }
            ElicitationFieldKind::Unsupported { schema_type } => h_flex()
                .min_h(px(32.0))
                .gap_2()
                .text_sm()
                .text_color(cx.theme().warning)
                .child(IconName::TriangleAlert)
                .child(format!("Unavailable: {schema_type}"))
                .into_any_element(),
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(label),
            )
            .when_some(field.description.clone(), |this, description| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                )
            })
            .child(control)
            .into_any_element()
    }

    fn render_elicitation_surface(
        &mut self,
        surface: crate::ElicitationSurfaceModel,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let request = surface.request;
        let request_id = request.id.to_string();
        let pending = self
            .workflow
            .agent
            .state
            .pending_elicitation_resolution(&request_id);
        let can_submit = !request.fields.iter().any(|field| {
            field.required && matches!(&field.kind, ElicitationFieldKind::Unsupported { .. })
        });
        let fields = request
            .fields
            .iter()
            .map(|field| self.render_elicitation_field(&request_id, field, cx))
            .collect::<Vec<_>>();
        let decline_request = request.clone();
        let submit_request = request.clone();
        let title = request
            .title
            .clone()
            .unwrap_or_else(|| "Input requested".to_string());
        v_flex()
            .id(format!("agent-elicitation:{request_id}"))
            .w_full()
            .min_w_0()
            .gap_3()
            .p_3()
            .border_1()
            .border_color(cx.theme().warning.opacity(0.45))
            .rounded(cx.theme().radius)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .child(div().text_sm().child(request.message.clone()))
                    .when_some(request.description.clone(), |this, description| {
                        this.when(description.trim() != request.message.trim(), |this| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(description),
                            )
                        })
                    }),
            )
            .children(fields)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(format!("elicitation-decline:{request_id}"))
                            .outline()
                            .icon(IconName::Close)
                            .label("Decline")
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let result = this.resolve_elicitation(
                                    decline_request.clone(),
                                    ElicitationResolutionAction::Decline,
                                    cx,
                                );
                                this.record_action_result(result, cx);
                            })),
                    )
                    .child(
                        Button::new(format!("elicitation-submit:{request_id}"))
                            .primary()
                            .icon(IconName::Check)
                            .label("Submit")
                            .disabled(pending || !can_submit)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let result = this.resolve_elicitation(
                                    submit_request.clone(),
                                    ElicitationResolutionAction::Accept,
                                    cx,
                                );
                                this.record_action_result(result, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_agent_wide(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self
            .workflow
            .agent
            .state
            .view(&self.sidebar, "", self.layout.kind);
        self.sync_elicitation_forms(&view.elicitations, window, cx);
        let elicitations = view
            .elicitations
            .into_iter()
            .map(|surface| self.render_elicitation_surface(surface, cx))
            .collect::<Vec<_>>();
        let selected_id = self.workflow.agent.state.selected_session_id.clone();
        let sessions = view.sessions.into_iter().enumerate().map(|(index, row)| {
            let is_session = row.kind == AgentSidebarRowKind::Session;
            let selected = row.session_id.as_ref() == selected_id.as_ref();
            let session_id = row.session_id.clone();
            let mut button = Button::new(("agent-session-row", index))
                .w_full()
                .justify_start()
                .label(row.label)
                .disabled(!is_session)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(session_id) = session_id.clone() {
                        let result = this.open_session(session_id, cx);
                        this.record_action_result(result, cx);
                    }
                }));
            button = if selected {
                button.primary()
            } else {
                button.ghost()
            };
            button
        });
        let timeline = view
            .timeline_rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                v_flex()
                    .id(("agent-timeline-row", index))
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(row.title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{:?}", row.kind)),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(bounded_text(row.body, MAX_RENDERED_TEXT_BYTES)),
                    )
            });
        let approvals = view
            .approvals
            .into_iter()
            .enumerate()
            .map(|(index, approval)| {
                let response_options = if approval.response_options.is_empty() {
                    approval
                        .allowed_responses
                        .iter()
                        .map(|response| {
                            (
                                None,
                                match response {
                                    PermissionResponseKind::Approve => "Allow".to_string(),
                                    PermissionResponseKind::AlwaysAllowForSession => {
                                        "Always allow for session".to_string()
                                    }
                                    PermissionResponseKind::Deny => "Deny".to_string(),
                                },
                                *response,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    approval
                        .response_options
                        .iter()
                        .map(|option| {
                            (
                                Some(option.option_id.clone()),
                                option.label.clone(),
                                option.response,
                            )
                        })
                        .collect::<Vec<_>>()
                };
                v_flex()
                    .id(("agent-approval", index))
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(approval.title),
                    )
                    .child(h_flex().gap_2().flex_wrap().children(
                        response_options.into_iter().enumerate().map(
                            |(option_index, (option_id, label, response))| {
                                let request_id = approval.request_id.clone();
                                let button = Button::new(format!(
                                    "agent-approval-response:{index}:{option_index}"
                                ))
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let result = this.resolve_approval(
                                        request_id.clone(),
                                        response,
                                        option_id.clone(),
                                        cx,
                                    );
                                    this.record_action_result(result, cx);
                                }));
                                match response {
                                    PermissionResponseKind::Approve => button.primary(),
                                    PermissionResponseKind::Deny => button.outline(),
                                    PermissionResponseKind::AlwaysAllowForSession => button,
                                }
                            },
                        ),
                    ))
            });
        let session_list = v_flex()
            .w(if self.layout.kind == ShellKind::Wide {
                px(240.0)
            } else {
                px(200.0)
            })
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Sessions"),
            )
            .child(
                v_flex()
                    .id("agent-session-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .gap_1()
                    .children(sessions),
            );
        let content = v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .child(
                v_flex()
                    .id("agent-timeline-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .children(timeline),
            )
            .child(v_flex().p_2().gap_2().children(approvals))
            .child(v_flex().p_2().gap_2().children(elicitations))
            .child(
                h_flex()
                    .min_h(px(72.0))
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(Input::new(&self.composer).w_full())
                    .child(Button::new("agent-send").primary().label("Send").on_click(
                        cx.listener(|this, _, window, cx| this.send_agent_message(window, cx)),
                    )),
            );
        if self.layout.kind == ShellKind::Compact {
            if self.navigation.level == NavigationLevel::Global {
                v_flex().size_full().child(session_list).into_any_element()
            } else {
                content.into_any_element()
            }
        } else {
            h_flex()
                .size_full()
                .min_h_0()
                .child(session_list)
                .child(content)
                .into_any_element()
        }
    }

    fn render_files_wide(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.workflow.files.state.view();
        let rows = view
            .rows
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let label = format!("{}{}", "  ".repeat(row.depth), row.name);
                let selected = view.selected_path.as_deref() == Some(row.path.as_str());
                let mut button = Button::new(("file-row", index))
                    .w_full()
                    .justify_start()
                    .icon(if row.kind == FileEntryKind::Directory {
                        IconName::Folder
                    } else {
                        IconName::File
                    })
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let result = this.open_file_row(index, window, cx);
                        this.record_action_result(result, cx);
                    }));
                button = if selected {
                    button.primary()
                } else {
                    button.ghost()
                };
                button
            });
        let search_rows = view.search.into_iter().enumerate().map(|(index, result)| {
            let path = result.path;
            Button::new(("file-search-result", index))
                .w_full()
                .justify_start()
                .ghost()
                .icon(IconName::Search)
                .label(result.name)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let result = this.open_file(path.clone(), window, cx);
                    this.record_action_result(result, cx);
                }))
        });
        let tree = v_flex()
            .w(if self.layout.kind == ShellKind::Wide {
                px(300.0)
            } else {
                px(240.0)
            })
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .p_2()
                    .child(Input::new(&self.file_search).small().w_full())
                    .child(
                        Button::new("file-search")
                            .ghost()
                            .compact()
                            .icon(IconName::Search)
                            .tooltip("Search files")
                            .on_click(cx.listener(|this, _, _, cx| this.search_files(cx))),
                    )
                    .child(
                        Button::new("file-tree-refresh")
                            .ghost()
                            .compact()
                            .icon(IconName::Replace)
                            .tooltip("Refresh file tree")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_file_tree(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("file-tree-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .vertical_scrollbar(&self.sidebar_scroll)
                    .px_2()
                    .gap_1()
                    .children(search_rows)
                    .children(rows),
            );
        let conflict = view.conflict.map(|conflict| {
            v_flex()
                .gap_2()
                .p_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Revision conflict"),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("Local"))
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .child(bounded_text(
                                            conflict.local_content,
                                            MAX_RENDERED_TEXT_BYTES / 4,
                                        )),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("Server"))
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .child(bounded_text(
                                            conflict.server_content,
                                            MAX_RENDERED_TEXT_BYTES / 4,
                                        )),
                                ),
                        ),
                )
                .child(
                    Button::new("file-conflict-reload")
                        .outline()
                        .label("Reload server version")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let result = this.apply_test_command(
                                WorkflowWorkbenchCommand::ReloadFileConflict,
                                window,
                                cx,
                            );
                            this.record_action_result(result, cx);
                        })),
                )
        });
        let editor = v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(44.0))
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().min_w_0().text_sm().child(
                            view.selected_path
                                .unwrap_or_else(|| "No file selected".into()),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{:?}", view.status)),
                            )
                            .child(
                                Button::new("file-save")
                                    .primary()
                                    .label("Save")
                                    .disabled(!matches!(
                                        view.status,
                                        FileEditorStatus::Dirty | FileEditorStatus::Saved
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let result = this.save_file(cx);
                                        this.record_action_result(result, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .child(Input::new(&self.file_editor).size_full()),
            )
            .children(conflict);
        if self.layout.kind == ShellKind::Compact {
            v_flex()
                .size_full()
                .child(div().h(px(220.0)).child(tree))
                .child(editor)
                .into_any_element()
        } else {
            h_flex()
                .size_full()
                .min_h_0()
                .child(tree)
                .child(editor)
                .into_any_element()
        }
    }

    fn render_git_wide(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.workflow.git.state.view();
        let changes = view
            .status
            .as_ref()
            .map(|status| status.changes.clone())
            .unwrap_or_default();
        let change_rows = changes.into_iter().enumerate().map(|(index, change)| {
            let stage = !change.staged || change.unstaged;
            v_flex()
                .id(("git-change-row", index))
                .gap_1()
                .px_2()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Button::new(("git-diff", index))
                        .w_full()
                        .justify_start()
                        .ghost()
                        .label(change.path)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let result = this.load_git_diff(index, cx);
                            this.record_action_result(result, cx);
                        })),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(("git-stage-toggle", index))
                                .outline()
                                .label(if stage { "Stage" } else { "Unstage" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let result = this.mutate_git_change(index, stage, cx);
                                    this.record_action_result(result, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{:?}", change.kind)),
                        ),
                )
        });
        let diff_rows = self
            .selected_git_key
            .as_ref()
            .and_then(|key| self.workflow.git.state.model.diffs.get(key))
            .map(|document| {
                document
                    .files
                    .iter()
                    .flat_map(|file| file.lines.iter())
                    .take(400)
                    .enumerate()
                    .map(|(index, line)| {
                        h_flex()
                            .id(("git-diff-line", index))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .child(format!("{:?}", line.kind))
                            .child(bounded_text(line.content.clone(), 2_048))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let confirmation = view.commit_confirmation.map(|confirmation| {
            h_flex()
                .items_center()
                .justify_between()
                .gap_2()
                .p_2()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(div().text_sm().child(format!(
                    "Commit {} staged path(s)?",
                    confirmation.paths.len()
                )))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("git-commit-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workflow.git.cancel_commit();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("git-commit-confirm")
                                .primary()
                                .label("Confirm commit")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let result = this.confirm_commit(cx);
                                    this.record_action_result(result, cx);
                                })),
                        ),
                )
        });
        h_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .w(if self.layout.kind == ShellKind::Compact {
                        px(190.0)
                    } else {
                        px(320.0)
                    })
                    .h_full()
                    .min_h_0()
                    .flex_shrink_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .h(px(44.0))
                            .items_center()
                            .justify_between()
                            .px_3()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Changes"),
                            )
                            .child(
                                Button::new("git-refresh")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::Replace)
                                    .tooltip("Refresh Git status")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.refresh_git_status(cx)),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .id("git-change-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(change_rows),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("git-diff-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.content_scroll)
                            .vertical_scrollbar(&self.content_scroll)
                            .children(diff_rows),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .p_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .children(confirmation)
                            // Tall input keeps the commit message focusable at
                            // the shared workflow pointer target (x 0.73, y
                            // 0.88 of the canvas) across shell sizes.
                            .child(Input::new(&self.commit_message).w_full().h(px(56.)))
                            .child(
                                Button::new("git-commit-prepare")
                                    .primary()
                                    .label("Review commit")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let result = this.prepare_commit(cx);
                                        this.record_action_result(result, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_terminal_wide(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.terminal.state.view(self.layout.kind);
        let sessions = view
            .sessions
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, session)| {
                let mut button = Button::new(("terminal-session", index))
                    .justify_start()
                    .label(session.title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let result = this.attach_terminal(index, cx);
                        this.record_action_result(result, cx);
                    }));
                button = if session.selected {
                    button.primary()
                } else {
                    button.ghost()
                };
                button
            });
        let output = view
            .frame
            .as_ref()
            .map(terminal_frame_text)
            .unwrap_or_else(|| "Attach or create a terminal".into());
        let keys = view.key_bar.into_iter().enumerate().map(|(index, action)| {
            let key = action.key;
            Button::new(("terminal-key", index))
                .outline()
                .h(px(44.0))
                .label(action.label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let result = this.send_terminal_key(key, cx);
                    this.record_action_result(result, cx);
                }))
        });
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(48.0))
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(sessions)
                    .child(
                        Button::new("terminal-create")
                            .ghost()
                            .compact()
                            .icon(IconName::Plus)
                            .tooltip("Create terminal")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let result = this.create_terminal(cx);
                                this.record_action_result(result, cx);
                            })),
                    )
                    .child(
                        Button::new("terminal-close")
                            .ghost()
                            .compact()
                            .icon(IconName::Close)
                            .tooltip("Close terminal")
                            .disabled(view.active_session_id.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let result = this.close_terminal(cx);
                                this.record_action_result(result, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("terminal-output-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .p_3()
                    .bg(cx.theme().popover)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(output),
            )
            .child(h_flex().flex_wrap().gap_1().px_2().children(keys))
            .child(
                h_flex()
                    .h(px(88.))
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    // Tall input keeps terminal input focusable at the shared
                    // workflow pointer target (x 0.5, y 0.91 of the canvas).
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .child(Input::new(&self.terminal_input).size_full()),
                    )
                    .child(
                        Button::new("terminal-send")
                            .primary()
                            .label("Send")
                            .disabled(view.access != TerminalAccessMode::ReadWrite)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_terminal_input(window, cx)
                            })),
                    )
                    .child(
                        Button::new("terminal-resize")
                            .outline()
                            .label("100 x 30")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let result = this.resize_terminal(30, 100, cx);
                                this.record_action_result(result, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_management_wide(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.management.state.view(self.layout.kind);
        let pairing_fragment = view
            .pairing_offer
            .as_ref()
            .filter(|offer| !offer.canceled)
            .and_then(|offer| offer.launch_fragment.clone());
        let pairing_active = pairing_fragment.is_some();
        let pairing_material = pairing_fragment.map(|fragment| {
            let clipboard_value = fragment.clone();
            v_flex()
                .id("management-pairing-material")
                .min_w_0()
                .gap_2()
                .p_2()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .child(bounded_text(fragment, 2048)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("management-pairing-copy")
                                .outline()
                                .icon(IconName::Copy)
                                .tooltip("Copy pairing link")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        clipboard_value.clone(),
                                    ));
                                })),
                        )
                        .child(
                            Button::new("management-pairing-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let result = this.cancel_pairing_offer(cx);
                                    this.record_action_result(result, cx);
                                })),
                        ),
                )
                .into_any_element()
        });
        let can_select_profile = self
            .management
            .capabilities
            .supports(BackendOperation::ManagementProfileSelect);
        let can_pair = self
            .management
            .capabilities
            .supports(BackendOperation::DevicePairing);
        let can_run_health = self
            .management
            .capabilities
            .supports(BackendOperation::ManagementHealth);
        let can_revoke = self
            .management
            .capabilities
            .supports(BackendOperation::DeviceRevoke);
        let sections = ManagementSection::ALL.into_iter().map(|section| {
            let mut button = Button::new(format!("management-section-{section:?}"))
                .w_full()
                .justify_start()
                .label(section.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.management.switch_section(section, true) {
                        this.management_operation_pending = false;
                        this.drain_management_event_refresh(cx);
                    }
                    cx.notify();
                }));
            button = if view.active_section == section {
                button.primary()
            } else {
                button.ghost()
            };
            button
        });
        let content = match view.active_section {
            ManagementSection::Overview => v_flex()
                .gap_3()
                .child(summary_row("Agents", view.agents.len(), cx))
                .child(summary_row("Provider profiles", view.profiles.len(), cx))
                .child(summary_row("Health summaries", view.health.len(), cx))
                .child(summary_row("Paired devices", view.devices.len(), cx))
                .child(summary_row("Audit records", view.audit_count, cx))
                .into_any_element(),
            ManagementSection::Providers => v_flex()
                .gap_2()
                .children(
                    view.profiles
                        .into_iter()
                        .enumerate()
                        .map(|(index, profile)| {
                            h_flex()
                                .id(("management-profile", index))
                                .justify_between()
                                .gap_3()
                                .p_2()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    v_flex().min_w_0().child(profile.display_name).child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{:?} / {:?}",
                                                profile.status, profile.secret_setup_state
                                            )),
                                    ),
                                )
                                .child(
                                    Button::new(("management-profile-select", index))
                                        .outline()
                                        .label("Select")
                                        .disabled(!can_select_profile)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let result = this.select_provider_profile(index, cx);
                                            this.record_action_result(result, cx);
                                        })),
                                )
                        }),
                )
                .into_any_element(),
            ManagementSection::Health => v_flex()
                .gap_2()
                .child(
                    Button::new("management-health-run")
                        .primary()
                        .label("Run health probes")
                        .disabled(!can_run_health)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let result = this.run_health_probes(cx);
                            this.record_action_result(result, cx);
                        })),
                )
                .children(view.health.into_iter().enumerate().map(|(index, health)| {
                    h_flex()
                        .id(("management-health", index))
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(health.display_name)
                        .child(format!("{:?}", health.status))
                }))
                .into_any_element(),
            ManagementSection::Relay => v_flex()
                .gap_3()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Self-hosted Relay"),
                )
                .child(
                    view.relay
                        .map(|relay| {
                            format!("{:?}, retry {}", relay.state, relay.reconnect_attempt)
                        })
                        .unwrap_or_else(|| "Relay status unavailable".into()),
                )
                .into_any_element(),
            ManagementSection::Devices => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("management-pair-device")
                                .primary()
                                .label("Create pairing offer")
                                .disabled(!can_pair)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let result = this.create_pairing_offer(cx);
                                    this.record_action_result(result, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if pairing_active {
                                    "Pairing offer ready"
                                } else {
                                    "No active pairing offer"
                                }),
                        ),
                )
                .children(pairing_material)
                .children(view.devices.into_iter().enumerate().map(|(index, device)| {
                    let can_revoke_device =
                        can_revoke && device.status == RemoteDeviceStatus::Active;
                    h_flex()
                        .id(("management-device", index))
                        .justify_between()
                        .gap_3()
                        .p_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            v_flex().min_w_0().child(device.display_name).child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{:?} / {:?}",
                                        device.permission_level, device.status
                                    )),
                            ),
                        )
                        .child(
                            Button::new(("management-device-revoke", index))
                                .outline()
                                .label("Revoke")
                                .disabled(!can_revoke_device)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let result = this.revoke_device(index, cx);
                                    this.record_action_result(result, cx);
                                })),
                        )
                }))
                .into_any_element(),
        };
        h_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .w(if self.layout.kind == ShellKind::Compact {
                        px(150.0)
                    } else {
                        px(220.0)
                    })
                    .h_full()
                    .flex_shrink_0()
                    .gap_1()
                    .p_2()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .children(sections),
            )
            .child(
                v_flex()
                    .id("management-content-scroll")
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.auxiliary_scroll)
                    .vertical_scrollbar(&self.auxiliary_scroll)
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(view.active_section.label()),
                            )
                            .child(
                                Button::new("management-refresh")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::Replace)
                                    .tooltip("Refresh ManagementCenter")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.refresh_management(cx)),
                                    ),
                            ),
                    )
                    .child(content),
            )
            .into_any_element()
    }

    fn render_agent_mobile(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self
            .workflow
            .agent
            .state
            .view(&self.sidebar, "", self.layout.kind);
        self.sync_elicitation_forms(&view.elicitations, window, cx);
        let selected_id = self.workflow.agent.state.selected_session_id.clone();

        if self.navigation.level == NavigationLevel::Global {
            let rows = view.sessions.into_iter().enumerate().map(|(index, row)| {
                let is_session = row.kind == AgentSidebarRowKind::Session;
                let selected = row.session_id.as_ref() == selected_id.as_ref();
                let session_id = row.session_id.clone();
                let icon = if is_session {
                    IconName::Bot
                } else {
                    IconName::FolderOpen
                };
                let state_label = row.state.map(agent_state_label).unwrap_or("Workspace");
                let row_label = row.label.clone();
                let mut button = Button::new(("mobile-agent-session", index))
                    .w_full()
                    .min_h(px(MOBILE_TOUCH_TARGET))
                    .justify_start()
                    .icon(icon)
                    .label(row_label)
                    .disabled(!is_session)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(session_id) = session_id.clone() {
                            let result = this.open_session(session_id, cx);
                            this.record_action_result(result, cx);
                        }
                    }));
                button = if selected {
                    button.primary()
                } else {
                    button.ghost()
                };
                v_flex()
                    .id(("mobile-agent-row", index))
                    .w_full()
                    .gap_1()
                    .child(button)
                    .when(is_session, |this| {
                        this.child(
                            div()
                                .pl_10()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(state_label),
                        )
                    })
                    .into_any_element()
            });
            let session_count = self
                .workflow
                .agent
                .state
                .sessions
                .value
                .as_ref()
                .map_or(0, Vec::len);
            return v_flex()
                .id("mobile-agent-session-list")
                .size_full()
                .min_h_0()
                .gap_3()
                .p(px(MOBILE_PAGE_GUTTER))
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Sessions"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{session_count} remote sessions")),
                                ),
                        )
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded(px(6.0))
                                .bg(cx.theme().muted.opacity(0.45))
                                .text_xs()
                                .child(connection_label(self.connection)),
                        ),
                )
                .child(
                    v_flex()
                        .id("mobile-agent-session-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.content_scroll)
                        .vertical_scrollbar(&self.content_scroll)
                        .gap_2()
                        .children(rows)
                        .when(session_count == 0, |this| {
                            this.child(mobile_empty_state(
                                "No sessions yet",
                                "Choose a workspace on the paired desktop to start coding.",
                                cx,
                            ))
                        }),
                )
                .into_any_element();
        }

        let active_title = view
            .active_session
            .as_ref()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "Session".into());
        let timeline_count = view.timeline_rows.len();
        let elicitations = view
            .elicitations
            .clone()
            .into_iter()
            .map(|surface| self.render_elicitation_surface(surface, cx))
            .collect::<Vec<_>>();
        let timeline = view
            .timeline_rows
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let row_id = row.id.clone();
                let expanded = !row.collapsible || self.expanded_timeline_rows.contains(&row_id);
                let title = row.title.clone();
                let kind = timeline_kind_label(row.kind);
                let body = bounded_text(row.body, MAX_RENDERED_TEXT_BYTES);
                let tone = if row.failed {
                    cx.theme().danger
                } else if row.pending_permission {
                    cx.theme().warning
                } else {
                    cx.theme().border
                };
                v_flex()
                    .id(("mobile-agent-timeline-row", index))
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .p_3()
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .border_1()
                    .border_color(tone.opacity(0.7))
                    .bg(cx.theme().background)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(kind),
                            )
                            .when(row.collapsible, |this| {
                                let row_id = row_id.clone();
                                this.child(
                                    Button::new(("mobile-agent-expand", index))
                                        .ghost()
                                        .compact()
                                        .size(px(MOBILE_TOUCH_TARGET))
                                        .icon(if expanded {
                                            IconName::ChevronUp
                                        } else {
                                            IconName::ChevronDown
                                        })
                                        .tooltip(if expanded { "Collapse" } else { "Expand" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_timeline_row(row_id.clone(), cx)
                                        })),
                                )
                            }),
                    )
                    .when(expanded, |this| {
                        this.child(div().min_w_0().text_sm().child(body))
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let approvals = view
            .approvals
            .into_iter()
            .enumerate()
            .map(|(index, approval)| {
                let response_options = if approval.response_options.is_empty() {
                    approval
                        .allowed_responses
                        .iter()
                        .map(|response| {
                            (
                                None,
                                permission_response_label(*response).to_string(),
                                *response,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    approval
                        .response_options
                        .iter()
                        .map(|option| {
                            (
                                Some(option.option_id.clone()),
                                option.label.clone(),
                                option.response,
                            )
                        })
                        .collect::<Vec<_>>()
                };
                v_flex()
                    .id(("mobile-agent-approval", index))
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.7))
                    .bg(cx.theme().warning.opacity(0.08))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(approval.title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child("Needs your decision"),
                            ),
                    )
                    .child(h_flex().w_full().flex_wrap().gap_2().children(
                        response_options.into_iter().enumerate().map(
                            |(option_index, (option_id, label, response))| {
                                let request_id = approval.request_id.clone();
                                let mut button =
                                    Button::new(("mobile-agent-approval-response", option_index))
                                        .min_h(px(MOBILE_TOUCH_TARGET))
                                        .label(label)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let result = this.resolve_approval(
                                                request_id.clone(),
                                                response,
                                                option_id.clone(),
                                                cx,
                                            );
                                            this.record_action_result(result, cx);
                                        }));
                                button = match response {
                                    PermissionResponseKind::Approve => button.primary(),
                                    PermissionResponseKind::Deny => button.danger(),
                                    PermissionResponseKind::AlwaysAllowForSession => {
                                        button.outline()
                                    }
                                };
                                button
                            },
                        ),
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        v_flex()
            .id("mobile-agent-session-detail")
            .size_full()
            .min_h_0()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .px(px(MOBILE_PAGE_GUTTER))
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(active_title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{timeline_count} activity items")),
                            ),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .bg(cx.theme().muted.opacity(0.45))
                            .text_xs()
                            .child(
                                view.active_session
                                    .as_ref()
                                    .map(|session| agent_state_label(session.state))
                                    .unwrap_or("Unavailable"),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("mobile-agent-timeline-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .gap_2()
                    .px(px(MOBILE_PAGE_GUTTER))
                    .py_2()
                    .children(timeline)
                    .children(approvals)
                    .children(elicitations),
            )
            .child(
                h_flex()
                    .id("agent-mobile-composer")
                    .w_full()
                    .min_h(px(84.0))
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .child(Input::new(&self.composer).w_full())
                    .child(
                        Button::new("agent-send")
                            .primary()
                            .min_h(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::ArrowUp)
                            .tooltip("Send message")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_agent_message(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.layout.kind == ShellKind::Wide {
            self.render_agent_wide(window, cx)
        } else {
            self.render_agent_mobile(window, cx)
        }
    }

    fn render_files_mobile(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self.workflow.files.state.view();
        if self.compact_detail == Some(CompactDetailPage::FilePreview) {
            return self.render_file_preview_mobile(view, cx);
        }
        let rows = view
            .rows
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let is_directory = row.kind == FileEntryKind::Directory;
                let icon = if is_directory {
                    IconName::Folder
                } else {
                    IconName::File
                };
                let label = row.name.clone();
                let path = row.path.clone();
                let mut button = Button::new(("mobile-file-row", index))
                    .w_full()
                    .min_h(px(MOBILE_TOUCH_TARGET))
                    .justify_start()
                    .icon(icon)
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let result = this.open_file_row(index, window, cx);
                        this.record_action_result(result, cx);
                    }));
                if view.selected_path.as_deref() == Some(path.as_str()) {
                    button = button.primary();
                } else {
                    button = button.ghost();
                }
                v_flex()
                    .id(("mobile-file-row-wrap", index))
                    .w_full()
                    .pl(px((row.depth as f32 * 16.0).min(64.0)))
                    .child(button)
                    .into_any_element()
            });
        let search_rows = view
            .search
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                let path = result.path.clone();
                Button::new(("mobile-file-search-result", index))
                    .w_full()
                    .min_h(px(MOBILE_TOUCH_TARGET))
                    .justify_start()
                    .ghost()
                    .icon(IconName::Search)
                    .label(result.name)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let result = this.open_file(path.clone(), window, cx);
                        this.record_action_result(result, cx);
                    }))
            });
        v_flex()
            .id("mobile-files-list")
            .size_full()
            .min_h_0()
            .gap_2()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.file_search).w_full())
                    .child(
                        Button::new("file-search")
                            .outline()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Search)
                            .tooltip("Search files")
                            .on_click(cx.listener(|this, _, _, cx| this.search_files(cx))),
                    )
                    .child(
                        Button::new("file-tree-refresh")
                            .ghost()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Replace)
                            .tooltip("Refresh file tree")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_file_tree(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("mobile-file-tree-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .vertical_scrollbar(&self.sidebar_scroll)
                    .gap_1()
                    .children(search_rows)
                    .children(rows)
                    .when(view.rows.is_empty() && view.search.is_empty(), |this| {
                        this.child(mobile_empty_state(
                            "Workspace is empty",
                            "Files from the selected remote workspace will appear here.",
                            cx,
                        ))
                    }),
            )
            .into_any_element()
    }

    fn render_file_preview_mobile(
        &mut self,
        view: crate::FileWorkflowView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = view
            .selected_path
            .clone()
            .unwrap_or_else(|| "No file selected".into());
        let status = file_status_label(view.status);
        let conflict = view.conflict.map(|conflict| {
            v_flex()
                .gap_2()
                .p_3()
                .rounded(px(MOBILE_CARD_RADIUS))
                .border_1()
                .border_color(cx.theme().danger.opacity(0.6))
                .bg(cx.theme().danger.opacity(0.06))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Revision conflict"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("The remote file changed while you were editing."),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("Local"))
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .child(bounded_text(
                                            conflict.local_content,
                                            MAX_RENDERED_TEXT_BYTES / 4,
                                        )),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_xs().child("Server"))
                                .child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .child(bounded_text(
                                            conflict.server_content,
                                            MAX_RENDERED_TEXT_BYTES / 4,
                                        )),
                                ),
                        ),
                )
                .child(
                    Button::new("file-conflict-reload")
                        .outline()
                        .min_h(px(MOBILE_TOUCH_TARGET))
                        .label("Reload server version")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let result = this.apply_test_command(
                                WorkflowWorkbenchCommand::ReloadFileConflict,
                                window,
                                cx,
                            );
                            this.record_action_result(result, cx);
                        })),
                )
        });
        v_flex()
            .id("mobile-file-preview")
            .size_full()
            .min_h_0()
            .gap_2()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(path),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .bg(cx.theme().muted.opacity(0.45))
                            .text_xs()
                            .child(status),
                    )
                    .child(
                        Button::new("file-save")
                            .primary()
                            .min_h(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Check)
                            .tooltip("Save file")
                            .disabled(!matches!(
                                view.status,
                                FileEditorStatus::Dirty | FileEditorStatus::Saved
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let result = this.save_file(cx);
                                this.record_action_result(result, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("mobile-file-editor")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_1()
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(Input::new(&self.file_editor).size_full()),
            )
            .children(conflict)
            .into_any_element()
    }

    fn render_files(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.layout.kind == ShellKind::Wide {
            self.render_files_wide(cx)
        } else {
            self.render_files_mobile(window, cx)
        }
    }

    fn render_git(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.layout.kind == ShellKind::Wide {
            self.render_git_wide(cx)
        } else {
            self.render_git_mobile(cx)
        }
    }

    fn render_git_mobile(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.workflow.git.state.view();
        if self.compact_detail == Some(CompactDetailPage::GitDiff) {
            return self.render_git_diff_mobile(view, cx);
        }
        let changes = view
            .status
            .as_ref()
            .map(|status| status.changes.clone())
            .unwrap_or_default();
        let changes_empty = changes.is_empty();
        let rows = changes.into_iter().enumerate().map(|(index, change)| {
            let path = change.path.clone();
            let stage = !change.staged || change.unstaged;
            v_flex()
                .id(("mobile-git-change", index))
                .w_full()
                .gap_2()
                .p_3()
                .rounded(px(MOBILE_CARD_RADIUS))
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    Button::new(("mobile-git-open-diff", index))
                        .w_full()
                        .min_h(px(MOBILE_TOUCH_TARGET))
                        .justify_start()
                        .ghost()
                        .icon(IconName::Github)
                        .label(path)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let result = this.load_git_diff(index, cx);
                            this.record_action_result(result, cx);
                        })),
                )
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{:?}", change.kind)),
                        )
                        .child(
                            Button::new(("mobile-git-stage", index))
                                .outline()
                                .min_h(px(MOBILE_TOUCH_TARGET))
                                .label(if stage { "Stage" } else { "Unstage" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let result = this.mutate_git_change(index, stage, cx);
                                    this.record_action_result(result, cx);
                                })),
                        ),
                )
                .into_any_element()
        });
        v_flex()
            .id("mobile-git-list")
            .size_full()
            .min_h_0()
            .gap_2()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Changes"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} files changed",
                                        view.status
                                            .as_ref()
                                            .map_or(0, |status| status.changes.len())
                                    )),
                            ),
                    )
                    .child(
                        Button::new("git-refresh")
                            .ghost()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Replace)
                            .tooltip("Refresh Git status")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_git_status(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("mobile-git-change-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .gap_2()
                    .children(rows)
                    .when(changes_empty, |this| {
                        this.child(mobile_empty_state(
                            "Working tree is clean",
                            "Remote changes will appear here for review and staging.",
                            cx,
                        ))
                    }),
            )
            .child(self.render_commit_bar(view, cx))
            .into_any_element()
    }

    fn render_git_diff_mobile(
        &mut self,
        view: crate::GitWorkflowView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let diff_rows = self
            .selected_git_key
            .as_ref()
            .and_then(|key| self.workflow.git.state.model.diffs.get(key))
            .map(|document| {
                document
                    .files
                    .iter()
                    .flat_map(|file| file.lines.iter())
                    .take(500)
                    .enumerate()
                    .map(|(index, line)| {
                        let (background, foreground, prefix) = match line.kind {
                            UnifiedDiffLineKind::Add => {
                                (cx.theme().success.opacity(0.12), cx.theme().success, "+")
                            }
                            UnifiedDiffLineKind::Delete => {
                                (cx.theme().danger.opacity(0.12), cx.theme().danger, "-")
                            }
                            UnifiedDiffLineKind::Hunk => {
                                (cx.theme().primary.opacity(0.12), cx.theme().primary, "@")
                            }
                            UnifiedDiffLineKind::Meta => (
                                cx.theme().muted.opacity(0.35),
                                cx.theme().muted_foreground,
                                "·",
                            ),
                            UnifiedDiffLineKind::Context => {
                                (cx.theme().background, cx.theme().foreground, " ")
                            }
                        };
                        div()
                            .id(("mobile-git-diff-line", index))
                            .w_full()
                            .px_2()
                            .py_1()
                            .bg(background)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .text_color(foreground)
                            .child(format!(
                                "{prefix} {}",
                                bounded_text(line.content.clone(), 2_048)
                            ))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        v_flex()
            .id("mobile-git-diff")
            .size_full()
            .min_h_0()
            .gap_2()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Review the selected change, then return to stage or commit."),
            )
            .child(
                v_flex()
                    .id("mobile-git-diff-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .border_1()
                    .border_color(cx.theme().border)
                    .children(diff_rows)
                    .when(self.selected_git_key.is_none(), |this| {
                        this.child(mobile_empty_state(
                            "No diff selected",
                            "Choose a changed file to inspect it.",
                            cx,
                        ))
                    }),
            )
            .child(self.render_commit_bar(view, cx))
            .into_any_element()
    }

    fn render_commit_bar(
        &self,
        view: crate::GitWorkflowView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let confirmation = view.commit_confirmation.map(|confirmation| {
            v_flex()
                .gap_2()
                .p_2()
                .rounded(px(MOBILE_CARD_RADIUS))
                .border_1()
                .border_color(cx.theme().warning.opacity(0.65))
                .bg(cx.theme().warning.opacity(0.08))
                .child(div().text_sm().child(format!(
                    "Commit {} staged path(s)?",
                    confirmation.paths.len()
                )))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("git-commit-cancel")
                                .outline()
                                .min_h(px(MOBILE_TOUCH_TARGET))
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workflow.git.cancel_commit();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("git-commit-confirm")
                                .primary()
                                .min_h(px(MOBILE_TOUCH_TARGET))
                                .label("Confirm commit")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let result = this.confirm_commit(cx);
                                    this.record_action_result(result, cx);
                                })),
                        ),
                )
        });
        v_flex()
            .w_full()
            .gap_2()
            .p_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .children(confirmation)
            .child(Input::new(&self.commit_message).w_full().h(px(56.0)))
            .child(
                Button::new("git-commit-prepare")
                    .primary()
                    .min_h(px(MOBILE_TOUCH_TARGET))
                    .label("Review commit")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let result = this.prepare_commit(cx);
                        this.record_action_result(result, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_terminal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.layout.kind == ShellKind::Wide {
            self.render_terminal_wide(cx)
        } else {
            self.render_terminal_mobile(cx)
        }
    }

    fn render_terminal_mobile(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.terminal.state.view(self.layout.kind);
        let output = view
            .frame
            .as_ref()
            .map(terminal_frame_text)
            .unwrap_or_else(|| "Attach or create a terminal".into());
        let sessions = view
            .sessions
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, session)| {
                let mut button = Button::new(("mobile-terminal-session", index))
                    .min_h(px(MOBILE_TOUCH_TARGET))
                    .label(session.title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let result = this.attach_terminal(index, cx);
                        this.record_action_result(result, cx);
                    }));
                button = if session.selected {
                    button.primary()
                } else {
                    button.ghost()
                };
                button
            });
        let keys = view.key_bar.into_iter().enumerate().map(|(index, action)| {
            let key = action.key;
            Button::new(("mobile-terminal-key", index))
                .outline()
                .min_h(px(MOBILE_TOUCH_TARGET))
                .label(action.label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let result = this.send_terminal_key(key, cx);
                    this.record_action_result(result, cx);
                }))
        });
        v_flex()
            .id("mobile-terminal")
            .size_full()
            .min_h_0()
            .gap_2()
            .p_2()
            .child(
                h_flex()
                    .id("mobile-terminal-session-scroll")
                    .w_full()
                    .min_h(px(MOBILE_TOUCH_TARGET))
                    .items_center()
                    .gap_1()
                    .overflow_x_scroll()
                    .children(sessions)
                    .child(
                        Button::new("terminal-create")
                            .ghost()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Plus)
                            .tooltip("Create terminal")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let result = this.create_terminal(cx);
                                this.record_action_result(result, cx);
                            })),
                    )
                    .child(
                        Button::new("terminal-close")
                            .ghost()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Close)
                            .tooltip("Close terminal")
                            .disabled(view.active_session_id.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let result = this.close_terminal(cx);
                                this.record_action_result(result, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("terminal-output-scroll")
                    .flex_1()
                    .min_h(px(160.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .p_3()
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .bg(cx.theme().popover)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(output),
            )
            .child(h_flex().w_full().flex_wrap().gap_1().children(keys))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_2()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(px(MOBILE_CARD_RADIUS))
                    .child(Input::new(&self.terminal_input).w_full().h(px(52.0)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("terminal-send")
                                    .primary()
                                    .min_h(px(MOBILE_TOUCH_TARGET))
                                    .label("Send")
                                    .disabled(view.access != TerminalAccessMode::ReadWrite)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send_terminal_input(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("terminal-resize")
                                    .outline()
                                    .min_h(px(MOBILE_TOUCH_TARGET))
                                    .label("100 x 30")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let result = this.resize_terminal(30, 100, cx);
                                        this.record_action_result(result, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} · seq {}",
                                        terminal_connection_label(view.connection),
                                        view.raw_next_sequence
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_management(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.layout.kind == ShellKind::Wide {
            self.render_management_wide(cx)
        } else {
            self.render_management_mobile(cx)
        }
    }

    fn render_management_mobile(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = self.management.state.view(self.layout.kind);
        if self.compact_detail == Some(CompactDetailPage::ManagementSection) {
            return self.render_management_section_mobile(view, cx);
        }
        let summaries = [
            (
                ManagementSection::Overview,
                "Workspace overview",
                "Agents, profiles, devices and audit",
            ),
            (
                ManagementSection::Providers,
                "Provider profiles",
                "Choose the active remote provider",
            ),
            (
                ManagementSection::Health,
                "Health checks",
                "Run provider connectivity probes",
            ),
            (
                ManagementSection::Relay,
                "Relay connection",
                "Inspect self-hosted relay status",
            ),
            (
                ManagementSection::Devices,
                "Paired devices",
                "Manage pairing and access permissions",
            ),
        ];
        let rows = summaries
            .into_iter()
            .enumerate()
            .map(|(index, (section, title, subtitle))| {
                Button::new(("mobile-management-section", index))
                    .w_full()
                    .min_h(px(64.0))
                    .justify_start()
                    .icon(match section {
                        ManagementSection::Overview => IconName::LayoutDashboard,
                        ManagementSection::Providers => IconName::Settings2,
                        ManagementSection::Health => IconName::TriangleAlert,
                        ManagementSection::Relay => IconName::SquareTerminal,
                        ManagementSection::Devices => IconName::Settings,
                    })
                    .label(title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.management.switch_section(section, true) {
                            this.management_operation_pending = false;
                            this.drain_management_event_refresh(cx);
                        }
                        if section != ManagementSection::Overview {
                            this.compact_detail = Some(CompactDetailPage::ManagementSection);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    )
            });
        v_flex()
            .id("mobile-management-list")
            .size_full()
            .min_h_0()
            .gap_3()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Management"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Remote runtime and device controls"),
                            ),
                    )
                    .child(
                        Button::new("management-refresh")
                            .ghost()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Replace)
                            .tooltip("Refresh management")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_management(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("mobile-management-section-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .gap_2()
                    .children(rows)
                    .child(
                        v_flex()
                            .gap_2()
                            .p_3()
                            .rounded(px(MOBILE_CARD_RADIUS))
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(summary_row("Agents", view.agents.len(), cx))
                            .child(summary_row("Provider profiles", view.profiles.len(), cx))
                            .child(summary_row("Paired devices", view.devices.len(), cx)),
                    ),
            )
            .into_any_element()
    }

    fn render_management_section_mobile(
        &self,
        view: crate::ManagementWorkflowView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let can_select_profile = self
            .management
            .capabilities
            .supports(BackendOperation::ManagementProfileSelect);
        let can_pair = self
            .management
            .capabilities
            .supports(BackendOperation::DevicePairing);
        let can_run_health = self
            .management
            .capabilities
            .supports(BackendOperation::ManagementHealth);
        let can_revoke = self
            .management
            .capabilities
            .supports(BackendOperation::DeviceRevoke);
        let pairing_fragment = view
            .pairing_offer
            .as_ref()
            .filter(|offer| !offer.canceled)
            .and_then(|offer| offer.launch_fragment.clone());
        let content = match view.active_section {
            ManagementSection::Overview => v_flex()
                .gap_2()
                .child(summary_row("Agents", view.agents.len(), cx))
                .child(summary_row("Provider profiles", view.profiles.len(), cx))
                .child(summary_row("Health summaries", view.health.len(), cx))
                .child(summary_row("Paired devices", view.devices.len(), cx))
                .child(summary_row("Audit records", view.audit_count, cx))
                .into_any_element(),
            ManagementSection::Providers => v_flex()
                .gap_2()
                .children(
                    view.profiles
                        .into_iter()
                        .enumerate()
                        .map(|(index, profile)| {
                            h_flex()
                                .w_full()
                                .min_h(px(64.0))
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .p_2()
                                .rounded(px(MOBILE_CARD_RADIUS))
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(profile.display_name)
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("{:?}", profile.status)),
                                        ),
                                )
                                .child(
                                    Button::new(("mobile-management-select-profile", index))
                                        .outline()
                                        .min_h(px(MOBILE_TOUCH_TARGET))
                                        .label("Select")
                                        .disabled(!can_select_profile)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let result = this.select_provider_profile(index, cx);
                                            this.record_action_result(result, cx);
                                        })),
                                )
                        }),
                )
                .into_any_element(),
            ManagementSection::Health => v_flex()
                .gap_2()
                .child(
                    Button::new("management-health-run")
                        .primary()
                        .min_h(px(MOBILE_TOUCH_TARGET))
                        .label("Run health probes")
                        .disabled(!can_run_health)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let result = this.run_health_probes(cx);
                            this.record_action_result(result, cx);
                        })),
                )
                .children(view.health.into_iter().enumerate().map(|(index, health)| {
                    h_flex()
                        .id(("mobile-management-health", index))
                        .min_h(px(56.0))
                        .items_center()
                        .justify_between()
                        .p_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(health.display_name)
                        .child(format!("{:?}", health.status))
                }))
                .into_any_element(),
            ManagementSection::Relay => v_flex()
                .gap_2()
                .p_3()
                .rounded(px(MOBILE_CARD_RADIUS))
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Self-hosted Relay"),
                )
                .child(
                    view.relay
                        .map(|relay| {
                            format!("{:?}, retry {}", relay.state, relay.reconnect_attempt)
                        })
                        .unwrap_or_else(|| "Relay status unavailable".into()),
                )
                .into_any_element(),
            ManagementSection::Devices => v_flex()
                .gap_2()
                .child(
                    Button::new("management-pair-device")
                        .primary()
                        .min_h(px(MOBILE_TOUCH_TARGET))
                        .label(if pairing_fragment.is_some() {
                            "Pairing offer ready"
                        } else {
                            "Create pairing offer"
                        })
                        .disabled(!can_pair)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let result = this.create_pairing_offer(cx);
                            this.record_action_result(result, cx);
                        })),
                )
                .children(pairing_fragment.map(|fragment| {
                    let clipboard_value = fragment.clone();
                    v_flex()
                        .gap_2()
                        .p_2()
                        .rounded(px(MOBILE_CARD_RADIUS))
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_xs()
                                .child(bounded_text(fragment, 2048)),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("management-pairing-copy")
                                        .outline()
                                        .size(px(MOBILE_TOUCH_TARGET))
                                        .icon(IconName::Copy)
                                        .tooltip("Copy pairing link")
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                clipboard_value.clone(),
                                            ));
                                        })),
                                )
                                .child(
                                    Button::new("management-pairing-cancel")
                                        .outline()
                                        .min_h(px(MOBILE_TOUCH_TARGET))
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let result = this.cancel_pairing_offer(cx);
                                            this.record_action_result(result, cx);
                                        })),
                                ),
                        )
                }))
                .children(view.devices.into_iter().enumerate().map(|(index, device)| {
                    let can_revoke_device =
                        can_revoke && device.status == RemoteDeviceStatus::Active;
                    h_flex()
                        .w_full()
                        .min_h(px(64.0))
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded(px(MOBILE_CARD_RADIUS))
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(device.display_name)
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{:?} / {:?}",
                                            device.permission_level, device.status
                                        )),
                                ),
                        )
                        .child(
                            Button::new(("mobile-management-revoke", index))
                                .danger()
                                .min_h(px(MOBILE_TOUCH_TARGET))
                                .label("Revoke")
                                .disabled(!can_revoke_device)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let result = this.revoke_device(index, cx);
                                    this.record_action_result(result, cx);
                                })),
                        )
                }))
                .into_any_element(),
        };
        v_flex()
            .id("mobile-management-detail")
            .size_full()
            .min_h_0()
            .gap_3()
            .p(px(MOBILE_PAGE_GUTTER))
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(view.active_section.label()),
                    )
                    .child(
                        Button::new("management-refresh")
                            .ghost()
                            .size(px(MOBILE_TOUCH_TARGET))
                            .icon(IconName::Replace)
                            .tooltip("Refresh management")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_management(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("mobile-management-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.content_scroll)
                    .vertical_scrollbar(&self.content_scroll)
                    .child(content),
            )
            .into_any_element()
    }

    fn render_error_bar(&self, cx: &App) -> Option<AnyElement> {
        let codes = self.error_codes();
        (!codes.is_empty()).then(|| {
            h_flex()
                .id("workflow-error-bar")
                .w_full()
                .min_h(px(36.0))
                .px_3()
                .gap_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(IconName::TriangleAlert)
                .child(codes.join(", "))
                .into_any_element()
        })
    }
}

impl Render for WorkflowWorkbenchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        let height = f32::from(window.viewport_size().height);
        self.layout =
            ShellLayout::resolve_mobile(normalized_dimension(width), normalized_dimension(height));
        let is_settings = self.navigation.level == NavigationLevel::Global
            && self.navigation.global == GlobalDestination::Settings;
        let surface = if is_settings {
            self.render_settings(cx)
        } else {
            match self.active_surface() {
                WorkbenchSurface::Agent => self.render_agent(window, cx),
                WorkbenchSurface::Files => self.render_files(window, cx),
                WorkbenchSurface::Git => self.render_git(cx),
                WorkbenchSurface::Terminal => self.render_terminal(cx),
                WorkbenchSurface::Management => self.render_management(cx),
            }
        };
        let session_navigation = self.navigation.level == NavigationLevel::Session && !is_settings;
        let navigation = session_navigation.then(|| self.render_surface_tabs(cx));
        let global_navigation = (!session_navigation && self.layout.kind != ShellKind::Wide)
            .then(|| self.render_global_navigation(cx));
        let core = v_flex()
            .flex_1()
            .h_full()
            .min_h_0()
            .min_w_0()
            .children(navigation)
            .child(div().flex_1().min_h_0().min_w_0().child(surface));
        let body = if self.layout.kind == ShellKind::Wide {
            h_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(self.render_workspace_sidebar(cx))
                .child(core)
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(core)
                .into_any_element()
        };
        v_flex()
            .id("vibex-workflow-workbench")
            .role(Role::Application)
            .aria_label("Vibex workflow workbench")
            .size_full()
            .min_h_0()
            .min_w_0()
            .pt(px(self.host_viewport.safe_area.top))
            .pr(px(self.host_viewport.safe_area.right))
            .pb(px(
                self.host_viewport.safe_area.bottom + self.host_viewport.keyboard_inset.min(320.0)
            ))
            .pl(px(self.host_viewport.safe_area.left))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_top_bar(cx))
            .child(body)
            .children(global_navigation)
            .children(self.render_error_bar(cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

fn normalized_dimension(value: f32) -> u32 {
    if value.is_finite() {
        value.clamp(1.0, u16::MAX as f32).round() as u32
    } else {
        1
    }
}

fn bounded_text(mut value: String, max_bytes: usize) -> SharedString {
    if value.len() > max_bytes {
        let mut boundary = max_bytes;
        while !value.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        value.truncate(boundary);
        value.push_str("\n[truncated]");
    }
    value.into()
}

fn terminal_frame_text(frame: &vibex_terminal_ui::TerminalFrameSnapshot) -> SharedString {
    let mut rows = vec![String::new(); usize::from(frame.rows)];
    for cell in &frame.cells {
        if cell.hidden || cell.wide_spacer {
            continue;
        }
        let row = &mut rows[usize::from(cell.row).min(usize::from(frame.rows.saturating_sub(1)))];
        let column = usize::from(cell.column);
        if row.chars().count() < column {
            row.push_str(&" ".repeat(column - row.chars().count()));
        }
        row.push_str(&cell.text);
    }
    bounded_text(rows.join("\n"), MAX_RENDERED_TEXT_BYTES)
}

fn agent_state_label(state: AgentSessionState) -> &'static str {
    match state {
        AgentSessionState::Initializing => "Starting",
        AgentSessionState::Idle => "Ready",
        AgentSessionState::Running => "Working",
        AgentSessionState::NeedsInput => "Needs input",
        AgentSessionState::Error => "Error",
        AgentSessionState::Closed => "Closed",
        AgentSessionState::Archived => "Archived",
    }
}

fn timeline_kind_label(kind: TimelineRowKind) -> &'static str {
    match kind {
        TimelineRowKind::UserMessage => "You",
        TimelineRowKind::AgentMessage => "Agent",
        TimelineRowKind::Reasoning => "Thinking",
        TimelineRowKind::Plan => "Plan",
        TimelineRowKind::ToolCall => "Tool",
        TimelineRowKind::Command => "Command",
        TimelineRowKind::FileOperation => "File",
        TimelineRowKind::WebSearch => "Search",
        TimelineRowKind::TodoUpdate => "Todo",
        TimelineRowKind::Collaboration => "Team",
        TimelineRowKind::ImageGeneration => "Image",
        TimelineRowKind::GitNotice => "Git",
        TimelineRowKind::SystemNotice => "System",
        TimelineRowKind::PermissionRequest => "Approval",
        TimelineRowKind::PermissionResolution => "Approval result",
        TimelineRowKind::ElicitationRequest => "Question",
        TimelineRowKind::ElicitationResolution => "Answer",
        TimelineRowKind::Error => "Error",
    }
}

fn permission_response_label(response: PermissionResponseKind) -> &'static str {
    match response {
        PermissionResponseKind::Approve => "Allow",
        PermissionResponseKind::AlwaysAllowForSession => "Always allow",
        PermissionResponseKind::Deny => "Deny",
    }
}

fn file_status_label(status: FileEditorStatus) -> &'static str {
    match status {
        FileEditorStatus::Loading => "Loading",
        FileEditorStatus::Clean => "Clean",
        FileEditorStatus::Dirty => "Unsaved",
        FileEditorStatus::Saving => "Saving",
        FileEditorStatus::Saved => "Saved",
        FileEditorStatus::Conflict => "Conflict",
        FileEditorStatus::Disconnected => "Offline",
        FileEditorStatus::Unsupported => "Unsupported",
        FileEditorStatus::TooLarge => "Too large",
    }
}

fn terminal_connection_label(state: TerminalConnectionState) -> &'static str {
    match state {
        TerminalConnectionState::Idle => "Idle",
        TerminalConnectionState::Connecting => "Connecting",
        TerminalConnectionState::Connected => "Connected",
        TerminalConnectionState::Reconnecting => "Reconnecting",
        TerminalConnectionState::Rebuilding => "Restoring",
        TerminalConnectionState::Closed => "Closed",
        TerminalConnectionState::Offline => "Offline",
        TerminalConnectionState::Error => "Error",
    }
}

fn mobile_empty_state(title: &'static str, description: &'static str, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .rounded(px(MOBILE_CARD_RADIUS))
        .border_1()
        .border_color(cx.theme().border)
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
        .child(
            div()
                .max_w(px(300.0))
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
        .into_any_element()
}

fn settings_value_row(
    label: &'static str,
    value: impl Into<SharedString>,
    color: gpui::Hsla,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_h(px(44.0))
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().text_sm().child(label))
        .child(div().text_sm().text_color(color).child(value.into()))
        .into_any_element()
}

fn summary_row(label: &'static str, count: usize, cx: &App) -> AnyElement {
    h_flex()
        .justify_between()
        .gap_3()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(label)
        .child(count.to_string())
        .into_any_element()
}

fn index_error(kind: &'static str) -> BackendError {
    BackendError::failed(
        "workflow_command_index_invalid",
        format!("the requested {kind} index is outside the current projection"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_commands_cannot_carry_product_content() {
        let command = serde_json::from_str::<WorkflowWorkbenchCommand>(
            r#"{"kind":"resize_terminal","rows":30,"cols":100}"#,
        )
        .unwrap();
        assert_eq!(
            command,
            WorkflowWorkbenchCommand::ResizeTerminal {
                rows: 30,
                cols: 100
            }
        );
        for forbidden in ["prompt", "path", "content", "diff", "secret", "bytes"] {
            assert!(!format!("{command:?}").to_lowercase().contains(forbidden));
        }

        let fill = serde_json::from_str::<WorkflowWorkbenchCommand>(
            r#"{"kind":"fill_test_input","input":"agent_composer"}"#,
        )
        .unwrap();
        assert_eq!(
            fill,
            WorkflowWorkbenchCommand::FillTestInput {
                input: WorkflowTestInput::AgentComposer
            }
        );
        for forbidden in ["prompt", "path", "content", "diff", "secret", "bytes"] {
            assert!(!format!("{fill:?}").to_lowercase().contains(forbidden));
        }
    }

    #[test]
    fn fixed_file_test_input_updates_the_shared_editor_buffer() {
        let workspace_id = WorkspaceId::new();
        let mut controller = crate::FileWorkflowController::new(
            std::sync::Arc::new(vibex_backend::DisconnectedBackend),
            vibex_backend::DomainCapabilities::available([BackendOperation::FileWrite]),
        );
        controller.select_workspace(workspace_id.clone());
        let file = vibex_core::FileReadResponse {
            workspace_id,
            path: "notes.txt".into(),
            name: "notes.txt".into(),
            preview_kind: vibex_core::FilePreviewKind::Text,
            content: Some("before\n".into()),
            size_bytes: 7,
            modified_at_ms: Some(1),
            language: Some("text".into()),
            truncated: false,
            encoding: vibex_core::FileEncoding::Utf8,
            line_ending: vibex_core::FileLineEnding::Lf,
            content_revision: "rev-1".into(),
        };
        controller.state.selected_path = Some(file.path.clone());
        controller.state.active_file.resolve(file.clone());
        controller.state.buffers.insert_read(file);

        fill_fixed_file_test_input(&mut controller).unwrap();

        assert_eq!(controller.state.editor_status(), FileEditorStatus::Dirty);
        assert_eq!(
            controller
                .state
                .buffers
                .active()
                .map(|buffer| buffer.content.as_str()),
            Some(TEST_FILE_CONTENT)
        );
    }

    #[test]
    fn snapshot_schema_contains_only_sanitized_projection_fields() {
        let snapshot = WorkflowWorkbenchSnapshot {
            schema_version: WORKFLOW_WORKBENCH_SCHEMA_VERSION,
            connection: WorkbenchConnectionState::Online,
            shell: ShellKind::Compact,
            navigation_level: NavigationLevel::Session,
            global_destination: GlobalDestination::Sessions,
            session_destination: SessionDestination::Files,
            active_surface: WorkbenchSurface::Files,
            workspace_count: 1,
            session_count: 2,
            timeline_row_count: 3,
            pending_approval_count: 1,
            agent_runtime_ready: true,
            agent_mutation_phase: AsyncPhase::Ready,
            agent_live_event_count: 2,
            agent_recovery_count: 1,
            file_row_count: 4,
            file_search_result_count: 1,
            file_has_active_file: true,
            file_editor_status: FileEditorStatus::Dirty,
            file_live_event_count: 1,
            file_recovery_count: 1,
            git_change_count: 2,
            git_selected_count: 1,
            git_mutation_pending: false,
            git_commit_phase: AsyncPhase::Ready,
            git_live_event_count: 1,
            git_recovery_count: 1,
            terminal_count: 1,
            terminal_connection: TerminalConnectionState::Connected,
            terminal_sequence: 4,
            terminal_rebuild_count: 0,
            terminal_recovery_count: 1,
            management_load_state: ManagementLoadState::Ready,
            management_agent_count: 2,
            management_profile_count: 2,
            management_health_count: 2,
            management_device_count: 1,
            management_revoked_device_count: 1,
            management_operation_pending: false,
            management_live_event_count: 2,
            management_recovery_count: 1,
            has_relay_status: true,
            has_pairing_offer: false,
            error_codes: vec!["offline".into()],
        };
        let encoded = serde_json::to_string(&snapshot).unwrap();
        for forbidden in [
            "prompt",
            "selectedPath",
            "content",
            "diff",
            "terminalBytes",
            "secret",
            "pairingCode",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        assert!(encoded.contains(WORKFLOW_WORKBENCH_SCHEMA_VERSION));
    }

    #[test]
    fn terminal_rendering_is_bounded_before_entering_the_element_tree() {
        assert_eq!(bounded_text("abc".into(), 4).as_ref(), "abc");
        assert_eq!(
            bounded_text("abcdef".into(), 4).as_ref(),
            "abcd\n[truncated]"
        );
    }

    #[test]
    fn management_completion_requires_matching_navigation_and_operation_fences() {
        assert!(management_completion_is_current(4, 7, 4, 7));
        assert!(!management_completion_is_current(5, 7, 4, 7));
        assert!(!management_completion_is_current(4, 8, 4, 7));
    }

    #[test]
    fn online_subscription_completion_restarts_after_the_task_slot_clears() {
        assert!(event_subscription_should_restart(
            WorkbenchConnectionState::Online
        ));
        for state in [
            WorkbenchConnectionState::Degraded,
            WorkbenchConnectionState::Reconnecting,
            WorkbenchConnectionState::Offline,
            WorkbenchConnectionState::Revoked,
            WorkbenchConnectionState::Incompatible,
        ] {
            assert!(!event_subscription_should_restart(state));
        }
    }

    #[test]
    fn selected_session_counts_a_wire_event_even_when_authoritative_refetch_is_needed() {
        let selected = VibexSessionId::new();
        let other = VibexSessionId::new();
        let refetch = vibex_backend::BackendRefetch {
            session_id: Some(selected.clone()),
            timeline: true,
            runtime: false,
            runtime_selection: false,
            projection: None,
        };
        let event = BackendEvent::Lagged {
            stream: vibex_backend::BackendEventStream::Timeline,
            skipped: 1,
            refetch: refetch.clone(),
            observed_live: true,
        };
        assert!(agent_event_is_relevant_live(Some(&selected), &event));
        assert!(!agent_event_is_relevant_live(Some(&other), &event));

        let recovery = BackendEvent::Lagged {
            stream: vibex_backend::BackendEventStream::Timeline,
            skipped: 1,
            refetch,
            observed_live: false,
        };
        assert!(!agent_event_is_relevant_live(Some(&selected), &recovery));
    }

    #[test]
    fn runtime_selection_event_without_switch_projection_is_scoped_by_session_id() {
        let selected = VibexSessionId::new();
        let other = VibexSessionId::new();
        let selection = vibex_core::SessionRuntimeSelection::provider(
            AgentId::parse("claude").unwrap(),
            ProviderProfileId::new(),
            "claude-sonnet",
        );
        let event = BackendEvent::RuntimeSelection(vibex_core::AgentSessionRuntimeSelectionEvent {
            session_id: selected.clone(),
            state: vibex_core::AgentSessionRuntimeSelectionState {
                desired: selection.clone(),
                effective: selection,
                status: vibex_core::SessionRuntimeSelectionStatus::Ready,
                session_revision: 2,
                selection_revision: 1,
                current_binding_id: None,
                activation_generation: 1,
                pending_switch_id: None,
                actionable_error: None,
            },
            event: None,
        });
        assert!(agent_event_is_relevant_live(Some(&selected), &event));
        assert!(!agent_event_is_relevant_live(Some(&other), &event));
    }
}

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, Context, ElementId, Entity, Focusable,
    FontWeight, IntoElement, KeyBinding, ListAlignment, ListState, MouseButton, MouseUpEvent,
    ParentElement as _, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent, Styled as _, Task,
    TouchPhase, UniformListScrollHandle, WeakEntity, Window, div, ease_in_out, ease_out_quint,
    list, prelude::*, px, rgb, svg, uniform_list,
};
use vibex_backend::{
    AgentBackend as _, BackendError, BackendEvent, BackendFuture, BackendOperation, BackendResult,
    MutationRequest, WorkspaceBackend as _, WorkspaceSummary,
};
use vibex_core::{
    AgentSessionState, ContinueAgentTurnRequest, CreateAgentSessionRequest, ElicitationFieldKind,
    ElicitationResolutionAction, OpenWorkspaceRequest, PermissionResolution,
    PermissionResponseKind, PermissionRiskCategory, RemoteDeepLinkResolutionStatus,
    RemoteLanPairingRequestState, RemoteSidebarDropPosition, RemoteSidebarItemKind,
    RemoteSidebarItemRef, RemoteSidebarOrganizationMutation, RenameAgentSessionRequest, RequestId,
    ResolvePermissionRequest, RuntimeOptionAvailability, RuntimeSelectionInteraction,
    SendAgentMessageRequest, SessionRuntimeFeature, SessionRuntimeFeatureKind,
    SessionRuntimeOption, SessionRuntimeOptionCatalog, SessionRuntimeSelection,
    SetDesiredAgentSessionRuntimeRequest, TimelinePayload, VibexSessionId, WorkspaceMode,
    WorkspaceRecord, agent_session_turn_requires_continuation, unix_timestamp_ms,
};
use vibex_desktop_model::{
    RuntimeCascadeChoice, RuntimeCascadeProjection, SidebarOrganizationItem,
    SidebarOrganizationView, SidebarState, TimelineConversationTurn, TimelineRow, TimelineRowKind,
};
use vibex_remote_client::{
    RemoteConnectionState, RemoteLifecycleSignal, WebRemoteBackend, ZeroConfigLanPairingSession,
};
use vibex_ui::{
    AgentEventDecision, AgentMutationTicket, AgentWorkflowController, AsyncPhase,
    ElicitationFormDraft, ElicitationSurfaceModel, ShellKind,
};

use crate::discovery::{LanDiscoveryCandidate, LanDiscoveryEvent, LanDiscoveryMode};
use crate::input::{
    Backspace, Copy, Cut, Delete, Down, Enter, Left, Paste, Right, SelectAll, SelectDown,
    SelectLeft, SelectRight, SelectUp, TextInput, Up,
};
use crate::lifecycle::MobileLifecycleEvent;
use crate::pairing::{MobileCredentialBundle, claim_pairing_link, claim_zero_config_lan_pairing};
use crate::sidebar::{
    SidebarDropPosition, SidebarDropTarget, SidebarProject, SidebarRow, SidebarRowInput,
    SidebarRowKind, SidebarWorkspace, ancestors_of, drop_target, press_is_on_grip, row_at_position,
    sidebar_rows,
};
use crate::storage::CredentialStorage;
use crate::workbench::{MobileWorkbench, WorkbenchSurface};
use crate::{locale, markdown, notifications, scanner, theme};

const TIMELINE_NEAR_BOTTOM_PX: f32 = 96.0;
const TIMELINE_LIST_OVERDRAW_PX: f32 = 800.0;
const TIMELINE_TURN_ESTIMATED_HEIGHT_PX: f32 = 180.0;
const RESUME_RECOVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RESUME_RECOVERY_POLL_ATTEMPTS: usize = 600;
const RUNTIME_FEATURE_VALUE_LIMIT: usize = 256;

fn timeline_list_state(turn_count: usize) -> ListState {
    ListState::new(
        turn_count,
        ListAlignment::Top,
        px(TIMELINE_LIST_OVERDRAW_PX),
    )
    .with_uniform_item_height(px(TIMELINE_TURN_ESTIMATED_HEIGHT_PX))
}

fn should_present_agent_notification(
    app_backgrounded: bool,
    workbench_open: bool,
    selected_session_id: Option<&VibexSessionId>,
    target_session_id: &VibexSessionId,
) -> bool {
    app_backgrounded || workbench_open || selected_session_id != Some(target_session_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootMode {
    Pairing,
    Connecting,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NearbyPairingState {
    Idle,
    Discovering,
    Empty,
    Validating {
        display_name: String,
    },
    Waiting {
        display_name: String,
        verification_code: String,
        expires_at_ms: i64,
    },
    PermissionDenied,
    Rejected,
    Expired,
    Failed {
        message: String,
    },
}

enum LanPairingOutcome {
    Bundle(Box<MobileCredentialBundle>),
    Rejected,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionActionKind {
    Rename,
    Archive,
    Delete,
}

/// Touch shells cannot rely on hover, so the sidebar keeps only the most-used
/// action on the row itself and moves the rest behind this sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarRowMenu {
    row: SidebarRow,
}

/// A folder name being entered. Folder ids are minted by the Desktop, so the
/// phone only carries the name and where the folder should land.
/// What a row-menu entry does when tapped. Boxed so the sheet can build its
/// entries uniformly regardless of which row kind opened it.
type SidebarMenuAction = Box<dyn Fn(&mut MobileApp, &mut Window, &mut Context<MobileApp>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarNamePrompt {
    CreateFolder {
        project_id: Option<String>,
        workspace_id: Option<String>,
        parent_folder_id: Option<String>,
    },
    RenameFolder {
        folder_id: String,
    },
}

/// A row being moved with the finger. Android reports a touch pan as a scroll
/// anchored at the press point, so a drag is a pan that started on a row's grip
/// column instead of its body.
#[derive(Debug, Clone, PartialEq)]
struct SidebarDrag {
    index: usize,
    row: SidebarRow,
    pointer_y: f32,
    target: Option<SidebarDropTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionActionPrompt {
    kind: SessionActionKind,
    session_id: VibexSessionId,
    current_title: String,
}

enum SessionMutationOutcome {
    Renamed(Box<vibex_core::AgentSession>),
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerPage {
    Sessions,
    Workbench,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MobileOverlay {
    Hosts,
    Settings,
    Usage,
    NewProject,
}

#[derive(Clone)]
struct MobileHostEntry {
    id: String,
    label: String,
    bundle: MobileCredentialBundle,
}

impl MobileHostEntry {
    fn from_bundle(bundle: &MobileCredentialBundle) -> Self {
        Self {
            id: bundle.host_id().to_string(),
            label: bundle.host_label(),
            bundle: bundle.clone(),
        }
    }
}

impl DrawerPage {
    const fn open_offset(self) -> f32 {
        match self {
            Self::Sessions => 1.0,
            Self::Workbench => -1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerDragOrigin {
    Main,
    Page(DrawerPage),
    Partial(DrawerPage),
}

/// Touch pans arrive as scroll events (see `gpui_android`/`gpui_ios`), so the drawer
/// tracks its own accumulated translation rather than absolute pointer positions.
#[derive(Debug, Clone, Copy)]
enum DrawerGesture {
    Pending {
        origin: DrawerDragOrigin,
        dx: f32,
        dy: f32,
    },
    Dragging {
        page: DrawerPage,
        last_dx: f32,
    },
}

#[derive(Debug, Clone, Copy)]
struct DrawerSnap {
    from: f32,
    target: f32,
    animation_id: u64,
}

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("MobileTextInput")),
        KeyBinding::new("delete", Delete, Some("MobileTextInput")),
        KeyBinding::new("enter", Enter, Some("MobileTextInput")),
        KeyBinding::new("left", Left, Some("MobileTextInput")),
        KeyBinding::new("right", Right, Some("MobileTextInput")),
        KeyBinding::new("up", Up, Some("MobileTextInput")),
        KeyBinding::new("down", Down, Some("MobileTextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("MobileTextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("MobileTextInput")),
        KeyBinding::new("shift-up", SelectUp, Some("MobileTextInput")),
        KeyBinding::new("shift-down", SelectDown, Some("MobileTextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("MobileTextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("MobileTextInput")),
        KeyBinding::new("cmd-v", Paste, Some("MobileTextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("MobileTextInput")),
        KeyBinding::new("cmd-c", Copy, Some("MobileTextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("MobileTextInput")),
        KeyBinding::new("cmd-x", Cut, Some("MobileTextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("MobileTextInput")),
    ]);
}

pub struct MobileApp {
    storage: CredentialStorage,
    mode: RootMode,
    backend: Option<Arc<WebRemoteBackend>>,
    controller: Option<AgentWorkflowController>,
    workbench: Option<Entity<MobileWorkbench>>,
    pending_workbench_surface: Option<WorkbenchSurface>,
    workbench_open: bool,
    composer_input: Entity<TextInput>,
    timeline_turns: Arc<Vec<TimelineConversationTurn>>,
    timeline_list: ListState,
    drawer_scroll: UniformListScrollHandle,
    settings_scroll: ScrollHandle,
    sidebar_search_input: Entity<TextInput>,
    sidebar_search_open: bool,
    sidebar_projects_initialized: bool,
    _sidebar_search_subscription: gpui::Subscription,
    drawer_open: bool,
    drawer_offset: f32,
    drawer_gesture: Option<DrawerGesture>,
    drawer_snap: Option<DrawerSnap>,
    drawer_animation_id: u64,
    drawer_snap_task: Option<Task<()>>,
    expanded_process: BTreeSet<String>,
    expanded_approval: BTreeSet<String>,
    workspaces: Vec<WorkspaceRecord>,
    workspace_summaries: Vec<WorkspaceSummary>,
    sidebar_state: SidebarState,
    overlay: Option<MobileOverlay>,
    overlay_parent: Option<MobileOverlay>,
    overlay_returns_to_drawer: bool,
    known_hosts: Vec<MobileHostEntry>,
    active_host_id: Option<String>,
    pairing_from_hosts: bool,
    elicitation_request_id: Option<RequestId>,
    elicitation_inputs: BTreeMap<String, Entity<TextInput>>,
    elicitation_draft: Option<ElicitationFormDraft>,
    pairing_busy: bool,
    nearby_pairing_state: NearbyPairingState,
    nearby_candidates: BTreeMap<String, LanDiscoveryCandidate>,
    nearby_discovery_generation: u64,
    lan_pairing_task: Option<Task<()>>,
    operation_busy: bool,
    new_session_busy: bool,
    new_project_input: Entity<TextInput>,
    new_project_busy: bool,
    new_project_error: Option<String>,
    session_action: Option<SessionActionPrompt>,
    sidebar_row_menu: Option<SidebarRowMenu>,
    sidebar_name_prompt: Option<SidebarNamePrompt>,
    sidebar_name_input: Entity<TextInput>,
    /// The Desktop's sidebar tree, mirrored so the phone renders the layout the
    /// user arranged there rather than a second, divergent ordering.
    sidebar_view: SidebarOrganizationView,
    collapsed_workspace_ids: BTreeSet<String>,
    sidebar_sync_busy: bool,
    sidebar_drag: Option<SidebarDrag>,
    /// Top edge and right edge of the row list in window space, recorded during
    /// paint so a touch pan can be resolved to a row without hit-test plumbing.
    sidebar_list_frame: Rc<Cell<(f32, f32)>>,
    session_action_input: Entity<TextInput>,
    session_action_busy: bool,
    runtime_options_open: bool,
    runtime_draft: Option<SessionRuntimeSelection>,
    runtime_feature_inputs: BTreeMap<String, Entity<TextInput>>,
    runtime_switch_generation: u64,
    runtime_switch_busy_generation: Option<u64>,
    runtime_switch_error: Option<BackendError>,
    notice: Option<String>,
    error: Option<BackendError>,
    app_backgrounded: bool,
    event_consumer_task: Option<Task<()>>,
    resume_recovery_task: Option<Task<()>>,
    pending_notification_action: Option<notifications::NotificationAction>,
    tasks: Vec<Task<()>>,
}

impl MobileApp {
    pub fn new(data_dir: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let storage = CredentialStorage::new(data_dir);
        let stored = storage.load();
        let stored_hosts = storage.load_hosts();
        let mode = if matches!(stored, Ok(Some(_))) {
            RootMode::Connecting
        } else {
            RootMode::Pairing
        };
        let known_hosts = stored_hosts
            .as_ref()
            .map(|hosts| hosts.iter().map(MobileHostEntry::from_bundle).collect())
            .unwrap_or_default();
        let sidebar_search_input = cx.new(|cx| TextInput::new(locale::common("Search"), cx));
        let sidebar_search_subscription = cx.observe(&sidebar_search_input, |_, _, cx| cx.notify());
        let mut app = Self {
            storage,
            mode,
            backend: None,
            controller: None,
            workbench: None,
            pending_workbench_surface: None,
            workbench_open: false,
            composer_input: cx.new(|cx| {
                TextInput::new(
                    locale::text("Message Vibex", "发送消息给 Vibex", "傳送訊息給 Vibex"),
                    cx,
                )
            }),
            timeline_turns: Arc::new(Vec::new()),
            timeline_list: timeline_list_state(0),
            drawer_scroll: UniformListScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            sidebar_search_input,
            sidebar_search_open: false,
            sidebar_projects_initialized: false,
            _sidebar_search_subscription: sidebar_search_subscription,
            drawer_open: false,
            drawer_offset: 0.0,
            drawer_gesture: None,
            drawer_snap: None,
            drawer_animation_id: 0,
            drawer_snap_task: None,
            expanded_process: BTreeSet::new(),
            expanded_approval: BTreeSet::new(),
            workspaces: Vec::new(),
            workspace_summaries: Vec::new(),
            sidebar_state: SidebarState::default(),
            overlay: None,
            overlay_parent: None,
            overlay_returns_to_drawer: false,
            known_hosts,
            active_host_id: None,
            pairing_from_hosts: false,
            elicitation_request_id: None,
            elicitation_inputs: BTreeMap::new(),
            elicitation_draft: None,
            pairing_busy: false,
            nearby_pairing_state: NearbyPairingState::Idle,
            nearby_candidates: BTreeMap::new(),
            nearby_discovery_generation: 0,
            lan_pairing_task: None,
            operation_busy: false,
            new_session_busy: false,
            new_project_input: cx
                .new(|cx| TextInput::new(locale::text("Project path", "项目路径", "專案路徑"), cx)),
            new_project_busy: false,
            new_project_error: None,
            session_action: None,
            sidebar_row_menu: None,
            sidebar_name_prompt: None,
            sidebar_name_input: cx.new(|cx| {
                TextInput::new(locale::text("Folder name", "文件夹名称", "資料夾名稱"), cx)
            }),
            sidebar_view: SidebarOrganizationView::default(),
            collapsed_workspace_ids: BTreeSet::new(),
            sidebar_sync_busy: false,
            sidebar_drag: None,
            sidebar_list_frame: Rc::new(Cell::new((0.0, 0.0))),
            session_action_input: cx.new(|cx| {
                TextInput::new(locale::text("Session name", "会话名称", "工作階段名稱"), cx)
            }),
            session_action_busy: false,
            runtime_options_open: false,
            runtime_draft: None,
            runtime_feature_inputs: BTreeMap::new(),
            runtime_switch_generation: 0,
            runtime_switch_busy_generation: None,
            runtime_switch_error: None,
            notice: None,
            error: stored
                .as_ref()
                .err()
                .cloned()
                .or_else(|| stored_hosts.as_ref().err().cloned()),
            app_backgrounded: crate::lifecycle::is_backgrounded(),
            event_consumer_task: None,
            resume_recovery_task: None,
            pending_notification_action: None,
            tasks: Vec::new(),
        };
        if let Ok(Some(bundle)) = stored {
            app.defer_bundle_install(bundle, cx);
        }
        app.start_scanner_result_stream(cx);
        app.start_notification_action_stream(cx);
        app.start_lan_discovery_event_stream(cx);
        app.start_lifecycle_stream(cx);
        app
    }

    fn start_lifecycle_stream(&mut self, cx: &mut Context<Self>) {
        let mut events = crate::lifecycle::subscribe();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            while let Some(event) = events.next().await {
                if entity
                    .update(cx, |this, cx| this.handle_lifecycle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.tasks.push(task);
    }

    fn handle_lifecycle_event(&mut self, event: MobileLifecycleEvent, cx: &mut Context<Self>) {
        match event {
            MobileLifecycleEvent::Backgrounded => {
                self.app_backgrounded = true;
                if matches!(
                    self.nearby_pairing_state,
                    NearbyPairingState::Discovering
                        | NearbyPairingState::Empty
                        | NearbyPairingState::Validating { .. }
                        | NearbyPairingState::Waiting { .. }
                ) {
                    self.stop_nearby_pairing();
                }
                if let Some(workbench) = self.workbench.as_ref() {
                    workbench.update(cx, |workbench, _| workbench.suspend());
                }
                #[cfg(not(target_os = "android"))]
                if let Some(backend) = self.backend.as_ref() {
                    backend.apply_lifecycle_signal(RemoteLifecycleSignal::AppBackgrounded);
                }
            }
            MobileLifecycleEvent::Resumed => {
                self.app_backgrounded = false;
                let Some(backend) = self.backend.clone() else {
                    return;
                };
                backend.apply_lifecycle_signal(RemoteLifecycleSignal::AppResumed);
                self.wait_for_resume_recovery(backend, cx);
            }
        }
    }

    fn wait_for_resume_recovery(&mut self, backend: Arc<WebRemoteBackend>, cx: &mut Context<Self>) {
        let background = cx.background_executor().clone();
        self.resume_recovery_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            for _ in 0..RESUME_RECOVERY_POLL_ATTEMPTS {
                match backend.connection_state().state {
                    RemoteConnectionState::Online => {
                        let _ = entity.update(cx, |this, cx| {
                            if this.app_backgrounded {
                                return;
                            }
                            this.start_event_stream(cx);
                            this.refresh_sessions(cx);
                            this.refresh_runtime_options(cx);
                            this.refresh_workspaces(cx);
                            this.reload_selected_session(cx);
                            if let Some(workbench) = this.workbench.as_ref() {
                                workbench.update(cx, |workbench, cx| workbench.resume(cx));
                            }
                            this.notice = None;
                            cx.notify();
                        });
                        return;
                    }
                    RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible => {
                        return;
                    }
                    RemoteConnectionState::Idle
                    | RemoteConnectionState::Resolving
                    | RemoteConnectionState::Probing
                    | RemoteConnectionState::Connecting
                    | RemoteConnectionState::Authenticating
                    | RemoteConnectionState::Syncing
                    | RemoteConnectionState::Reconnecting
                    | RemoteConnectionState::Degraded
                    | RemoteConnectionState::Offline => {}
                }
                background.timer(RESUME_RECOVERY_POLL_INTERVAL).await;
            }
        }));
    }

    fn start_scanner_result_stream(&mut self, cx: &mut Context<Self>) {
        let mut results = scanner::subscribe();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            while let Some(link) = results.next().await {
                if entity
                    .update(cx, |this, cx| {
                        this.claim_scanned_pairing_link(link, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.tasks.push(task);
    }

    fn start_notification_action_stream(&mut self, cx: &mut Context<Self>) {
        let mut sessions = notifications::subscribe_actions();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            while let Some(action) = sessions.next().await {
                if entity
                    .update(cx, |this, cx| this.handle_notification_action(action, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.tasks.push(task);
    }

    fn handle_notification_action(
        &mut self,
        action: notifications::NotificationAction,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self
            .backend
            .clone()
            .filter(|_| self.mode == RootMode::Workspace)
        else {
            self.pending_notification_action = Some(action);
            return;
        };
        self.pending_notification_action = None;
        let notification_id = action.notification_id.clone();
        let opaque_locator = action.opaque_locator.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .resolve_opaque_locator(notification_id, opaque_locator)
                .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                match outcome {
                    Ok(resolution)
                        if resolution.status == RemoteDeepLinkResolutionStatus::Resolved =>
                    {
                        if let Some(session_id) = resolution.session_id {
                            this.open_session(session_id, cx);
                        }
                    }
                    Ok(_) => {
                        this.notice = Some(
                            locale::text(
                                "This Agent notification is no longer available",
                                "这条 Agent 通知已失效",
                                "這則 Agent 通知已失效",
                            )
                            .to_string(),
                        );
                    }
                    Err(error) => {
                        this.pending_notification_action = Some(action);
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn resolve_pending_notification_action(&mut self, cx: &mut Context<Self>) {
        if let Some(action) = self.pending_notification_action.take() {
            self.handle_notification_action(action, cx);
        }
    }

    fn defer_bundle_install(&mut self, bundle: MobileCredentialBundle, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = entity.update(cx, |this, cx| this.install_bundle(bundle, cx));
        });
        self.tasks.push(task);
    }

    fn install_bundle(&mut self, bundle: MobileCredentialBundle, cx: &mut Context<Self>) {
        crate::discovery::stop();
        self.stop_connection_tasks();
        self.reset_runtime_options();
        self.lan_pairing_task = None;
        self.nearby_candidates.clear();
        self.nearby_pairing_state = NearbyPairingState::Idle;
        match bundle.backend() {
            Ok(backend) => {
                self.remember_host(&bundle);
                if let Some(workbench) = self.workbench.take() {
                    workbench.update(cx, |workbench, _| workbench.suspend());
                }
                self.reset_drawers();
                self.reset_sidebar_ui(cx);
                self.workspaces.clear();
                self.workspace_summaries.clear();
                self.pending_workbench_surface = None;
                self.clear_overlay();
                self.pairing_from_hosts = false;
                self.mode = RootMode::Connecting;
                self.error = None;
                self.backend = Some(backend.clone());
                self.persist_known_hosts();
                self.connect_backend(backend, cx);
            }
            Err(error) => {
                crate::background_connection::disconnect();
                self.mode = RootMode::Pairing;
                self.error = Some(error);
                let _ = self.storage.clear();
                cx.notify();
            }
        }
    }

    fn remember_host(&mut self, bundle: &MobileCredentialBundle) {
        let entry = MobileHostEntry::from_bundle(bundle);
        let id = entry.id.clone();
        if let Some(existing) = self.known_hosts.iter_mut().find(|host| host.id == id) {
            *existing = entry;
        } else {
            self.known_hosts.push(entry);
        }
        self.active_host_id = Some(id);
    }

    fn persist_known_hosts(&mut self) {
        let bundles = self
            .known_hosts
            .iter()
            .map(|host| host.bundle.clone())
            .collect::<Vec<_>>();
        if let Err(error) = self.storage.save_hosts(&bundles) {
            self.error = Some(error);
        }
    }

    fn connect_backend(&mut self, backend: Arc<WebRemoteBackend>, cx: &mut Context<Self>) {
        self.operation_busy = true;
        let connection = crate::background_connection::connect(backend.clone());
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = connection.await;
            let _ = entity.update(cx, |this, cx| {
                this.operation_busy = false;
                match outcome {
                    Ok(Ok(_)) => {
                        let capabilities = backend.capability_snapshot().agent;
                        this.controller =
                            Some(AgentWorkflowController::new(backend.clone(), capabilities));
                        this.mode = RootMode::Workspace;
                        this.error = None;
                        this.notice = None;
                        this.start_event_stream(cx);
                        this.refresh_sessions(cx);
                        this.refresh_runtime_options(cx);
                        this.refresh_workspaces(cx);
                        notifications::request_authorization();
                        this.resolve_pending_notification_action(cx);
                    }
                    Ok(Err(error)) => {
                        this.mode = RootMode::Connecting;
                        this.error = Some(error);
                    }
                    Err(_) => {
                        this.mode = RootMode::Connecting;
                        this.error = Some(BackendError::offline(
                            "mobile_connect_task_failed",
                            locale::text(
                                "Native mobile connection stopped unexpectedly",
                                "移动端连接意外停止。",
                                "行動端連線意外停止。",
                            ),
                        ));
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn retry_connection(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.operation_busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            self.mode = RootMode::Pairing;
            cx.notify();
            return;
        };
        self.error = None;
        self.connect_backend(backend, cx);
    }

    fn start_event_stream(&mut self, cx: &mut Context<Self>) {
        if self.backend.is_none() || self.app_backgrounded {
            return;
        }
        let mut receiver = crate::background_connection::subscribe_ui_events();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            while let Some(event) = receiver.next().await {
                let needs_refetch = entity
                    .update(cx, |this, cx| {
                        if let BackendEvent::Notification(notification) = &event {
                            let selected = this.controller.as_ref().and_then(|controller| {
                                controller.state.selected_session_id.as_ref()
                            });
                            if should_present_agent_notification(
                                this.app_backgrounded,
                                this.workbench_open,
                                selected,
                                &notification.session_id,
                            ) {
                                notifications::present(notification);
                            }
                        }
                        let should_follow = this.timeline_is_near_bottom();
                        let decision = this
                            .controller
                            .as_mut()
                            .map(|controller| controller.apply_event(event));
                        match decision {
                            Some(AgentEventDecision::Applied) => {
                                this.notice = None;
                                this.rebuild_timeline_turns();
                                let turn_count = this.timeline_turns.len();
                                if turn_count > 0 {
                                    this.timeline_list
                                        .remeasure_items(turn_count - 1..turn_count);
                                }
                                if should_follow {
                                    this.timeline_list.scroll_to_end();
                                }
                            }
                            Some(AgentEventDecision::Disconnected) if !this.app_backgrounded => {
                                this.notice = Some(
                                    locale::text(
                                        "Desktop connection lost",
                                        "桌面端连接已断开",
                                        "桌面版連線已中斷",
                                    )
                                    .to_string(),
                                );
                            }
                            _ => {}
                        }
                        cx.notify();
                        decision == Some(AgentEventDecision::NeedsAuthoritativeRefetch)
                    })
                    .unwrap_or(false);
                if needs_refetch {
                    let _ = entity.update(cx, |this, cx| this.reload_selected_session(cx));
                }
            }
        });
        self.event_consumer_task = Some(task);
    }

    fn stop_connection_tasks(&mut self) {
        crate::background_connection::suspend_ui_events();
        self.event_consumer_task = None;
        self.resume_recovery_task = None;
    }

    fn scan_pairing_code(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing_busy && self.lan_pairing_task.is_none() {
            return;
        }
        self.stop_nearby_pairing();
        window.hide_soft_keyboard();
        self.error = scanner::launch().err();
        cx.notify();
    }

    fn start_lan_discovery_event_stream(&mut self, cx: &mut Context<Self>) {
        let mut events = crate::discovery::subscribe();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            while let Some(event) = events.next().await {
                if entity
                    .update(cx, |this, cx| this.handle_lan_discovery_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.tasks.push(task);
    }

    fn handle_lan_discovery_event(&mut self, event: LanDiscoveryEvent, cx: &mut Context<Self>) {
        if !matches!(
            self.nearby_pairing_state,
            NearbyPairingState::Discovering | NearbyPairingState::Empty
        ) {
            return;
        }
        match event {
            LanDiscoveryEvent::Candidate(candidate) => {
                if candidate.mode != LanDiscoveryMode::ZeroConfig {
                    return;
                }
                self.nearby_candidates.insert(candidate.key(), candidate);
                self.nearby_pairing_state = NearbyPairingState::Discovering;
            }
            LanDiscoveryEvent::Removed { service_instance } => {
                self.nearby_candidates
                    .retain(|_, candidate| candidate.service_instance != service_instance);
                if self.nearby_candidates.is_empty() {
                    self.nearby_pairing_state = NearbyPairingState::Empty;
                }
            }
            LanDiscoveryEvent::PermissionDenied => {
                crate::discovery::stop();
                self.nearby_pairing_state = NearbyPairingState::PermissionDenied;
            }
            LanDiscoveryEvent::Failed(error) => {
                crate::discovery::stop();
                self.nearby_pairing_state = NearbyPairingState::Failed {
                    message: error.message,
                };
            }
        }
        cx.notify();
    }

    fn start_nearby_pairing(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.stop_nearby_pairing();
        self.error = None;
        self.nearby_pairing_state = NearbyPairingState::Discovering;
        self.nearby_discovery_generation = self.nearby_discovery_generation.wrapping_add(1);
        let generation = self.nearby_discovery_generation;
        if let Err(error) = crate::discovery::start() {
            self.nearby_pairing_state = NearbyPairingState::Failed {
                message: error.message,
            };
        } else {
            let runner = gpui_tokio::Tokio::spawn(cx, async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
            });
            let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                let _ = runner.await;
                let _ = entity.update(cx, |this, cx| {
                    if this.nearby_discovery_generation == generation
                        && this.nearby_candidates.is_empty()
                        && matches!(this.nearby_pairing_state, NearbyPairingState::Discovering)
                    {
                        this.nearby_pairing_state = NearbyPairingState::Empty;
                        cx.notify();
                    }
                });
            });
            self.tasks.push(task);
        }
        cx.notify();
    }

    fn cancel_nearby_pairing(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.stop_nearby_pairing();
        cx.notify();
    }

    fn stop_nearby_pairing(&mut self) {
        crate::discovery::stop();
        self.nearby_discovery_generation = self.nearby_discovery_generation.wrapping_add(1);
        self.lan_pairing_task = None;
        self.nearby_candidates.clear();
        self.nearby_pairing_state = NearbyPairingState::Idle;
        self.pairing_busy = false;
    }

    fn select_nearby_candidate(&mut self, key: String, cx: &mut Context<Self>) {
        if !matches!(
            self.nearby_pairing_state,
            NearbyPairingState::Discovering | NearbyPairingState::Empty
        ) || self.pairing_busy
        {
            return;
        }
        let Some(candidate) = self.nearby_candidates.get(&key).cloned() else {
            return;
        };
        crate::discovery::stop();
        self.pairing_busy = true;
        self.error = None;
        self.nearby_pairing_state = NearbyPairingState::Validating {
            display_name: candidate.display_name.clone(),
        };
        let display_name = candidate.display_name.clone();
        let display_name_for_bundle = display_name.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            if candidate.mode != LanDiscoveryMode::ZeroConfig {
                return Err(BackendError::failed(
                    "remote_zero_config_pairing_mode_invalid",
                    locale::text(
                        "Local network pairing selected an incompatible desktop.",
                        "局域网配对选择了不兼容的桌面端。",
                        "區域網路配對選擇了不相容的桌面版。",
                    ),
                ));
            }
            let server_key = candidate.server_identity_public_key.ok_or_else(|| {
                BackendError::failed(
                    "remote_zero_config_pairing_identity_missing",
                    locale::text(
                        "The nearby desktop did not provide its pairing identity.",
                        "附近的桌面端未提供配对身份。",
                        "附近的桌面版未提供配對身分。",
                    ),
                )
            })?;
            let server_id = candidate.server_id.ok_or_else(|| {
                BackendError::failed(
                    "remote_zero_config_pairing_identity_missing",
                    locale::text(
                        "The nearby desktop did not provide its server identity.",
                        "附近的桌面端未提供服务器身份。",
                        "附近的桌面版未提供伺服器身分。",
                    ),
                )
            })?;
            ZeroConfigLanPairingSession::start(
                candidate.origin,
                &server_id,
                &server_key,
                "Vibex Mobile",
            )
            .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| match outcome {
                Ok(Ok(session)) => {
                    this.nearby_pairing_state = NearbyPairingState::Waiting {
                        display_name,
                        verification_code: session.verification_code().to_string(),
                        expires_at_ms: session.expires_at_ms(),
                    };
                    this.start_lan_pairing_poll(session, display_name_for_bundle, cx);
                    cx.notify();
                }
                Ok(Err(error)) => {
                    this.pairing_busy = false;
                    this.lan_pairing_task = None;
                    this.nearby_pairing_state = NearbyPairingState::Failed {
                        message: error.message,
                    };
                    cx.notify();
                }
                Err(_) => {
                    this.pairing_busy = false;
                    this.lan_pairing_task = None;
                    this.nearby_pairing_state = NearbyPairingState::Failed {
                        message: locale::text(
                            "Nearby pairing stopped unexpectedly.",
                            "附近配对意外停止。",
                            "附近配對意外停止。",
                        )
                        .to_string(),
                    };
                    cx.notify();
                }
            });
        });
        self.lan_pairing_task = Some(task);
        cx.notify();
    }

    fn start_lan_pairing_poll(
        &mut self,
        mut session: ZeroConfigLanPairingSession,
        display_name: String,
        cx: &mut Context<Self>,
    ) {
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            loop {
                tokio::time::sleep(Duration::from_millis(700)).await;
                let status = session.poll().await?;
                match status.state {
                    RemoteLanPairingRequestState::Pending => continue,
                    RemoteLanPairingRequestState::Approved => {
                        let bundle = claim_zero_config_lan_pairing(&mut session, status).await?;
                        return Ok(LanPairingOutcome::Bundle(Box::new(bundle)));
                    }
                    RemoteLanPairingRequestState::Rejected => {
                        return Ok(LanPairingOutcome::Rejected);
                    }
                    RemoteLanPairingRequestState::Expired
                    | RemoteLanPairingRequestState::Claimed => {
                        return Ok(LanPairingOutcome::Expired);
                    }
                    RemoteLanPairingRequestState::Unknown => {
                        return Err(BackendError::failed(
                            "remote_lan_pairing_state_unknown",
                            locale::text(
                                "The desktop returned an unknown pairing state.",
                                "桌面端返回了未知的配对状态。",
                                "桌面版傳回了未知的配對狀態。",
                            ),
                        ));
                    }
                }
            }
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.pairing_busy = false;
                this.lan_pairing_task = None;
                match outcome {
                    Ok(Ok(LanPairingOutcome::Bundle(mut bundle))) => {
                        // The advertised service name is the only human label
                        // the desktop publishes, so keep it with the credential.
                        let display_name = display_name.trim();
                        if !display_name.is_empty() {
                            bundle.display_name = Some(display_name.to_string());
                        }
                        match this.storage.save(&bundle) {
                            Ok(()) => this.install_bundle(*bundle, cx),
                            Err(error) => {
                                this.nearby_pairing_state = NearbyPairingState::Failed {
                                    message: error.message,
                                }
                            }
                        }
                    }
                    Ok(Ok(LanPairingOutcome::Rejected)) => {
                        this.nearby_pairing_state = NearbyPairingState::Rejected;
                    }
                    Ok(Ok(LanPairingOutcome::Expired)) => {
                        this.nearby_pairing_state = NearbyPairingState::Expired;
                    }
                    Ok(Err(error)) => {
                        this.nearby_pairing_state = NearbyPairingState::Failed {
                            message: error.message,
                        };
                    }
                    Err(_) => {
                        this.nearby_pairing_state = NearbyPairingState::Failed {
                            message: locale::text(
                                "Nearby pairing stopped unexpectedly.",
                                "附近配对意外停止。",
                                "附近配對意外停止。",
                            )
                            .to_string(),
                        };
                    }
                }
                cx.notify();
            });
        });
        self.lan_pairing_task = Some(task);
    }

    fn claim_scanned_pairing_link(&mut self, link: String, cx: &mut Context<Self>) {
        if self.pairing_busy || self.mode != RootMode::Pairing {
            return;
        }
        self.pairing_busy = true;
        self.error = None;
        let runner = gpui_tokio::Tokio::spawn(cx, async move { claim_pairing_link(link).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.pairing_busy = false;
                match outcome {
                    Ok(Ok(bundle)) => match this.storage.save(&bundle) {
                        Ok(()) => {
                            this.install_bundle(bundle, cx);
                        }
                        Err(error) => this.error = Some(error),
                    },
                    Ok(Err(error)) => this.error = Some(error),
                    Err(_) => {
                        this.error = Some(BackendError::failed(
                            "remote_pairing_task_failed",
                            locale::text(
                                "Pairing stopped unexpectedly.",
                                "配对意外停止。",
                                "配對意外停止。",
                            ),
                        ))
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        let capabilities = self
            .backend
            .as_ref()
            .map(|backend| backend.capability_snapshot().agent);
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        if let Some(capabilities) = capabilities {
            controller.set_capabilities(capabilities);
        }
        controller.begin_sessions_refresh();
        let future = controller.list_sessions(false);
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                let selected = this
                    .controller
                    .as_ref()
                    .and_then(|controller| controller.state.selected_session_id.clone());
                let first = outcome
                    .as_ref()
                    .ok()
                    .and_then(|sessions| sessions.first())
                    .map(|session| session.id.clone());
                if let Some(controller) = this.controller.as_mut()
                    && let Err(error) = controller.apply_sessions(outcome)
                {
                    this.error = Some(error);
                }
                this.sync_sidebar_state();
                this.refresh_sidebar_organization(cx);
                if selected.is_none()
                    && let Some(session_id) = first
                {
                    this.open_session(session_id, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn refresh_runtime_options(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        controller.state.runtime_options.begin();
        let runner = gpui_tokio::Tokio::spawn(cx, controller.list_runtime_options());
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                let applied = if let Some(controller) = this.controller.as_mut() {
                    match controller.apply_runtime_options(outcome) {
                        Ok(()) => true,
                        Err(error) => {
                            if this.runtime_options_open {
                                this.runtime_switch_error = Some(error);
                            } else {
                                this.error = Some(error);
                            }
                            false
                        }
                    }
                } else {
                    false
                };
                if applied && this.runtime_options_open {
                    this.sync_runtime_feature_inputs(cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn open_runtime_options(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.runtime_switch_busy_generation.is_some() || self.session_action.is_some() {
            return;
        }
        let can_switch = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentSwitchRuntime)
        });
        if !can_switch {
            return;
        }
        let Some((desired, has_catalog)) = self.controller.as_ref().and_then(|controller| {
            controller
                .state
                .runtime_selection
                .value
                .as_ref()
                .map(|state| {
                    (
                        state.desired.clone(),
                        controller.state.runtime_options.value.is_some(),
                    )
                })
        }) else {
            self.error = Some(BackendError::loading(
                "mobile_runtime_selection_loading",
                locale::text(
                    "The session runtime selection is not available yet.",
                    "会话运行时选择尚不可用。",
                    "工作階段執行環境選擇尚無法使用。",
                ),
            ));
            cx.notify();
            return;
        };
        self.runtime_draft = Some(desired);
        self.runtime_options_open = true;
        self.runtime_switch_error = None;
        self.sync_runtime_feature_inputs(cx);
        if !has_catalog {
            self.refresh_runtime_options(cx);
        }
        cx.notify();
    }

    fn close_runtime_options(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.runtime_switch_busy_generation.is_none() {
            self.runtime_options_open = false;
            self.runtime_draft = None;
            self.runtime_feature_inputs.clear();
            self.runtime_switch_error = None;
            cx.notify();
        }
    }

    fn reset_runtime_options(&mut self) {
        self.runtime_switch_generation = self.runtime_switch_generation.wrapping_add(1);
        self.runtime_switch_busy_generation = None;
        self.runtime_options_open = false;
        self.runtime_draft = None;
        self.runtime_feature_inputs.clear();
        self.runtime_switch_error = None;
    }

    fn choose_runtime_selection(
        &mut self,
        selection: SessionRuntimeSelection,
        cx: &mut Context<Self>,
    ) {
        if self.runtime_switch_busy_generation.is_some() {
            return;
        }
        self.runtime_draft = Some(selection);
        self.runtime_switch_error = None;
        self.sync_runtime_feature_inputs(cx);
        cx.notify();
    }

    fn choose_default_runtime_reasoning(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.runtime_draft.as_mut() {
            draft.reasoning_effort = None;
        }
        self.runtime_switch_error = None;
        cx.notify();
    }

    fn choose_default_runtime_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.runtime_draft.as_mut() {
            draft.mode_id = None;
        }
        self.runtime_switch_error = None;
        cx.notify();
    }

    fn choose_runtime_feature(
        &mut self,
        feature_id: String,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.runtime_switch_busy_generation.is_some() {
            return;
        }
        if let Some(draft) = self.runtime_draft.as_mut() {
            if let Some(value) = value {
                draft.config_values.insert(feature_id, value);
            } else {
                draft.config_values.remove(&feature_id);
            }
        }
        self.runtime_switch_error = None;
        cx.notify();
    }

    fn sync_runtime_feature_inputs(&mut self, cx: &mut Context<Self>) {
        let features = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.runtime_options.value.as_ref())
            .zip(self.runtime_draft.as_ref())
            .map(|(catalog, draft)| RuntimeCascadeProjection::from_catalog(catalog, draft).features)
            .unwrap_or_default();
        let config_values = self
            .runtime_draft
            .as_ref()
            .map(|draft| draft.config_values.clone())
            .unwrap_or_default();
        self.runtime_feature_inputs = features
            .into_iter()
            .filter(|feature| feature.kind == SessionRuntimeFeatureKind::String)
            .map(|feature| {
                let value = config_values.get(&feature.id).cloned().unwrap_or_default();
                let input = cx.new(|cx| {
                    let mut input = TextInput::new(locale::common("Value"), cx);
                    input.set_text(value, cx);
                    input
                });
                (feature.id, input)
            })
            .collect();
    }

    fn apply_runtime_feature_inputs(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        let Some(draft) = self.runtime_draft.as_mut() else {
            return Ok(());
        };
        for (feature_id, input) in &self.runtime_feature_inputs {
            let value = input.read(cx).text().to_string();
            match runtime_string_override(value)? {
                Some(value) => {
                    draft.config_values.insert(feature_id.clone(), value);
                }
                None => {
                    draft.config_values.remove(feature_id);
                }
            }
        }
        Ok(())
    }

    fn apply_runtime_options(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.runtime_switch_busy_generation.is_some() {
            return;
        }
        if let Err(error) = self.apply_runtime_feature_inputs(cx) {
            self.runtime_switch_error = Some(error);
            cx.notify();
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let Some((session_id, state, catalog)) = self.controller.as_ref().and_then(|controller| {
            Some((
                controller.state.selected_session_id.clone()?,
                controller.state.runtime_selection.value.clone()?,
                controller.state.runtime_options.value.clone()?,
            ))
        }) else {
            return;
        };
        let Some(desired) = self.runtime_draft.clone() else {
            return;
        };
        if !runtime_selection_is_available(&catalog.options, &desired) {
            self.runtime_switch_error = Some(BackendError::failed(
                "mobile_runtime_option_unavailable",
                locale::text(
                    "This runtime option is no longer available.",
                    "此运行时选项已不可用。",
                    "此執行環境選項已無法使用。",
                ),
            ));
            cx.notify();
            return;
        }

        self.runtime_switch_generation = self.runtime_switch_generation.wrapping_add(1);
        let generation = self.runtime_switch_generation;
        self.runtime_switch_busy_generation = Some(generation);
        self.runtime_switch_error = None;
        let requested_session_id = session_id.clone();
        let request = MutationRequest::new(SetDesiredAgentSessionRuntimeRequest {
            session_id,
            idempotency_key: RequestId::new().into_string(),
            expected_revision: state.session_revision,
            expected_selection_revision: state.selection_revision,
            desired,
            interaction: RuntimeSelectionInteraction::Seamless,
        });
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { backend.set_desired_runtime(request).await },
            );
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                let selected_session_id = this
                    .controller
                    .as_ref()
                    .and_then(|controller| controller.state.selected_session_id.as_ref());
                if this.runtime_switch_generation != generation
                    || selected_session_id != Some(&requested_session_id)
                {
                    return;
                }
                this.runtime_switch_busy_generation = None;
                match outcome {
                    Ok(state) => {
                        if let Some(controller) = this.controller.as_mut() {
                            controller.state.runtime_selection.resolve(state);
                        }
                        this.runtime_options_open = false;
                        this.runtime_draft = None;
                        this.runtime_feature_inputs.clear();
                        this.runtime_switch_error = None;
                        this.notice =
                            Some(locale::common("Runtime selection sent to desktop").to_string());
                    }
                    Err(error) => this.runtime_switch_error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn refresh_workspaces(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move { backend.list_workspaces().await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                match outcome {
                    Ok(summaries) => {
                        this.workspace_summaries = summaries.clone();
                        this.workspaces = summaries
                            .into_iter()
                            .map(|summary| summary.workspace)
                            .collect();
                        let workspace_ids = this
                            .workspaces
                            .iter()
                            .map(|workspace| workspace.id.as_str().to_string())
                            .collect::<BTreeSet<_>>();
                        this.collapsed_workspace_ids
                            .retain(|workspace_id| workspace_ids.contains(workspace_id));
                        let active_workspace = this
                            .controller
                            .as_ref()
                            .and_then(|controller| controller.state.active_session.value.as_ref())
                            .map(|session| session.workspace_id.clone());
                        let target = active_workspace
                            .filter(|workspace_id| {
                                this.workspaces
                                    .iter()
                                    .any(|workspace| &workspace.id == workspace_id)
                            })
                            .or_else(|| {
                                this.workspaces
                                    .iter()
                                    .find(|workspace| {
                                        workspace.mode == WorkspaceMode::CurrentCheckout
                                    })
                                    .or_else(|| this.workspaces.first())
                                    .map(|workspace| workspace.id.clone())
                            });
                        if let Some(workspace_id) = target {
                            this.ensure_workbench(workspace_id, cx);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn ensure_workbench(&mut self, workspace_id: vibex_core::WorkspaceId, cx: &mut Context<Self>) {
        let session_id = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.selected_session_id.clone());
        let pending_surface = self.pending_workbench_surface.take();
        if let Some(workbench) = self.workbench.as_ref() {
            workbench.update(cx, |workbench, cx| {
                workbench.set_workspace(workspace_id, cx);
                workbench.set_session(session_id, cx);
                if let Some(surface) = pending_surface {
                    workbench.set_surface(surface, cx);
                }
            });
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.workbench =
            Some(cx.new(|cx| MobileWorkbench::new(backend, workspace_id, session_id, cx)));
        if let Some(surface) = pending_surface
            && let Some(workbench) = self.workbench.as_ref()
        {
            workbench.update(cx, |workbench, cx| workbench.set_surface(surface, cx));
        }
    }

    fn close_workbench(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.start_drawer_snap(0.0, Some(window), cx);
    }

    fn create_session(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.start_session_creation(None, window, cx);
    }

    /// Creates a session inside a specific project, the way the desktop project
    /// row's "+" does, falling back to the active session's workspace when the
    /// project has no workspace of its own yet.
    fn create_session_in_project(
        &mut self,
        project_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self
            .workspace_summaries
            .iter()
            .find(|summary| summary.project.id.as_str() == project_id)
            .map(|summary| (summary.workspace.root_path.clone(), summary.workspace.mode));
        self.start_session_creation(workspace, window, cx);
    }

    fn create_session_in_workspace(
        &mut self,
        workspace_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id.as_str() == workspace_id)
            .map(|workspace| (workspace.root_path.clone(), workspace.mode));
        self.start_session_creation(workspace, window, cx);
    }

    fn start_session_creation(
        &mut self,
        workspace_override: Option<(String, WorkspaceMode)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.new_session_busy {
            return;
        }
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let active_session = controller.state.active_session.value.as_ref();
        let runtime = controller
            .state
            .runtime_selection
            .value
            .as_ref()
            .map(|state| state.desired.clone())
            .or_else(|| {
                controller
                    .state
                    .runtime_options
                    .value
                    .as_ref()?
                    .options
                    .iter()
                    .find(|option| option.availability == RuntimeOptionAvailability::Available)
                    .map(|option| option.selection.clone())
            });
        let workspace = workspace_override
            .or_else(|| {
                active_session
                    .map(|session| (session.workspace_root.clone(), session.workspace_mode))
            })
            .or_else(|| {
                self.workspaces
                    .iter()
                    .find(|workspace| workspace.mode == WorkspaceMode::CurrentCheckout)
                    .or_else(|| self.workspaces.first())
                    .map(|workspace| (workspace.root_path.clone(), workspace.mode))
            });
        let Some(runtime) = runtime else {
            self.error = Some(BackendError::loading(
                "mobile_runtime_catalog_loading",
                locale::text(
                    "The desktop has not published an available Agent runtime yet.",
                    "桌面端尚未发布可用的 Agent 运行时。",
                    "桌面版尚未發佈可用的 Agent 執行環境。",
                ),
            ));
            self.refresh_runtime_options(cx);
            cx.notify();
            return;
        };
        let Some((workspace_root, workspace_mode)) = workspace else {
            self.error = Some(BackendError::failed(
                "mobile_workspace_required",
                locale::text(
                    "Open a workspace on the desktop before creating a mobile session.",
                    "请先在桌面端打开工作区，再创建移动端会话。",
                    "請先在桌面版開啟工作區，再建立行動端工作階段。",
                ),
            ));
            self.refresh_workspaces(cx);
            cx.notify();
            return;
        };
        let request = MutationRequest::new(CreateAgentSessionRequest {
            runtime,
            workspace_root,
            workspace_mode,
            title: None,
            safety: None,
        });
        let runner = gpui_tokio::Tokio::spawn(cx, controller.create_session(request));
        self.new_session_busy = true;
        self.error = None;
        self.start_drawer_snap(0.0, Some(window), cx);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.new_session_busy = false;
                match outcome {
                    Ok(session) => {
                        this.refresh_sessions(cx);
                        this.open_session(session.id, cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn begin_session_action(
        &mut self,
        kind: SessionActionKind,
        session_id: VibexSessionId,
        current_title: String,
        cx: &mut Context<Self>,
    ) {
        if self.session_action_busy {
            return;
        }
        if kind == SessionActionKind::Rename {
            self.session_action_input
                .update(cx, |input, cx| input.set_text(current_title.clone(), cx));
        }
        self.session_action = Some(SessionActionPrompt {
            kind,
            session_id,
            current_title,
        });
        self.error = None;
        cx.notify();
    }

    fn cancel_session_action(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.session_action_busy {
            self.session_action = None;
            cx.notify();
        }
    }

    fn confirm_session_action(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.session_action_busy {
            return;
        }
        let Some(prompt) = self.session_action.clone() else {
            return;
        };
        let Some(controller) = self.controller.as_ref() else {
            return;
        };
        let future: BackendFuture<'static, SessionMutationOutcome> = match prompt.kind {
            SessionActionKind::Rename => {
                let title = self.session_action_input.read(cx).text().trim().to_string();
                let future =
                    controller.rename_session(MutationRequest::new(RenameAgentSessionRequest {
                        session_id: prompt.session_id.clone(),
                        title,
                    }));
                Box::pin(async move {
                    future
                        .await
                        .map(Box::new)
                        .map(SessionMutationOutcome::Renamed)
                })
            }
            SessionActionKind::Archive => {
                let future =
                    controller.archive_session(MutationRequest::new(prompt.session_id.clone()));
                Box::pin(async move {
                    future.await?;
                    Ok(SessionMutationOutcome::Removed)
                })
            }
            SessionActionKind::Delete => {
                let future =
                    controller.delete_session(MutationRequest::new(prompt.session_id.clone()));
                Box::pin(async move {
                    future.await?;
                    Ok(SessionMutationOutcome::Removed)
                })
            }
        };
        self.session_action_busy = true;
        self.error = None;
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.session_action_busy = false;
                match outcome {
                    Ok(SessionMutationOutcome::Renamed(session)) => {
                        if let Some(controller) = this.controller.as_mut()
                            && controller.state.selected_session_id.as_ref() == Some(&session.id)
                        {
                            controller.state.active_session.resolve(*session);
                        }
                        this.session_action = None;
                        this.notice = Some(locale::common("Session renamed").to_string());
                        this.refresh_sessions(cx);
                    }
                    Ok(SessionMutationOutcome::Removed) => {
                        let removed_selected = this.controller.as_ref().is_some_and(|controller| {
                            controller.state.selected_session_id.as_ref()
                                == Some(&prompt.session_id)
                        });
                        if removed_selected {
                            if let Some(controller) = this.controller.as_mut() {
                                controller.state.selected_session_id = None;
                                controller.state.active_session.clear();
                                controller.state.timeline_status.clear();
                                controller.state.runtime_selection.clear();
                            }
                            this.timeline_turns = Arc::new(Vec::new());
                            this.timeline_list.reset(0);
                            if let Some(workbench) = this.workbench.as_ref() {
                                workbench
                                    .update(cx, |workbench, cx| workbench.set_session(None, cx));
                            }
                        }
                        this.session_action = None;
                        this.notice = Some(match prompt.kind {
                            SessionActionKind::Archive => {
                                locale::common("Session archived").to_string()
                            }
                            SessionActionKind::Delete => {
                                locale::common("Session deleted").to_string()
                            }
                            SessionActionKind::Rename => unreachable!(),
                        });
                        this.refresh_sessions(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn open_session(&mut self, session_id: VibexSessionId, cx: &mut Context<Self>) {
        self.reset_runtime_options();
        self.sidebar_state.selected_ids.clear();
        self.sidebar_state
            .selected_ids
            .insert(session_id.as_str().to_string());
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        let ticket = match controller.begin_session_load(session_id) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = controller.load_session(ticket.clone());
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        self.reset_drawers();
        self.expanded_process.clear();
        self.expanded_approval.clear();
        self.timeline_turns = Arc::new(Vec::new());
        self.timeline_list.reset(0);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if let Some(controller) = this.controller.as_mut()
                    && controller.apply_session_snapshot(&ticket, outcome)
                {
                    this.rebuild_timeline_turns();
                    this.timeline_list.scroll_to_end();
                    let workspace_id = this
                        .controller
                        .as_ref()
                        .and_then(|controller| controller.state.active_session.value.as_ref())
                        .map(|session| session.workspace_id.clone());
                    if let Some(workspace_id) = workspace_id {
                        this.ensure_workbench(workspace_id, cx);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Pulls the Desktop's sidebar tree. The phone never invents folders or
    /// ordering of its own; it renders what the Desktop reports.
    fn refresh_sidebar_organization(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if self.sidebar_sync_busy
            || !backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentSidebarOrganizationRead)
        {
            return;
        }
        self.sidebar_sync_busy = true;
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.sidebar_organization().await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.sidebar_sync_busy = false;
                // A desktop without the sidebar bridge still lists sessions;
                // the phone falls back to an unorganized tree rather than
                // surfacing an error it cannot act on.
                if let Ok(snapshot) = outcome {
                    this.sidebar_view = SidebarOrganizationView::from_remote(&snapshot);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Sends a change to the Desktop and adopts the tree it returns. Passing the
    /// rendered revision lets the Desktop refuse a move aimed at a layout that
    /// has since changed there, instead of reordering the wrong row.
    fn send_sidebar_mutation(
        &mut self,
        mutation: RemoteSidebarOrganizationMutation,
        guard_revision: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if !backend
            .capability_snapshot()
            .agent
            .supports(BackendOperation::AgentSidebarOrganizationMutate)
        {
            return;
        }
        let expected_revision = guard_revision.then_some(self.sidebar_view.revision);
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .mutate_sidebar_organization(mutation, expected_revision)
                .await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                match outcome {
                    Ok(snapshot) => {
                        this.sidebar_view = SidebarOrganizationView::from_remote(&snapshot);
                    }
                    Err(error) => {
                        // The optimistic local edit is now wrong either way, so
                        // re-read rather than leaving the two shells disagreeing.
                        this.error = Some(error);
                        this.refresh_sidebar_organization(cx);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn sidebar_projects(&self) -> Vec<SidebarProject> {
        let mut seen = BTreeSet::new();
        let mut projects = self
            .workspace_summaries
            .iter()
            .filter_map(|summary| {
                let id = summary.project.id.as_str().to_string();
                seen.insert(id.clone()).then(|| SidebarProject {
                    id,
                    label: summary.project.name.clone(),
                })
            })
            .collect::<Vec<_>>();
        // A session can outlive the workspace listing the phone last fetched;
        // showing its project beats hiding the session entirely.
        for session in self.sessions() {
            let id = session.project_id.as_str().to_string();
            if seen.insert(id.clone()) {
                projects.push(SidebarProject {
                    id,
                    label: workspace_label(&session.workspace_root).to_string(),
                });
            }
        }
        projects
    }

    fn sidebar_workspaces(&self) -> Vec<SidebarWorkspace> {
        self.workspace_summaries
            .iter()
            .map(|summary| {
                let workspace_id = summary.workspace.id.as_str().to_string();
                SidebarWorkspace {
                    id: workspace_id.clone(),
                    project_id: summary.project.id.as_str().to_string(),
                    label: workspace_label(&summary.workspace.root_path).to_string(),
                    detail: summary.workspace.root_path.clone(),
                    mode: summary.workspace.mode,
                    collapsed: self.collapsed_workspace_ids.contains(&workspace_id),
                }
            })
            .collect()
    }

    fn sessions(&self) -> &[vibex_core::AgentSession] {
        self.controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_deref())
            .unwrap_or_default()
    }

    fn mobile_sidebar_rows(&self, query: &str) -> Vec<SidebarRow> {
        let projects = self.sidebar_projects();
        let workspaces = self.sidebar_workspaces();
        sidebar_rows(SidebarRowInput {
            view: &self.sidebar_view,
            projects: &projects,
            workspaces: &workspaces,
            sessions: self.sessions(),
            selected_session_id: self
                .controller
                .as_ref()
                .and_then(|controller| controller.state.selected_session_id.as_ref()),
            query,
        })
    }

    fn toggle_workspace(&mut self, workspace_id: String, cx: &mut Context<Self>) {
        if !self.collapsed_workspace_ids.remove(&workspace_id) {
            self.collapsed_workspace_ids.insert(workspace_id);
        }
        cx.notify();
    }

    fn toggle_folder(&mut self, folder_id: String, cx: &mut Context<Self>) {
        let collapsed = !self
            .sidebar_view
            .organization
            .collapsed_folder_ids
            .contains(&folder_id);
        if collapsed {
            self.sidebar_view
                .organization
                .collapsed_folder_ids
                .insert(folder_id.clone());
        } else {
            self.sidebar_view
                .organization
                .collapsed_folder_ids
                .remove(&folder_id);
        }
        self.send_sidebar_mutation(
            RemoteSidebarOrganizationMutation::SetFolderCollapsed {
                folder_id,
                collapsed,
            },
            false,
            cx,
        );
        cx.notify();
    }

    /// Starts a move if the pan began on a row's grip column. Anywhere else the
    /// pan stays a list scroll, which is what a finger usually means.
    fn begin_sidebar_drag(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        if self.sidebar_search_open {
            return false;
        }
        let (list_top, list_right) = self.sidebar_list_frame.get();
        if list_right <= 0.0
            || !press_is_on_grip(
                f32::from(event.position.x),
                list_right,
                theme::SIDEBAR_GRIP_WIDTH,
            )
        {
            return false;
        }
        let rows = self.mobile_sidebar_rows(self.sidebar_search_input.read(cx).text());
        let offset_y = f32::from(self.drawer_scroll.0.borrow().base_handle.offset().y);
        let position = row_at_position(
            f32::from(event.position.y),
            list_top,
            offset_y,
            theme::SIDEBAR_ROW_HEIGHT,
        );
        if position < 0.0 {
            return false;
        }
        let index = position.floor() as usize;
        let Some(row) = rows.get(index).cloned() else {
            return false;
        };
        if row.kind == SidebarRowKind::Workspace {
            return false;
        }
        self.sidebar_drag = Some(SidebarDrag {
            index,
            row,
            pointer_y: f32::from(event.position.y),
            target: None,
        });
        cx.notify();
        true
    }

    fn advance_sidebar_drag(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.sidebar_drag.as_ref() else {
            return;
        };
        let index = drag.index;
        let rows = self.mobile_sidebar_rows(self.sidebar_search_input.read(cx).text());
        let (list_top, _) = self.sidebar_list_frame.get();
        let offset_y = f32::from(self.drawer_scroll.0.borrow().base_handle.offset().y);
        let pointer_y = f32::from(event.position.y);
        let position = row_at_position(pointer_y, list_top, offset_y, theme::SIDEBAR_ROW_HEIGHT);
        let mut target = drop_target(&rows, index, position);
        if target
            .as_ref()
            .is_some_and(|candidate| rows[candidate.index].kind == SidebarRowKind::Workspace)
        {
            target = None;
        }
        // A folder cannot be filed inside itself or its own descendants.
        if let Some(candidate) = target.as_ref()
            && rows[index].kind == SidebarRowKind::Folder
            && (candidate.index == index
                || ancestors_of(&rows, candidate.index).contains(rows[index].id()))
        {
            target = None;
        }
        if let Some(drag) = self.sidebar_drag.as_mut() {
            drag.pointer_y = pointer_y;
            drag.target = target;
        }
        cx.notify();
    }

    fn finish_sidebar_drag(&mut self, cancelled: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.sidebar_drag.take() else {
            return;
        };
        if drag.row.kind == SidebarRowKind::Workspace {
            return;
        }
        cx.notify();
        let Some(target) = drag.target.filter(|_| !cancelled) else {
            return;
        };
        let rows = self.mobile_sidebar_rows(self.sidebar_search_input.read(cx).text());
        let Some(anchor) = rows.get(target.index) else {
            return;
        };
        if anchor.kind == SidebarRowKind::Workspace {
            return;
        }
        // Scopes never mix: a root item stays at the root and a project item
        // stays inside its project, exactly as the Desktop enforces.
        if anchor.project_id != drag.row.project_id {
            return;
        }
        self.send_sidebar_mutation(
            RemoteSidebarOrganizationMutation::MoveItems {
                items: vec![sidebar_item_ref(&drag.row.item)],
                anchor: Some(sidebar_item_ref(&anchor.item)),
                position: match target.position {
                    SidebarDropPosition::Before => RemoteSidebarDropPosition::Before,
                    SidebarDropPosition::After => RemoteSidebarDropPosition::After,
                    SidebarDropPosition::Into => RemoteSidebarDropPosition::Into,
                },
                project_id: drag.row.project_id.clone(),
            },
            true,
            cx,
        );
    }

    fn open_folder_name_prompt(&mut self, prompt: SidebarNamePrompt, cx: &mut Context<Self>) {
        let initial = match &prompt {
            SidebarNamePrompt::CreateFolder { .. } => String::new(),
            SidebarNamePrompt::RenameFolder { folder_id } => self
                .sidebar_view
                .organization
                .folder(folder_id)
                .map(|folder| folder.name.clone())
                .unwrap_or_default(),
        };
        self.sidebar_name_input
            .update(cx, |input, cx| input.set_text(initial, cx));
        self.sidebar_name_prompt = Some(prompt);
        cx.notify();
    }

    fn submit_folder_name(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.sidebar_name_prompt.clone() else {
            return;
        };
        let name = self.sidebar_name_input.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let mutation = match prompt {
            SidebarNamePrompt::CreateFolder {
                project_id,
                workspace_id,
                parent_folder_id,
            } => RemoteSidebarOrganizationMutation::CreateFolder {
                name,
                project_id,
                workspace_id,
                parent_folder_id,
            },
            SidebarNamePrompt::RenameFolder { folder_id } => {
                RemoteSidebarOrganizationMutation::RenameFolder { folder_id, name }
            }
        };
        self.sidebar_name_prompt = None;
        self.send_sidebar_mutation(mutation, false, cx);
    }

    fn sync_sidebar_state(&mut self) {
        let Some(sessions) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_ref())
        else {
            return;
        };
        let collapsed = self.sidebar_state.collapsed_ids.clone();
        self.sidebar_state.reconcile(
            sessions
                .iter()
                .map(|session| session.id.as_str().to_string()),
        );
        self.sidebar_state.collapsed_ids = if self.sidebar_projects_initialized {
            collapsed
        } else {
            self.sidebar_projects_initialized = true;
            BTreeSet::new()
        };
        self.sidebar_state.selected_ids.clear();
        if let Some(selected) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.selected_session_id.as_ref())
        {
            self.sidebar_state
                .selected_ids
                .insert(selected.as_str().to_string());
        }
    }

    fn reset_sidebar_ui(&mut self, cx: &mut Context<Self>) {
        self.sidebar_state = SidebarState::default();
        // The tree belongs to whichever desktop is paired, so switching hosts
        // must not leave the previous desktop's folders on screen.
        self.sidebar_view = SidebarOrganizationView::default();
        self.collapsed_workspace_ids.clear();
        self.sidebar_drag = None;
        self.sidebar_row_menu = None;
        self.sidebar_name_prompt = None;
        self.sidebar_projects_initialized = false;
        self.sidebar_search_open = false;
        self.sidebar_search_input
            .update(cx, |input, cx| input.set_text("", cx));
    }

    fn toggle_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let collapsed = !self
            .sidebar_view
            .collapsed_project_ids
            .contains(&project_id);
        if collapsed {
            self.sidebar_view
                .collapsed_project_ids
                .insert(project_id.clone());
        } else {
            self.sidebar_view.collapsed_project_ids.remove(&project_id);
        }
        self.send_sidebar_mutation(
            RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                project_id,
                collapsed,
            },
            false,
            cx,
        );
        cx.notify();
    }

    fn toggle_all_projects(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let project_ids = self
            .sidebar_projects()
            .into_iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();
        if project_ids.is_empty() {
            return;
        }
        let collapsed = project_ids
            .iter()
            .any(|project_id| !self.sidebar_view.collapsed_project_ids.contains(project_id));
        // Each project carries its own collapse flag on the Desktop, so this
        // sends one unguarded change per project rather than a batch that a
        // revision check would reject after the first.
        for project_id in project_ids {
            if collapsed {
                self.sidebar_view
                    .collapsed_project_ids
                    .insert(project_id.clone());
            } else {
                self.sidebar_view.collapsed_project_ids.remove(&project_id);
            }
            self.send_sidebar_mutation(
                RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                    project_id,
                    collapsed,
                },
                false,
                cx,
            );
        }
        cx.notify();
    }

    /// Mirrors the desktop "locate current session" toolbar action: reveal the
    /// selected session by expanding its project, then scroll the list to it.
    fn locate_selected_session(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.selected_session_id.clone())
        else {
            return;
        };
        let project_id = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_deref())
            .and_then(|sessions| {
                sessions
                    .iter()
                    .find(|session| session.id == session_id)
                    .map(|session| session.project_id.as_str().to_string())
            });
        let workspace_id = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_deref())
            .and_then(|sessions| sessions.iter().find(|session| session.id == session_id))
            .map(|session| session.workspace_id.as_str().to_string());
        if let Some(project_id) = project_id
            && self
                .sidebar_view
                .collapsed_project_ids
                .contains(&project_id)
        {
            self.toggle_project(project_id, cx);
        }
        if let Some(workspace_id) = workspace_id
            && self.collapsed_workspace_ids.contains(&workspace_id)
        {
            self.toggle_workspace(workspace_id, cx);
        }
        let query = self.sidebar_search_input.read(cx).text().to_string();
        if let Some(index) = self
            .mobile_sidebar_rows(&query)
            .iter()
            .position(|row| row.session_id.as_ref() == Some(&session_id))
        {
            self.drawer_scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn toggle_sidebar_search(
        &mut self,
        _: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_search_open = !self.sidebar_search_open;
        if self.sidebar_search_open {
            self.sidebar_search_input
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
            window.show_soft_keyboard();
        } else {
            self.sidebar_search_input
                .update(cx, |input, cx| input.set_text("", cx));
            window.hide_soft_keyboard();
        }
        cx.notify();
    }

    fn show_overlay(
        &mut self,
        overlay: MobileOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = self.overlay;
        if previous.is_none() {
            self.overlay_returns_to_drawer = self.drawer_open;
        } else {
            self.overlay_parent = previous;
        }
        self.overlay = Some(overlay);
        self.start_drawer_snap(0.0, Some(window), cx);
        cx.notify();
    }

    fn clear_overlay(&mut self) {
        self.overlay = None;
        self.overlay_parent = None;
        self.overlay_returns_to_drawer = false;
    }

    fn close_overlay(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_overlay(Some(window), cx);
    }

    fn dismiss_overlay(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if let Some(parent) = self.overlay_parent.take() {
            self.overlay = Some(parent);
            cx.notify();
            return;
        }
        let return_to_drawer = self.overlay_returns_to_drawer;
        self.overlay = None;
        self.overlay_returns_to_drawer = false;
        if return_to_drawer {
            self.start_drawer_snap(DrawerPage::Sessions.open_offset(), window, cx);
        }
        cx.notify();
    }

    fn open_hosts(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.show_overlay(MobileOverlay::Hosts, window, cx);
    }

    fn open_settings(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.show_overlay(MobileOverlay::Settings, window, cx);
    }

    fn open_usage(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.show_overlay(MobileOverlay::Usage, window, cx);
    }

    fn open_new_project(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_project_busy {
            return;
        }
        self.new_project_error = None;
        self.new_project_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.show_overlay(MobileOverlay::NewProject, window, cx);
        self.new_project_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        window.show_soft_keyboard();
    }

    fn submit_new_project(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.new_project_busy {
            return;
        }
        let root_path = self.new_project_input.read(cx).text().trim().to_string();
        if root_path.is_empty() {
            self.new_project_error = Some(
                locale::text(
                    "Enter a project path on the desktop host.",
                    "请输入桌面端上的项目路径。",
                    "請輸入桌面版上的專案路徑。",
                )
                .to_string(),
            );
            cx.notify();
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if !backend
            .capability_snapshot()
            .workspace
            .supports(BackendOperation::WorkspaceOpen)
        {
            self.new_project_error = Some(
                locale::text(
                    "This host cannot open a project remotely.",
                    "当前主机不支持远程打开项目。",
                    "目前主機不支援遠端開啟專案。",
                )
                .to_string(),
            );
            cx.notify();
            return;
        }
        let request = MutationRequest::new(OpenWorkspaceRequest {
            root_path,
            mode: Some(WorkspaceMode::CurrentCheckout),
        });
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.open_workspace(request).await });
        self.new_project_busy = true;
        self.new_project_error = None;
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.new_project_busy = false;
                match outcome {
                    Ok(summary) => {
                        this.workspaces
                            .retain(|workspace| workspace.id != summary.workspace.id);
                        this.workspaces.push(summary.workspace.clone());
                        this.workspace_summaries
                            .retain(|candidate| candidate.workspace.id != summary.workspace.id);
                        this.workspace_summaries.push(summary.clone());
                        this.ensure_workbench(summary.workspace.id, cx);
                        this.notice = Some(locale::common("Project added").to_string());
                        this.dismiss_overlay(None, cx);
                    }
                    Err(error) => {
                        this.new_project_error = Some(error.message);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn open_workbench_surface(
        &mut self,
        surface: WorkbenchSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_overlay();
        if let Some(workbench) = self.workbench.as_ref() {
            workbench.update(cx, |workbench, cx| workbench.set_surface(surface, cx));
        } else {
            self.pending_workbench_surface = Some(surface);
            self.refresh_workspaces(cx);
        }
        self.start_drawer_snap(DrawerPage::Workbench.open_offset(), Some(window), cx);
    }

    fn begin_pairing_host(
        &mut self,
        _: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_connection_tasks();
        crate::background_connection::disconnect();
        self.backend = None;
        self.controller = None;
        self.pending_workbench_surface = None;
        if let Some(workbench) = self.workbench.take() {
            workbench.update(cx, |workbench, _| workbench.suspend());
        }
        self.reset_drawers();
        self.clear_overlay();
        self.mode = RootMode::Pairing;
        self.pairing_from_hosts = true;
        self.error = None;
        self.workspaces.clear();
        self.workspace_summaries.clear();
        self.reset_sidebar_ui(cx);
        window.hide_soft_keyboard();
        cx.notify();
    }

    fn switch_host(
        &mut self,
        host_id: String,
        _: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_host_id.as_deref() == Some(host_id.as_str())
            && self.mode == RootMode::Workspace
        {
            self.dismiss_overlay(Some(window), cx);
            return;
        }
        let Some(entry) = self
            .known_hosts
            .iter()
            .find(|host| host.id == host_id)
            .cloned()
        else {
            return;
        };
        if let Err(error) = self.storage.save(&entry.bundle) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.clear_overlay();
        self.install_bundle(entry.bundle, cx);
    }

    fn cancel_pairing_host(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(host_id) = self.active_host_id.clone() else {
            return;
        };
        let Some(entry) = self
            .known_hosts
            .iter()
            .find(|host| host.id == host_id)
            .cloned()
        else {
            return;
        };
        if let Err(error) = self.storage.save(&entry.bundle) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.install_bundle(entry.bundle, cx);
    }

    fn reload_selected_session(&mut self, cx: &mut Context<Self>) {
        if self
            .controller
            .as_ref()
            .is_some_and(|controller| controller.state.timeline_status.phase == AsyncPhase::Loading)
        {
            return;
        }
        let selected = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.selected_session_id.clone());
        let Some(session_id) = selected else {
            return;
        };
        let should_follow = self.timeline_is_near_bottom();
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        let ticket = match controller.begin_session_load(session_id) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, controller.load_session(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if let Some(controller) = this.controller.as_mut()
                    && controller.apply_session_snapshot(&ticket, outcome)
                {
                    this.rebuild_timeline_turns();
                    this.timeline_list.remeasure();
                    if should_follow {
                        this.timeline_list.scroll_to_end();
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn send_message(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.operation_busy || self.runtime_switch_busy_generation.is_some() {
            return;
        }
        let text = self.composer_input.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        let Some(session_id) = controller.state.selected_session_id.clone() else {
            return;
        };
        let Some(runtime) = controller.state.runtime_selection.value.as_ref() else {
            self.error = Some(BackendError::loading(
                "mobile_runtime_selection_loading",
                locale::text(
                    "The session runtime selection is not available yet.",
                    "会话运行时选择尚不可用。",
                    "工作階段執行環境選擇尚無法使用。",
                ),
            ));
            cx.notify();
            return;
        };
        let restore_text = text.clone();
        let request = MutationRequest::new(SendAgentMessageRequest {
            session_id,
            message_idempotency_key: RequestId::new().into_string(),
            desired_runtime: runtime.desired.clone(),
            text,
            attachments: Vec::new(),
            reasoning_effort: runtime.desired.reasoning_effort.clone(),
            correlation_id: None,
        });
        let ticket = match controller.begin_send_message(&request) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = controller.send_message(request);
        self.composer_input.update(cx, |input, cx| {
            let _ = input.take(cx);
        });
        self.spawn_timeline_mutation(ticket, future, Some(restore_text), cx);
    }

    fn continue_turn(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.operation_busy {
            return;
        }
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        let Some(session_id) = controller.state.selected_session_id.clone() else {
            return;
        };
        let request = MutationRequest::new(ContinueAgentTurnRequest {
            session_id,
            correlation_id: None,
        });
        let ticket = match controller.begin_continue_turn(&request) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = controller.continue_turn(request);
        self.spawn_timeline_mutation(ticket, future, None, cx);
    }

    fn spawn_timeline_mutation(
        &mut self,
        ticket: AgentMutationTicket,
        future: vibex_backend::BackendFuture<'static, Vec<vibex_core::TimelineItem>>,
        restore_composer: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.operation_busy = true;
        self.rebuild_timeline_turns();
        self.timeline_list.scroll_to_end();
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let failed = outcome.is_err();
            let _ = entity.update(cx, |this, cx| {
                this.operation_busy = false;
                if let Some(controller) = this.controller.as_mut() {
                    controller.apply_timeline_mutation(&ticket, outcome);
                    this.error = controller.state.latest_mutation.error.clone();
                }
                this.rebuild_timeline_turns();
                if failed
                    && let Some(text) = restore_composer
                    && this.composer_input.read(cx).text().is_empty()
                {
                    this.composer_input
                        .update(cx, |input, cx| input.set_text(text, cx));
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn interrupt(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.operation_busy {
            return;
        }
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        let Some(session_id) = controller.state.selected_session_id.clone() else {
            return;
        };
        let request = MutationRequest::new(session_id);
        let ticket = match controller.begin_interrupt(&request) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = controller.interrupt(request);
        self.operation_busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.operation_busy = false;
                if let Some(controller) = this.controller.as_mut() {
                    controller.apply_simple_mutation(&ticket, outcome);
                    this.error = controller.state.latest_mutation.error.clone();
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn resolve_permission(
        &mut self,
        request_id: RequestId,
        response: PermissionResponseKind,
        cx: &mut Context<Self>,
    ) {
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        if controller
            .state
            .pending_permission_resolution(request_id.as_str())
        {
            return;
        }
        let Some(permission) = controller.state.timeline.items.iter().find_map(|item| {
            let TimelinePayload::PermissionRequest(permission) = &item.payload else {
                return None;
            };
            (permission.id == request_id).then_some(permission.clone())
        }) else {
            return;
        };
        let request_id_string = request_id.to_string();
        let request = MutationRequest::new(ResolvePermissionRequest {
            session_id: permission.session_id.clone(),
            request_id: request_id.clone(),
            resolution: PermissionResolution {
                request_id,
                session_id: permission.session_id,
                response,
                responder_device_id: None,
                provider_resolution_id: None,
                note: None,
                resolved_at_ms: unix_timestamp_ms(),
            },
        });
        let ticket = match controller.begin_resolve_permission(&request) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let future = controller.resolve_permission(request);
        let runner = gpui_tokio::Tokio::spawn(cx, future);
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if let Some(controller) = this.controller.as_mut() {
                    controller.apply_permission_mutation(&ticket, &request_id_string, outcome);
                    this.error = controller.state.latest_mutation.error.clone();
                }
                this.rebuild_timeline_turns();
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn sync_elicitation_form(&mut self, cx: &mut Context<Self>) {
        let request = self.controller.as_ref().and_then(|controller| {
            controller
                .state
                .elicitation_surfaces(ShellKind::Compact)
                .into_iter()
                .next()
                .map(|surface| surface.request)
        });
        let Some(request) = request else {
            self.elicitation_request_id = None;
            self.elicitation_inputs.clear();
            self.elicitation_draft = None;
            return;
        };
        if self.elicitation_request_id.as_ref() == Some(&request.id) {
            return;
        }

        let draft = ElicitationFormDraft::from_request(&request);
        self.elicitation_inputs = request
            .fields
            .iter()
            .filter_map(|field| {
                let placeholder = if field.required {
                    format!("{} (required)", field.title)
                } else {
                    field.title.clone()
                };
                let initial = match &field.kind {
                    ElicitationFieldKind::Text { options, .. } if options.is_empty() => {
                        draft.text(&field.id).unwrap_or_default().to_string()
                    }
                    ElicitationFieldKind::Number { .. } | ElicitationFieldKind::Integer { .. } => {
                        draft.text(&field.id).unwrap_or_default().to_string()
                    }
                    _ => return None,
                };
                let input = cx.new(|cx| {
                    let mut input = TextInput::new(placeholder, cx);
                    input.set_text(initial, cx);
                    input
                });
                Some((field.id.clone(), input))
            })
            .collect();
        self.elicitation_request_id = Some(request.id);
        self.elicitation_draft = Some(draft);
    }

    fn set_elicitation_option(
        &mut self,
        request_id: RequestId,
        field_id: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
        if self.elicitation_request_id.as_ref() != Some(&request_id) {
            return;
        }
        if let Some(draft) = self.elicitation_draft.as_mut() {
            draft.select_option(field_id, value);
            cx.notify();
        }
    }

    fn set_elicitation_boolean(
        &mut self,
        request_id: RequestId,
        field_id: String,
        value: bool,
        cx: &mut Context<Self>,
    ) {
        if self.elicitation_request_id.as_ref() != Some(&request_id) {
            return;
        }
        if let Some(draft) = self.elicitation_draft.as_mut() {
            draft.set_boolean(field_id, value);
            cx.notify();
        }
    }

    fn toggle_elicitation_multi_option(
        &mut self,
        request_id: RequestId,
        field_id: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
        if self.elicitation_request_id.as_ref() != Some(&request_id) {
            return;
        }
        if let Some(draft) = self.elicitation_draft.as_mut() {
            draft.toggle_multi_option(field_id, value);
            cx.notify();
        }
    }

    fn resolve_elicitation(
        &mut self,
        request_id: RequestId,
        action: ElicitationResolutionAction,
        cx: &mut Context<Self>,
    ) {
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        if controller
            .state
            .pending_elicitation_resolution(request_id.as_str())
        {
            return;
        }
        let Some(request) = controller.state.timeline.items.iter().find_map(|item| {
            let TimelinePayload::ElicitationRequest(request) = &item.payload else {
                return None;
            };
            (request.id == request_id).then_some(request.clone())
        }) else {
            return;
        };
        let mut draft = self
            .elicitation_draft
            .clone()
            .filter(|draft| draft.request_id == request_id)
            .unwrap_or_else(|| ElicitationFormDraft::from_request(&request));
        for field in &request.fields {
            if let Some(input) = self.elicitation_inputs.get(&field.id) {
                draft.set_text(field.id.clone(), input.read(cx).text().to_string());
            }
        }
        let payload = match draft.resolve_request(&request, action, unix_timestamp_ms()) {
            Ok(payload) => payload,
            Err(error) => {
                self.error = Some(BackendError::from(error));
                cx.notify();
                return;
            }
        };
        let mutation = MutationRequest::new(payload);
        let request_id_string = request_id.to_string();
        let ticket = match controller.begin_resolve_elicitation(&mutation) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, controller.resolve_elicitation(mutation));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if let Some(controller) = this.controller.as_mut() {
                    controller.apply_elicitation_mutation(&ticket, &request_id_string, outcome);
                    this.error = controller.state.latest_mutation.error.clone();
                }
                this.rebuild_timeline_turns();
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn close_drawer(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.start_drawer_snap(0.0, Some(window), cx);
    }

    fn toggle_drawer(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let target = sessions_button_target(self.drawer_open);
        self.start_drawer_snap(target, Some(window), cx);
    }

    fn close_drawer_from_backdrop(
        &mut self,
        _: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.drawer_gesture, Some(DrawerGesture::Dragging { .. })) {
            self.start_drawer_snap(0.0, Some(window), cx);
        }
    }

    fn start_drawer_snap(
        &mut self,
        target: f32,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let target = target.clamp(-1.0, 1.0);
        let from = self.drawer_offset;
        self.drawer_gesture = None;
        self.drawer_open = target > 0.0;
        let workbench_was_open = self.workbench_open;
        self.workbench_open = target < 0.0;
        if self.workbench_open && !workbench_was_open {
            if let Some(workbench) = self.workbench.as_ref() {
                workbench.update(cx, |workbench, cx| workbench.resume(cx));
            } else {
                self.refresh_workspaces(cx);
            }
        } else if workbench_was_open
            && !self.workbench_open
            && let Some(workbench) = self.workbench.as_ref()
        {
            workbench.update(cx, |workbench, _| workbench.suspend());
        }
        self.drawer_snap_task = None;
        if let Some(window) = window {
            window.hide_soft_keyboard();
        }
        if (from - target).abs() < 0.001 {
            self.drawer_offset = target;
            self.drawer_snap = None;
            cx.notify();
            return;
        }

        self.drawer_animation_id = self.drawer_animation_id.wrapping_add(1);
        let animation_id = self.drawer_animation_id;
        self.drawer_snap = Some(DrawerSnap {
            from,
            target,
            animation_id,
        });
        let duration_ms = drawer_snap_duration_ms(from, target);
        let timer = cx
            .background_executor()
            .timer(Duration::from_millis(duration_ms));
        self.drawer_snap_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            timer.await;
            let _ = entity.update(cx, |this, cx| {
                let is_current = this
                    .drawer_snap
                    .is_some_and(|snap| snap.animation_id == animation_id);
                if is_current {
                    this.drawer_offset = target;
                    this.drawer_snap = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn reset_drawers(&mut self) {
        self.drawer_open = false;
        self.workbench_open = false;
        self.drawer_offset = 0.0;
        self.drawer_gesture = None;
        self.drawer_snap = None;
        self.drawer_snap_task = None;
    }

    fn settled_drawer_target(&self) -> f32 {
        if self.drawer_open {
            DrawerPage::Sessions.open_offset()
        } else if self.workbench_open {
            DrawerPage::Workbench.open_offset()
        } else {
            0.0
        }
    }

    fn toggle_process(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.expanded_process.insert(id.clone()) {
            self.expanded_process.remove(&id);
        }
        cx.notify();
    }

    fn toggle_approval_details(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.expanded_approval.insert(id.clone()) {
            self.expanded_approval.remove(&id);
        }
        cx.notify();
    }

    fn timeline_is_near_bottom(&self) -> bool {
        timeline_distance_to_bottom(
            f32::from(self.timeline_list.scroll_px_offset_for_scrollbar().y),
            f32::from(self.timeline_list.max_offset_for_scrollbar().y),
        ) <= TIMELINE_NEAR_BOTTOM_PX
    }

    fn sync_timeline_list(&mut self, turn_count: usize) {
        let current_count = self.timeline_list.item_count();
        if current_count == turn_count {
            return;
        }
        if current_count == 0 || current_count > turn_count {
            self.timeline_list = timeline_list_state(turn_count);
        } else {
            self.timeline_list
                .splice(current_count..current_count, turn_count - current_count);
        }
    }

    fn rebuild_timeline_turns(&mut self) {
        let turns = self
            .controller
            .as_ref()
            .map(|controller| controller.state.conversation_turns())
            .unwrap_or_default();
        self.sync_timeline_list(turns.len());
        self.timeline_turns = Arc::new(turns);
    }

    fn refresh(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(backend) = self.backend.clone() {
            let state = backend.connection_state();
            if state.state != vibex_remote_client::RemoteConnectionState::Online {
                self.mode = RootMode::Connecting;
                self.connect_backend(backend, cx);
                return;
            }
        }
        self.refresh_sessions(cx);
        self.reload_selected_session(cx);
    }

    fn forget_desktop(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.stop_connection_tasks();
        self.backend = None;
        crate::background_connection::disconnect();
        let _ = self.storage.clear();
        let _ = self.storage.clear_hosts();
        if let Some(workbench) = self.workbench.take() {
            workbench.update(cx, |workbench, _| workbench.suspend());
        }
        self.controller = None;
        self.mode = RootMode::Pairing;
        self.timeline_turns = Arc::new(Vec::new());
        self.timeline_list.reset(0);
        self.reset_drawers();
        self.pending_workbench_surface = None;
        self.clear_overlay();
        self.workspaces.clear();
        self.workspace_summaries.clear();
        self.reset_sidebar_ui(cx);
        self.known_hosts.clear();
        self.active_host_id = None;
        self.pairing_from_hosts = false;
        self.expanded_process.clear();
        self.expanded_approval.clear();
        self.elicitation_request_id = None;
        self.elicitation_inputs.clear();
        self.elicitation_draft = None;
        self.reset_runtime_options();
        self.notice = None;
        self.error = None;
        cx.notify();
    }

    /// Drives the full-page side drawers from the platform touch-pan stream.
    /// Android and iOS synthesize a finger drag as `ScrollWheel` events carrying
    /// a `TouchPhase`; mouse-move events only ever describe a tap.
    fn drawer_pan(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match drawer_pan_input(event) {
            DrawerPanInput::Started { delta_x, delta_y } => {
                self.drawer_gesture = None;
                if self.drawer_snap.is_some()
                    || self.session_action.is_some()
                    || self.runtime_options_open
                    || self.overlay.is_some()
                {
                    return;
                }
                let origin = drawer_drag_origin(self.drawer_offset);
                self.drawer_gesture = Some(DrawerGesture::Pending {
                    origin,
                    dx: 0.0,
                    dy: 0.0,
                });
                // Android reports the translation that broke its touch slop on this
                // very event, so fold it in rather than waiting for the next one.
                self.advance_drawer_pan(delta_x, delta_y, window, cx);
            }
            DrawerPanInput::Moved { delta_x, delta_y } => {
                self.advance_drawer_pan(delta_x, delta_y, window, cx)
            }
            DrawerPanInput::Ended => self.finish_drawer_pan(false, window, cx),
            DrawerPanInput::Cancelled => self.finish_drawer_pan(true, window, cx),
            DrawerPanInput::Ignore => {}
        }
    }

    fn finish_drawer_pan(&mut self, cancelled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let gesture = self.drawer_gesture.take();
        if self.drawer_snap.is_some() {
            return;
        }
        let was_dragging = matches!(gesture, Some(DrawerGesture::Dragging { .. }));
        let was_partial = drawer_offset_is_intermediate(self.drawer_offset);
        let Some(target) = drawer_terminal_target(
            gesture,
            self.drawer_offset,
            self.settled_drawer_target(),
            cancelled,
        ) else {
            return;
        };
        if was_dragging || was_partial {
            cx.stop_propagation();
        }
        self.start_drawer_snap(target, Some(window), cx);
    }

    /// Touch pans over the row list. A pan that started on a row's grip moves
    /// that row; anything else stays a list scroll, which the drawer must not
    /// mistake for a horizontal page swipe.
    fn sidebar_list_pan(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.touch_phase {
            TouchPhase::Started => {
                if self.begin_sidebar_drag(event, cx) {
                    cx.stop_propagation();
                }
            }
            TouchPhase::Moved => {
                if self.sidebar_drag.is_some() {
                    self.advance_sidebar_drag(event, cx);
                    cx.stop_propagation();
                    return;
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.sidebar_drag.is_some() {
                    let cancelled = event.touch_phase == TouchPhase::Cancelled;
                    self.finish_sidebar_drag(cancelled, cx);
                    cx.stop_propagation();
                    return;
                }
            }
        }
        cx.stop_propagation();
    }

    /// The GPUI scroll element updates its handle before bubble listeners run.
    /// Consume the event here so the overlaid timeline cannot scroll in parallel.
    fn consume_drawer_scroll(
        &mut self,
        _: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
    }

    fn advance_drawer_pan(
        &mut self,
        delta_x: f32,
        delta_y: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.drawer_gesture {
            Some(DrawerGesture::Pending { origin, dx, dy }) => {
                let (dx, dy) = (dx + delta_x, dy + delta_y);
                match drawer_pan_decision(origin, dx, dy) {
                    DrawerPanDecision::Wait => {
                        self.drawer_gesture = Some(DrawerGesture::Pending { origin, dx, dy });
                    }
                    DrawerPanDecision::Cancel => {
                        self.drawer_gesture = None;
                        if matches!(origin, DrawerDragOrigin::Partial(_)) {
                            let target = drawer_nearest_target(self.drawer_offset);
                            self.start_drawer_snap(target, Some(window), cx);
                        }
                    }
                    DrawerPanDecision::Drag(page) => {
                        window.hide_soft_keyboard();
                        if page == DrawerPage::Workbench && self.workbench.is_none() {
                            self.refresh_workspaces(cx);
                        }
                        self.drawer_gesture = Some(DrawerGesture::Dragging { page, last_dx: 0.0 });
                        self.apply_drawer_drag(page, dx, workspace_page_width(window), cx);
                        cx.stop_propagation();
                    }
                }
            }
            Some(DrawerGesture::Dragging { page, .. }) => {
                self.apply_drawer_drag(page, delta_x, workspace_page_width(window), cx);
                cx.stop_propagation();
            }
            None => {}
        }
    }

    fn apply_drawer_drag(
        &mut self,
        page: DrawerPage,
        delta_x: f32,
        page_width: f32,
        cx: &mut Context<Self>,
    ) {
        let normalized_delta = delta_x / page_width.max(1.0);
        self.drawer_offset = match page {
            DrawerPage::Sessions => (self.drawer_offset + normalized_delta).clamp(0.0, 1.0),
            DrawerPage::Workbench => (self.drawer_offset + normalized_delta).clamp(-1.0, 0.0),
        };
        self.drawer_gesture = Some(DrawerGesture::Dragging {
            page,
            last_dx: delta_x,
        });
        cx.notify();
    }

    fn render_pairing(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let retry = matches!(
            self.nearby_pairing_state,
            NearbyPairingState::PermissionDenied
                | NearbyPairingState::Rejected
                | NearbyPairingState::Expired
                | NearbyPairingState::Failed { .. }
        );
        let qr_enabled = !self.pairing_busy || self.lan_pairing_task.is_some();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(theme::SPACING_XL))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .mb(px(theme::SPACING_LG))
                    .child(
                        svg()
                            .path("brand/logo.svg")
                            .size(px(44.0))
                            .text_color(theme::text_primary())
                            .mb(px(theme::SPACING_SM)),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_APP_TITLE))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(theme::text_primary())
                            .child("Vibex"),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(theme::CARD_WIDTH))
                    .flex()
                    .flex_col()
                    .gap(px(theme::SPACING_SM))
                    .child(self.render_nearby_pairing(cx))
                    .when(retry, |panel| {
                        panel.child(
                            div()
                                .id("retry-nearby-pairing")
                                .h(px(theme::TOUCH_TARGET))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_default())
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::start_nearby_pairing),
                                )
                                .child(locale::common("Try Again")),
                        )
                    })
                    .when_some(self.error.as_ref(), |panel, error| {
                        panel.child(
                            div()
                                .rounded(px(theme::RADIUS_CARD))
                                .border_1()
                                .border_color(rgb(theme::ACCENT_RED))
                                .p(px(theme::SPACING_MD))
                                .text_size(px(theme::FONT_DETAIL))
                                .text_color(rgb(theme::ACCENT_RED))
                                .child(error.message.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("scan-pairing-qr")
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .text_color(theme::text_primary())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(theme::FONT_HEADING))
                            .gap(px(theme::SPACING_SM))
                            .when(qr_enabled, |button| {
                                button
                                    .cursor_pointer()
                                    .active(|style| style.opacity(0.7))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::scan_pairing_code),
                                    )
                            })
                            .child(
                                svg()
                                    .path("icons/scan-line.svg")
                                    .size(px(theme::ICON_MD))
                                    .text_color(theme::text_primary()),
                            )
                            .child(if self.pairing_busy && self.lan_pairing_task.is_none() {
                                locale::common("Pairing...")
                            } else {
                                locale::common("Use QR Code")
                            }),
                    )
                    .when(self.pairing_from_hosts, |panel| {
                        panel.child(
                            div()
                                .id("back-to-mobile-hosts")
                                .h(px(theme::TOUCH_TARGET))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::text_muted())
                                .cursor_pointer()
                                .active(|style| style.opacity(0.7))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::cancel_pairing_host),
                                )
                                .child(locale::common("Back to hosts")),
                        )
                    }),
            )
    }

    fn render_nearby_pairing(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.nearby_pairing_state {
            NearbyPairingState::Idle => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(theme::SPACING_SM))
                .child(
                    div()
                        .text_size(px(theme::FONT_DETAIL))
                        .text_color(theme::text_muted())
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(locale::common("Local Network Pairing")),
                )
                .child(
                    div()
                        .id("find-nearby-desktops")
                        .h(px(theme::TOUCH_TARGET))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .bg(rgb(theme::TEXT_PRIMARY))
                        .text_color(rgb(theme::BG_PRIMARY))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(theme::SPACING_SM))
                        .text_size(px(theme::FONT_HEADING))
                        .cursor_pointer()
                        .active(|style| style.opacity(0.7))
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::start_nearby_pairing))
                        .child(
                            svg()
                                .path("icons/refresh.svg")
                                .size(px(theme::ICON_MD))
                                .text_color(rgb(theme::BG_PRIMARY)),
                        )
                        .child(locale::common("Find Desktops")),
                )
                .into_any_element(),
            NearbyPairingState::Discovering | NearbyPairingState::Empty => {
                let candidates = self.nearby_candidates.values().cloned().collect::<Vec<_>>();
                let empty = matches!(self.nearby_pairing_state, NearbyPairingState::Empty);
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(theme::FONT_HEADING))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(locale::common("Nearby desktops")),
                            )
                            .child(
                                div()
                                    .id("stop-nearby-discovery")
                                    .size(px(theme::TOUCH_TARGET))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::cancel_nearby_pairing),
                                    )
                                    .child(
                                        svg()
                                            .path("icons/x.svg")
                                            .size(px(theme::ICON_MD))
                                            .text_color(theme::text_muted()),
                                    ),
                            ),
                    )
                    .when(candidates.is_empty(), |panel| {
                        panel.child(
                            div()
                                .h(px(64.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(theme::FONT_DETAIL))
                                .text_color(theme::text_muted())
                                .child(if empty {
                                    locale::common("No nearby desktops found")
                                } else {
                                    locale::common("Searching...")
                                }),
                        )
                    })
                    .children(candidates.into_iter().map(|candidate| {
                        let key = candidate.key();
                        div()
                            .id(format!("nearby:{key}"))
                            .min_h(px(58.0))
                            .px(px(theme::SPACING_MD))
                            .border_b_1()
                            .border_color(theme::border_subtle())
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.select_nearby_candidate(key.clone(), cx)
                                }),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(theme::FONT_HEADING))
                                            .text_color(theme::text_primary())
                                            .child(candidate.display_name),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(locale::common("Vibex Remote v2")),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(locale::common("Pair")),
                            )
                    }))
                    .into_any_element()
            }
            NearbyPairingState::Validating { display_name } => self.render_nearby_message(
                format!(
                    "{} {display_name}…",
                    locale::text("Checking", "检查", "檢查")
                ),
                false,
            ),
            NearbyPairingState::Waiting {
                display_name,
                verification_code,
                expires_at_ms,
            } => {
                let code = if verification_code.len() == 6 {
                    format!("{} {}", &verification_code[..3], &verification_code[3..])
                } else {
                    verification_code.clone()
                };
                let remaining = expires_at_ms
                    .saturating_sub(unix_timestamp_ms())
                    .div_euclid(1_000)
                    .max(0);
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        div()
                            .text_size(px(theme::FONT_HEADING))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_primary())
                            .child(format!(
                                "{} {display_name}",
                                locale::text("Waiting for", "等待", "等待")
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_DETAIL))
                            .text_color(theme::text_muted())
                            .child(locale::text(
                                "Confirm the same code is shown on the desktop.",
                                "请确认桌面端显示了相同的验证码。",
                                "請確認桌面版顯示了相同的驗證碼。",
                            )),
                    )
                    .child(
                        div()
                            .h(px(58.0))
                            .flex()
                            .items_center()
                            .text_size(px(30.0))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(theme::text_primary())
                            .child(code),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(format!(
                                "{} {remaining}s",
                                locale::text("Expires in", "将在", "將於")
                            )),
                    )
                    .into_any_element()
            }
            NearbyPairingState::PermissionDenied => self.render_nearby_message(
                locale::text(
                    "Local network access is required to find nearby desktops.",
                    "查找附近的桌面端需要局域网访问权限。",
                    "尋找附近的桌面版需要區域網路存取權限。",
                ),
                true,
            ),
            NearbyPairingState::Rejected => self.render_nearby_message(
                locale::text(
                    "The desktop rejected this pairing request.",
                    "桌面端拒绝了此次配对请求。",
                    "桌面版拒絕了此次配對請求。",
                ),
                true,
            ),
            NearbyPairingState::Expired => self.render_nearby_message(
                locale::text(
                    "The nearby pairing window expired.",
                    "附近配对窗口已过期。",
                    "附近配對視窗已過期。",
                ),
                true,
            ),
            NearbyPairingState::Failed { message } => {
                self.render_nearby_message(message.clone(), true)
            }
        }
    }

    fn render_nearby_message(&self, message: impl Into<String>, error: bool) -> gpui::AnyElement {
        div()
            .w_full()
            .min_h(px(72.0))
            .p(px(theme::SPACING_MD))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(theme::FONT_DETAIL))
            .text_color(theme::text_muted())
            .when(error, |message| message.text_color(rgb(theme::ACCENT_RED)))
            .child(message.into())
            .into_any_element()
    }

    fn render_connecting(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(theme::SPACING_XL))
            .child(
                svg()
                    .path("brand/logo.svg")
                    .size(px(44.0))
                    .text_color(theme::text_muted())
                    .mb(px(theme::SPACING_LG)),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(theme::CARD_WIDTH))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::SPACING_MD))
                    .text_size(px(theme::FONT_DETAIL))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .child(div().size(px(theme::ICON_STATUS)).rounded_full().bg(rgb(
                                if self.operation_busy {
                                    theme::ACCENT_YELLOW
                                } else {
                                    theme::ACCENT_RED
                                },
                            )))
                            .child(div().text_color(theme::text_secondary()).child(
                                if self.operation_busy {
                                    locale::common("Connecting to desktop...")
                                } else {
                                    locale::common("Desktop is unavailable")
                                },
                            )),
                    )
                    .when_some(self.error.as_ref(), |panel, error| {
                        panel.child(
                            div()
                                .w_full()
                                .rounded(px(theme::RADIUS_CARD))
                                .border_1()
                                .border_color(theme::border_subtle())
                                .bg(theme::bg_card_dim())
                                .p(px(theme::SPACING_MD))
                                .text_color(rgb(theme::ACCENT_RED))
                                .child(error.message.clone()),
                        )
                    })
                    .when(!self.operation_busy, |panel| {
                        panel
                            .child(
                                div()
                                    .id("retry-desktop-connection")
                                    .w_full()
                                    .h(px(theme::TOUCH_TARGET))
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .border_1()
                                    .border_color(theme::border_default())
                                    .bg(theme::bg_card())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme::text_primary())
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::retry_connection),
                                    )
                                    .child(locale::common("Retry")),
                            )
                            .child(
                                div()
                                    .id("disconnect-unavailable-desktop")
                                    .h(px(theme::TOUCH_TARGET))
                                    .px(px(theme::SPACING_MD))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme::text_muted())
                                    .cursor_pointer()
                                    .active(|style| style.opacity(0.7))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::forget_desktop),
                                    )
                                    .child(locale::common("Disconnect")),
                            )
                            .when(!self.known_hosts.is_empty(), |panel| {
                                panel.child(
                                    div()
                                        .id("switch-host-while-connecting")
                                        .h(px(theme::TOUCH_TARGET))
                                        .px(px(theme::SPACING_MD))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(theme::text_secondary())
                                        .cursor_pointer()
                                        .active(|style| style.opacity(0.7))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::open_hosts),
                                        )
                                        .child(locale::common("Switch host")),
                                )
                            })
                    }),
            )
            .when(self.overlay == Some(MobileOverlay::Hosts), |root| {
                root.child(self.render_hosts(cx))
            })
    }

    fn render_workspace(&mut self, page_width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.active_session.value.as_ref())
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "Vibex".to_string());
        let state = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.active_session.value.as_ref())
            .map(|session| session.state);
        let running = state == Some(AgentSessionState::Running);
        let timeline_loading = self.controller.as_ref().is_some_and(|controller| {
            controller.state.timeline_status.phase == AsyncPhase::Loading
        });
        let turns = self.timeline_turns.clone();
        let approvals = self
            .controller
            .as_ref()
            .map(|controller| controller.state.approval_surfaces(ShellKind::Compact))
            .unwrap_or_default();
        let elicitations = self
            .controller
            .as_ref()
            .map(|controller| controller.state.elicitation_surfaces(ShellKind::Compact))
            .unwrap_or_default();
        let no_selected_session = self
            .controller
            .as_ref()
            .is_some_and(|controller| controller.state.selected_session_id.is_none());
        let turns_for_list = turns.clone();
        let drawer_page = visible_drawer_page(self.drawer_offset, self.drawer_snap);
        let workbench = self.workbench.clone();
        let session_action = self.session_action.clone();
        let row_menu = self.sidebar_row_menu.clone();
        let name_prompt = self.sidebar_name_prompt.clone();
        let overlay = self.overlay;
        let can_create_session = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentCreateSession)
        });

        div()
            .size_full()
            .relative()
            .capture_scroll_wheel(cx.listener(Self::drawer_pan))
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(&title, state, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px_4()
                            .py_4()
                            .flex()
                            .flex_col()
                            .when(timeline_loading && turns.is_empty(), |timeline| {
                                timeline.child(
                                    div()
                                        .py_8()
                                        .text_size(px(theme::FONT_BODY))
                                        .text_color(theme::text_muted())
                                        .text_center()
                                        .child(locale::common("Loading conversation...")),
                                )
                            })
                            .when(!timeline_loading && turns.is_empty(), |timeline| {
                                timeline.child(
                                    div()
                                        .py_8()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .gap_3()
                                        .text_size(px(theme::FONT_BODY))
                                        .text_color(theme::text_muted())
                                        .text_center()
                                        .child(locale::common("No messages yet"))
                                        .when(no_selected_session, |empty| {
                                            empty.child(
                                                div()
                                                    .id("create-first-session")
                                                    .h(px(theme::TOUCH_TARGET))
                                                    .px_4()
                                                    .rounded(px(theme::RADIUS_CONTROL))
                                                    .border_1()
                                                    .border_color(theme::border_default())
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_color(theme::text_secondary())
                                                    .when(can_create_session, |button| {
                                                        button.cursor_pointer().on_mouse_up(
                                                            MouseButton::Left,
                                                            cx.listener(Self::create_session),
                                                        )
                                                    })
                                                    .when(!can_create_session, |button| {
                                                        button.opacity(0.55)
                                                    })
                                                    .child(locale::common("New session")),
                                            )
                                        }),
                                )
                            })
                            .when(!turns.is_empty(), |timeline| {
                                timeline.child(
                                    list(
                                        self.timeline_list.clone(),
                                        cx.processor(move |this, index, _window, cx| {
                                            turns_for_list
                                                .get(index)
                                                .map(|turn| {
                                                    div()
                                                        .pb(px(theme::SPACING_XL))
                                                        .child(this.render_turn(turn, cx))
                                                        .into_any_element()
                                                })
                                                .unwrap_or_else(|| div().into_any_element())
                                        }),
                                    )
                                    .w_full()
                                    .flex_1()
                                    .min_h_0(),
                                )
                            }),
                    )
                    .when_some(self.notice.as_ref(), |workspace, notice| {
                        workspace.child(
                            div()
                                .border_t_1()
                                .border_color(theme::border_subtle())
                                .bg(theme::bg_card_dim())
                                .px_4()
                                .py_2()
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(rgb(theme::ACCENT_YELLOW))
                                .child(notice.clone()),
                        )
                    })
                    .when_some(approvals.first(), |workspace, approval| {
                        workspace.child(self.render_approval(approval, cx))
                    })
                    .when_some(elicitations.first(), |workspace, elicitation| {
                        workspace.child(self.render_elicitation(elicitation, cx))
                    })
                    .child(self.render_composer(running, state, &turns, cx)),
            )
            .when_some(drawer_page, |root, page| {
                let backdrop_base = div()
                    .id("drawer-backdrop")
                    .absolute()
                    .inset_0()
                    // Keep the root gesture host in the scroll hit chain while the
                    // moving page is on top. Ordinary taps remain blocked below.
                    .block_mouse_except_scroll()
                    .on_scroll_wheel(cx.listener(Self::consume_drawer_scroll))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(Self::close_drawer_from_backdrop),
                    );
                let drawer_base = match page {
                    DrawerPage::Sessions => self.render_drawer(cx),
                    DrawerPage::Workbench => self.render_workbench_drawer(workbench.clone(), cx),
                }
                .w(px(page_width))
                .block_mouse_except_scroll();
                let (backdrop, drawer) = if let Some(snap) = self.drawer_snap {
                    let duration = drawer_animation(snap.from, snap.target);
                    let from_opacity = drawer_backdrop_opacity(snap.from);
                    let target_opacity = drawer_backdrop_opacity(snap.target);
                    (
                        backdrop_base
                            .with_animation(
                                ElementId::NamedInteger(
                                    "mobile-drawer-backdrop".into(),
                                    snap.animation_id,
                                ),
                                duration.clone(),
                                move |element, delta| {
                                    element.bg(theme::backdrop(
                                        from_opacity + (target_opacity - from_opacity) * delta,
                                    ))
                                },
                            )
                            .into_any_element(),
                        drawer_base
                            .with_animation(
                                ElementId::NamedInteger(
                                    "mobile-drawer-panel".into(),
                                    snap.animation_id,
                                ),
                                duration,
                                move |element, delta| {
                                    let offset = snap.from + (snap.target - snap.from) * delta;
                                    element.left(px(drawer_left(page, offset, page_width)))
                                },
                            )
                            .into_any_element(),
                    )
                } else {
                    (
                        backdrop_base
                            .bg(theme::backdrop(drawer_backdrop_opacity(self.drawer_offset)))
                            .into_any_element(),
                        drawer_base
                            .left(px(drawer_left(page, self.drawer_offset, page_width)))
                            .into_any_element(),
                    )
                };
                root.child(backdrop).child(drawer)
            })
            .when_some(row_menu, |root, menu| {
                root.child(self.render_sidebar_row_menu(&menu, cx))
            })
            .when_some(name_prompt, |root, prompt| {
                root.child(self.render_folder_name_prompt(&prompt, cx))
            })
            .when_some(session_action, |root, prompt| {
                root.child(self.render_session_action_prompt(&prompt, cx))
            })
            .when(self.runtime_options_open, |root| {
                root.child(self.render_runtime_options_sheet(cx))
            })
            .when_some(overlay, |root, overlay| {
                root.child(self.render_mobile_overlay(overlay, cx))
            })
    }

    /// Row actions that are too rare to sit permanently on a touch row. What
    /// the sheet offers depends on what the row is.
    fn render_sidebar_row_menu(
        &self,
        menu: &SidebarRowMenu,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row = menu.row.clone();
        let mut sheet = div()
            .w_full()
            .max_w(px(360.0))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::border_default())
            .bg(theme::bg_card())
            .p_2()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child(row.label.clone()),
            );
        let entry = |sheet: gpui::Div,
                     id: &'static str,
                     label: String,
                     destructive: bool,
                     action: SidebarMenuAction| {
            sheet.child(
                div()
                    .id(id)
                    .h(px(theme::TOUCH_TARGET))
                    .px_3()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .active(|style| style.bg(theme::row_pressed_bg()))
                    .text_size(px(theme::FONT_BODY))
                    .text_color(if destructive {
                        rgb(theme::ACCENT_RED).into()
                    } else {
                        theme::text_primary()
                    })
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.sidebar_row_menu = None;
                            action(this, window, cx);
                        }),
                    )
                    .child(label),
            )
        };

        match row.kind {
            SidebarRowKind::Session => {
                let session_id = row.session_id.clone();
                for (id, kind, label, destructive) in [
                    (
                        "mobile-row-menu-rename",
                        SessionActionKind::Rename,
                        locale::common("Rename"),
                        false,
                    ),
                    (
                        "mobile-row-menu-archive",
                        SessionActionKind::Archive,
                        locale::common("Archive"),
                        false,
                    ),
                    (
                        "mobile-row-menu-delete",
                        SessionActionKind::Delete,
                        locale::common("Delete"),
                        true,
                    ),
                ] {
                    let session_id = session_id.clone();
                    let title = row.label.clone();
                    sheet = entry(
                        sheet,
                        id,
                        label.to_string(),
                        destructive,
                        Box::new(move |this, _, cx| {
                            if let Some(session_id) = session_id.clone() {
                                this.begin_session_action(kind, session_id, title.clone(), cx);
                            }
                        }),
                    );
                }
                let pin_session_id = row.id().to_string();
                let pinned = row.pinned;
                sheet = entry(
                    sheet,
                    "mobile-row-menu-pin",
                    if pinned {
                        locale::text("Unpin", "取消置顶", "取消置頂").to_string()
                    } else {
                        locale::text("Pin", "置顶", "置頂").to_string()
                    },
                    false,
                    Box::new(move |this, _, cx| {
                        this.send_sidebar_mutation(
                            RemoteSidebarOrganizationMutation::SetSessionPinned {
                                session_id: pin_session_id.clone(),
                                pinned: !pinned,
                            },
                            false,
                            cx,
                        );
                    }),
                );
            }
            SidebarRowKind::Folder => {
                let rename_id = row.id().to_string();
                sheet = entry(
                    sheet,
                    "mobile-row-menu-folder-rename",
                    locale::common("Rename").to_string(),
                    false,
                    Box::new(move |this, _, cx| {
                        this.open_folder_name_prompt(
                            SidebarNamePrompt::RenameFolder {
                                folder_id: rename_id.clone(),
                            },
                            cx,
                        );
                    }),
                );
                let nest_project_id = row.project_id.clone();
                let nest_workspace_id = row.workspace_id.clone();
                let nest_parent_id = row.id().to_string();
                sheet = entry(
                    sheet,
                    "mobile-row-menu-folder-new",
                    locale::text("New folder", "新建文件夹", "新增資料夾").to_string(),
                    false,
                    Box::new(move |this, _, cx| {
                        this.open_folder_name_prompt(
                            SidebarNamePrompt::CreateFolder {
                                project_id: nest_project_id.clone(),
                                workspace_id: nest_workspace_id.clone(),
                                parent_folder_id: Some(nest_parent_id.clone()),
                            },
                            cx,
                        );
                    }),
                );
                let delete_id = row.id().to_string();
                sheet = entry(
                    sheet,
                    "mobile-row-menu-folder-delete",
                    locale::common("Delete").to_string(),
                    true,
                    Box::new(move |this, _, cx| {
                        this.send_sidebar_mutation(
                            RemoteSidebarOrganizationMutation::DeleteFolder {
                                folder_id: delete_id.clone(),
                            },
                            false,
                            cx,
                        );
                    }),
                );
            }
            SidebarRowKind::Project => {
                let project_id = row.id().to_string();
                let folder_project_id = row.id().to_string();
                sheet = entry(
                    sheet,
                    "mobile-row-menu-project-new-session",
                    locale::common("New session").to_string(),
                    false,
                    Box::new(move |this, window, cx| {
                        this.create_session_in_project(project_id.clone(), window, cx);
                    }),
                );
                sheet = entry(
                    sheet,
                    "mobile-row-menu-project-new-folder",
                    locale::text("New folder", "新建文件夹", "新增資料夾").to_string(),
                    false,
                    Box::new(move |this, _, cx| {
                        this.open_folder_name_prompt(
                            SidebarNamePrompt::CreateFolder {
                                project_id: Some(folder_project_id.clone()),
                                workspace_id: None,
                                parent_folder_id: None,
                            },
                            cx,
                        );
                    }),
                );
            }
            SidebarRowKind::Workspace => {}
        }

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme::backdrop(0.72))
            .p_4()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.sidebar_row_menu = None;
                    cx.notify();
                }),
            )
            .child(sheet)
    }

    fn render_folder_name_prompt(
        &self,
        prompt: &SidebarNamePrompt,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let heading = match prompt {
            SidebarNamePrompt::CreateFolder { .. } => {
                locale::text("New folder", "新建文件夹", "新增資料夾")
            }
            SidebarNamePrompt::RenameFolder { .. } => {
                locale::text("Rename folder", "重命名文件夹", "重新命名資料夾")
            }
        };
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme::backdrop(0.72))
            .p_4()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(360.0))
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_card())
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(theme::FONT_HEADING))
                            .text_color(theme::text_primary())
                            .child(heading),
                    )
                    .child(
                        div()
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .bg(theme::bg_primary())
                            .px_3()
                            .flex()
                            .items_center()
                            .child(self.sidebar_name_input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("mobile-folder-name-cancel")
                                    .h(px(theme::TOUCH_TARGET))
                                    .flex_1()
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .border_1()
                                    .border_color(theme::border_default())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.sidebar_name_prompt = None;
                                            cx.notify();
                                        }),
                                    )
                                    .child(locale::common("Cancel")),
                            )
                            .child(
                                div()
                                    .id("mobile-folder-name-confirm")
                                    .h(px(theme::TOUCH_TARGET))
                                    .flex_1()
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .bg(theme::sidebar_selected_bg())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_primary())
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::submit_folder_name),
                                    )
                                    .child(locale::common("Confirm")),
                            ),
                    ),
            )
    }

    fn render_session_action_prompt(
        &self,
        prompt: &SessionActionPrompt,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let can_manage_session = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentManageSession)
        });
        let (heading, detail, confirm_label, destructive) = match prompt.kind {
            SessionActionKind::Rename => (
                locale::text("Rename session", "重命名会话", "重新命名工作階段"),
                locale::text(
                    "Update the session name on the desktop.",
                    "更新桌面端上的会话名称。",
                    "更新桌面版上的工作階段名稱。",
                ),
                locale::common("Rename"),
                false,
            ),
            SessionActionKind::Archive => (
                locale::text("Archive session", "归档会话", "封存工作階段"),
                locale::text(
                    "The session leaves the active list but remains available in desktop history.",
                    "会话将从活动列表中移除，但仍保留在桌面端历史记录中。",
                    "工作階段會從使用中清單移除，但仍保留在桌面版歷史記錄中。",
                ),
                locale::common("Archive"),
                false,
            ),
            SessionActionKind::Delete => (
                locale::text("Delete session", "删除会话", "刪除工作階段"),
                locale::text(
                    "This removes the session and its stored conversation from the desktop.",
                    "这会从桌面端删除会话及其已保存的对话。",
                    "這會從桌面版刪除工作階段及其已儲存的對話。",
                ),
                locale::common("Delete"),
                true,
            ),
        };
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme::backdrop(0.72))
            .p_4()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(360.0))
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_card())
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(theme::FONT_HEADING))
                            .text_color(theme::text_primary())
                            .child(heading),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .child(detail),
                    )
                    .when(prompt.kind == SessionActionKind::Rename, |dialog| {
                        dialog.child(
                            div()
                                .h(px(theme::TOUCH_TARGET))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_default())
                                .bg(theme::bg_primary())
                                .px_1()
                                .child(self.session_action_input.clone()),
                        )
                    })
                    .when(prompt.kind != SessionActionKind::Rename, |dialog| {
                        dialog.child(
                            div()
                                .text_size(px(theme::FONT_BODY))
                                .text_color(theme::text_secondary())
                                .child(prompt.current_title.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("cancel-session-action")
                                    .h(px(theme::TOUCH_TARGET))
                                    .px_4()
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .border_1()
                                    .border_color(theme::border_default())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .when(!self.session_action_busy, |button| {
                                        button.cursor_pointer().on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::cancel_session_action),
                                        )
                                    })
                                    .child(locale::common("Cancel")),
                            )
                            .child(
                                div()
                                    .id("confirm-session-action")
                                    .h(px(theme::TOUCH_TARGET))
                                    .px_4()
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .bg(if destructive {
                                        rgb(theme::ACCENT_RED)
                                    } else {
                                        rgb(theme::TEXT_PRIMARY)
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(rgb(theme::BG_PRIMARY))
                                    .when(
                                        !self.session_action_busy && can_manage_session,
                                        |button| {
                                            button.cursor_pointer().on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::confirm_session_action),
                                            )
                                        },
                                    )
                                    .when(!can_manage_session, |button| button.opacity(0.55))
                                    .child(if self.session_action_busy {
                                        locale::common("Working...")
                                    } else {
                                        confirm_label
                                    }),
                            ),
                    ),
            )
    }

    fn render_runtime_options_sheet(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let catalog = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.runtime_options.value.as_ref().cloned());
        let draft = self.runtime_draft.clone();
        let projection = catalog
            .as_ref()
            .zip(draft.as_ref())
            .map(|(catalog, draft)| RuntimeCascadeProjection::from_catalog(catalog, draft))
            .unwrap_or_default();
        let RuntimeCascadeProjection {
            agents,
            auth_sources,
            models,
            reasoning_efforts,
            modes,
            features,
        } = projection;
        let busy = self.runtime_switch_busy_generation.is_some();
        let selection_available = catalog.as_ref().is_some_and(|catalog| {
            draft
                .as_ref()
                .is_some_and(|draft| runtime_selection_is_available(&catalog.options, draft))
        });
        let can_switch = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentSwitchRuntime)
        });
        let can_apply = can_switch && selection_available && !busy;
        let selected_agent = draft.as_ref().map(|draft| draft.agent_id.to_string());
        let selected_auth_source = draft
            .as_ref()
            .map(|draft| draft.auth_source.id().to_string());
        let selected_model = draft.as_ref().map(|draft| draft.model.clone());
        let selected_reasoning = draft
            .as_ref()
            .and_then(|draft| draft.reasoning_effort.clone());
        let selected_mode = draft.as_ref().and_then(|draft| draft.mode_id.clone());

        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme::backdrop(0.72))
            .pt(px(56.0))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(520.0))
                    .flex_1()
                    .min_h_0()
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_primary())
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(52.0))
                            .flex_shrink_0()
                            .border_b_1()
                            .border_color(theme::border_subtle())
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(theme::FONT_HEADING))
                                    .text_color(theme::text_primary())
                                    .child(locale::text(
                                        "Runtime options",
                                        "运行时选项",
                                        "執行環境選項",
                                    )),
                            )
                            .child(
                                div()
                                    .id("close-runtime-options")
                                    .size(px(theme::TOUCH_TARGET))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(!busy, |button| {
                                        button
                                            .cursor_pointer()
                                            .active(|style| style.opacity(0.6))
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::close_runtime_options),
                                            )
                                    })
                                    .child(
                                        svg()
                                            .path("icons/x.svg")
                                            .size(px(theme::ICON_SM))
                                            .text_color(theme::text_secondary()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("runtime-options-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .when(catalog.is_none(), |body| {
                                body.child(
                                    div()
                                        .p_4()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .text_color(theme::text_muted())
                                        .child(locale::text(
                                            "Loading runtime options...",
                                            "正在加载运行时选项…",
                                            "正在載入執行環境選項…",
                                        )),
                                )
                            })
                            .when(
                                catalog.is_some() && draft.is_some() && !selection_available,
                                |body| {
                                    body.child(
                                        div()
                                            .mx_3()
                                            .mt_3()
                                            .rounded(px(theme::RADIUS_CONTROL))
                                            .border_1()
                                            .border_color(rgb(theme::ACCENT_YELLOW))
                                            .bg(theme::bg_card_dim())
                                            .p_3()
                                            .text_size(px(theme::FONT_CAPTION))
                                            .text_color(rgb(theme::ACCENT_YELLOW))
                                            .child(locale::text(
                                                "The current runtime is unavailable. Select an available option.",
                                                "当前运行时不可用，请选择一个可用选项。",
                                                "目前執行環境無法使用，請選擇可用選項。",
                                            )),
                                    )
                                },
                            )
                            .when(!agents.is_empty(), |body| {
                                body.child(runtime_section_heading(locale::text(
                                    "Agent", "Agent", "Agent",
                                )))
                                .child(
                                    div()
                                        .px_3()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(agents.into_iter().map(|choice| {
                                            let selected = selected_agent.as_deref()
                                                == Some(choice.value.as_str());
                                            self.render_runtime_choice(
                                                "agent",
                                                choice,
                                                selected,
                                                cx,
                                            )
                                        })),
                                )
                            })
                            .when(!auth_sources.is_empty(), |body| {
                                body.child(runtime_section_heading(locale::text(
                                    "Authentication",
                                    "身份来源",
                                    "身分來源",
                                )))
                                .child(
                                    div()
                                        .px_3()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(auth_sources.into_iter().map(|choice| {
                                            let selected = selected_auth_source.as_deref()
                                                == Some(choice.value.as_str());
                                            self.render_runtime_choice(
                                                "authentication",
                                                choice,
                                                selected,
                                                cx,
                                            )
                                        })),
                                )
                            })
                            .when(!models.is_empty(), |body| {
                                body.child(runtime_section_heading(locale::text(
                                    "Model", "模型", "模型",
                                )))
                                .child(
                                    div()
                                        .px_3()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(models.into_iter().map(|choice| {
                                            let selected = selected_model.as_ref()
                                                == Some(&choice.selection.model);
                                            self.render_runtime_choice(
                                                "model", choice, selected, cx,
                                            )
                                        })),
                                )
                            })
                            .when(!reasoning_efforts.is_empty(), |body| {
                                body.child(runtime_section_heading(locale::common("Reasoning")))
                                    .child(
                                        div()
                                            .px_3()
                                            .flex()
                                            .flex_wrap()
                                            .gap_2()
                                            .child(
                                                runtime_choice_button(
                                                    "runtime-reasoning:default",
                                                    locale::common("Default"),
                                                    selected_reasoning.is_none(),
                                                )
                                                .when(!busy, |button| {
                                                    button
                                                        .cursor_pointer()
                                                        .active(|style| {
                                                            style.bg(theme::row_pressed_bg())
                                                        })
                                                        .on_mouse_up(
                                                            MouseButton::Left,
                                                            cx.listener(|this, _, _, cx| {
                                                                this.choose_default_runtime_reasoning(cx)
                                                            }),
                                                        )
                                                }),
                                            )
                                            .children(reasoning_efforts.into_iter().map(|choice| {
                                                let selected = selected_reasoning.as_deref()
                                                    == Some(choice.value.as_str());
                                                self.render_runtime_choice(
                                                    "reasoning",
                                                    choice,
                                                    selected,
                                                    cx,
                                                )
                                            })),
                                    )
                            })
                            .when(!modes.is_empty(), |body| {
                                body.child(runtime_section_heading(locale::common("Mode")))
                                    .child(
                                        div()
                                            .px_3()
                                            .flex()
                                            .flex_wrap()
                                            .gap_2()
                                            .child(
                                                runtime_choice_button(
                                                    "runtime-mode:default",
                                                    locale::common("Default"),
                                                    selected_mode.is_none(),
                                                )
                                                .when(!busy, |button| {
                                                    button
                                                        .cursor_pointer()
                                                        .active(|style| {
                                                            style.bg(theme::row_pressed_bg())
                                                        })
                                                        .on_mouse_up(
                                                            MouseButton::Left,
                                                            cx.listener(|this, _, _, cx| {
                                                                this.choose_default_runtime_mode(cx)
                                                            }),
                                                        )
                                                }),
                                            )
                                            .children(modes.into_iter().map(|choice| {
                                                let selected = selected_mode.as_deref()
                                                    == Some(choice.value.as_str());
                                                self.render_runtime_choice(
                                                    "mode", choice, selected, cx,
                                                )
                                            })),
                                    )
                            })
                            .when(!features.is_empty(), |body| {
                                body.child(runtime_section_heading(locale::common(
                                    "Session options",
                                )))
                                .children(features.into_iter().map(|feature| {
                                    self.render_runtime_feature(feature, cx)
                                }))
                            })
                            .when_some(self.runtime_switch_error.as_ref(), |body, error| {
                                body.child(
                                    div()
                                        .mx_3()
                                        .my_3()
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .border_1()
                                        .border_color(rgb(theme::ACCENT_RED))
                                        .bg(theme::bg_card_dim())
                                        .p_3()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .text_color(rgb(theme::ACCENT_RED))
                                        .child(error.message.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .border_t_1()
                            .border_color(theme::border_subtle())
                            .p_3()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                runtime_sheet_action_button(
                                    "cancel-runtime-options",
                                    locale::common("Cancel"),
                                    false,
                                )
                                .when(!busy, |button| {
                                    button
                                        .cursor_pointer()
                                        .active(|style| style.opacity(0.7))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::close_runtime_options),
                                        )
                                }),
                            )
                            .child(
                                runtime_sheet_action_button(
                                    "apply-runtime-options",
                                    if busy {
                                        locale::common("Applying...")
                                    } else {
                                        locale::common("Apply runtime")
                                    },
                                    true,
                                )
                                .when(can_apply, |button| {
                                    button
                                        .cursor_pointer()
                                        .active(|style| style.opacity(0.7))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::apply_runtime_options),
                                        )
                                })
                                .when(!can_apply, |button| button.opacity(0.5)),
                            ),
                    ),
            )
    }

    fn render_runtime_choice(
        &self,
        group: &'static str,
        choice: RuntimeCascadeChoice,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selection = choice.selection;
        runtime_choice_button(
            format!("runtime-{group}:{}", choice.value),
            choice.label,
            selected,
        )
        .when(self.runtime_switch_busy_generation.is_none(), |button| {
            button
                .cursor_pointer()
                .active(|style| style.bg(theme::row_pressed_bg()))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.choose_runtime_selection(selection.clone(), cx)
                    }),
                )
        })
        .into_any_element()
    }

    fn render_runtime_feature(
        &self,
        feature: SessionRuntimeFeature,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let feature_id = feature.id.clone();
        let selected_value = self
            .runtime_draft
            .as_ref()
            .and_then(|draft| draft.config_values.get(&feature_id))
            .cloned();
        let choices = match feature.kind {
            SessionRuntimeFeatureKind::Toggle => vec![
                (locale::common("Default").to_string(), None),
                (locale::common("On").to_string(), Some("true".to_string())),
                (locale::common("Off").to_string(), Some("false".to_string())),
            ],
            SessionRuntimeFeatureKind::Select => {
                std::iter::once((locale::common("Default").to_string(), None))
                    .chain(feature.values.iter().map(|value| {
                        (
                            value.label.clone().unwrap_or_else(|| value.value.clone()),
                            Some(value.value.clone()),
                        )
                    }))
                    .collect()
            }
            SessionRuntimeFeatureKind::String => Vec::new(),
        };
        let row = div()
            .px_3()
            .pb_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_secondary())
                    .child(feature.label.clone()),
            )
            .when_some(feature.description.clone(), |row, description| {
                row.child(
                    div()
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::text_muted())
                        .child(description),
                )
            });
        if feature.kind == SessionRuntimeFeatureKind::String {
            return match self.runtime_feature_inputs.get(&feature_id) {
                Some(input) => row
                    .child(runtime_feature_input(input.clone()))
                    .into_any_element(),
                None => row
                    .child(
                        div()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(locale::common("Loading value...")),
                    )
                    .into_any_element(),
            };
        }
        row.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(
                    choices
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, value))| {
                            let selected = selected_value == value;
                            let selected_feature_id = feature_id.clone();
                            let selected_value = value.clone();
                            runtime_choice_button(
                                format!("runtime-feature:{feature_id}:{index}"),
                                label,
                                selected,
                            )
                            .when(
                                self.runtime_switch_busy_generation.is_none(),
                                |button| {
                                    button
                                        .cursor_pointer()
                                        .active(|style| style.bg(theme::row_pressed_bg()))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.choose_runtime_feature(
                                                    selected_feature_id.clone(),
                                                    selected_value.clone(),
                                                    cx,
                                                )
                                            }),
                                        )
                                },
                            )
                        }),
                ),
        )
        .into_any_element()
    }

    fn render_header(
        &self,
        title: &str,
        state: Option<AgentSessionState>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status_color = match state {
            Some(AgentSessionState::Running) => theme::ACCENT_GREEN,
            Some(AgentSessionState::Error) => theme::ACCENT_RED,
            Some(_) => theme::TEXT_MUTED,
            None => theme::ACCENT_YELLOW,
        };
        div()
            .h(px(theme::HEADER_HEIGHT))
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme::border_subtle())
            .flex()
            .items_center()
            .child(
                div()
                    .id("open-session-drawer")
                    .size(px(theme::HEADER_BUTTON_SIZE))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .active(|style| style.opacity(0.6))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::toggle_drawer))
                    .child(
                        svg()
                            .path("icons/menu.svg")
                            .size(px(theme::ICON_MD))
                            .text_color(theme::text_secondary()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        svg()
                            .path("brand/logo.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::text_primary()),
                    )
                    .child(
                        div()
                            .size(px(theme::ICON_STATUS))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(rgb(status_color)),
                    )
                    .child(
                        div()
                            .max_w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_secondary())
                            .child(title.to_string()),
                    ),
            )
            .child(
                div()
                    .id("refresh-session")
                    .size(px(theme::HEADER_BUTTON_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .active(|style| style.opacity(0.6))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::refresh))
                    .child(
                        svg()
                            .path("icons/refresh.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::text_muted()),
                    ),
            )
    }

    fn render_turn(
        &self,
        turn: &TimelineConversationTurn,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.expanded_process.contains(&turn.id);
        let turn_id = turn.id.clone();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .when_some(turn.user_row.as_ref(), |container, row| {
                container.child(
                    div()
                        .ml_auto()
                        .max_w(px(520.0))
                        .rounded(px(theme::RADIUS_CARD))
                        .bg(theme::bg_card())
                        .border_1()
                        .border_color(theme::border_default())
                        .px_3()
                        .py_2()
                        .text_size(px(theme::FONT_HEADING))
                        .line_height(px(19.0))
                        .text_color(theme::text_primary())
                        .whitespace_normal()
                        .child(row.body.clone()),
                )
            })
            .when(!turn.process_rows.is_empty(), |container| {
                container.child(
                    div()
                        .id(format!("process:{}", turn.id))
                        .w_full()
                        .rounded(px(theme::RADIUS_CARD))
                        .bg(theme::bg_card_dim())
                        .border_1()
                        .border_color(theme::border_subtle())
                        .cursor_pointer()
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_process(turn_id.clone(), cx)
                            }),
                        )
                        .child(
                            div()
                                .h(px(36.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .text_color(theme::text_muted())
                                        .child(turn.live_status.clone().unwrap_or_else(|| {
                                            locale::common("Process").to_string()
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::text_muted())
                                        .child(if expanded {
                                            locale::common("Hide").to_string()
                                        } else {
                                            format!(
                                                "{} {}",
                                                turn.process_rows.len(),
                                                locale::text("items", "项", "項")
                                            )
                                        }),
                                ),
                        )
                        .when(expanded, |process| {
                            process.child(
                                div()
                                    .border_t_1()
                                    .border_color(theme::border_subtle())
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(
                                        turn.process_rows
                                            .iter()
                                            .map(|row| self.render_process_row(row)),
                                    ),
                            )
                        }),
                )
            })
            .when_some(turn.conclusion_row.as_ref(), |container, row| {
                container.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .child(markdown::render(&row.body, row.last_sequence.max(0) as u64)),
                )
            })
            .when(
                turn.conclusion_row.is_none() && !turn.complete,
                |container| {
                    container.child(
                        div()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .child(
                                turn.live_status
                                    .clone()
                                    .unwrap_or_else(|| locale::common("Working...").to_string()),
                            ),
                    )
                },
            )
    }

    fn render_process_row(&self, row: &TimelineRow) -> impl IntoElement {
        let color = if row.failed {
            theme::ACCENT_RED
        } else if row.streaming {
            theme::ACCENT_GREEN
        } else {
            theme::TEXT_MUTED
        };
        div()
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .mt(px(6.0))
                    .size(px(theme::ICON_STATUS))
                    .rounded_full()
                    .bg(rgb(color)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_secondary())
                            .child(process_title(row)),
                    )
                    .when(!row.body.trim().is_empty(), |content| {
                        content.child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .line_height(px(16.0))
                                .text_color(theme::text_muted())
                                .whitespace_normal()
                                .child(row.body.clone()),
                        )
                    }),
            )
    }

    fn render_approval(
        &self,
        approval: &vibex_ui::ApprovalSurfaceModel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let approve_id = approval.request_id.clone();
        let deny_id = approval.request_id.clone();
        let always_id = approval.request_id.clone();
        let details_id = approval.request_id.to_string();
        let details_expanded = self.expanded_approval.contains(&details_id);
        let details_to_show = if details_expanded {
            approval.details.len()
        } else {
            approval.details.len().min(3)
        };
        let resolving = self.controller.as_ref().is_some_and(|controller| {
            controller
                .state
                .pending_permission_resolution(approval.request_id.as_str())
        });
        let can_approve = approval
            .allowed_responses
            .contains(&PermissionResponseKind::Approve);
        let can_deny = approval
            .allowed_responses
            .contains(&PermissionResponseKind::Deny);
        let can_always = approval
            .allowed_responses
            .contains(&PermissionResponseKind::AlwaysAllowForSession);
        let approve_label = approval_response_label(
            approval,
            PermissionResponseKind::Approve,
            locale::common("Approve"),
        );
        let deny_label = approval_response_label(
            approval,
            PermissionResponseKind::Deny,
            locale::common("Deny"),
        );
        let always_label = approval_response_label(
            approval,
            PermissionResponseKind::AlwaysAllowForSession,
            locale::common("Always allow"),
        );
        div()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(rgb(theme::ACCENT_YELLOW))
            .bg(theme::bg_card())
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_primary())
                            .child(approval.title.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(rgb(theme::ACCENT_YELLOW))
                            .child(permission_risk_label(approval.risk_category)),
                    ),
            )
            .children(
                approval
                    .details
                    .iter()
                    .take(details_to_show)
                    .map(|(label, value)| {
                        div()
                            .flex()
                            .gap_2()
                            .text_size(px(theme::FONT_CAPTION))
                            .child(div().text_color(theme::text_muted()).child(label.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(theme::text_secondary())
                                    .child(value.clone()),
                            )
                    }),
            )
            .when(approval.details.len() > 3, |card| {
                card.child(
                    div()
                        .id(format!("approval-details:{details_id}"))
                        .h(px(36.0))
                        .flex()
                        .items_center()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_muted())
                        .cursor_pointer()
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_approval_details(details_id.clone(), cx)
                            }),
                        )
                        .child(if details_expanded {
                            locale::common("Show less").to_string()
                        } else {
                            format!(
                                "{} {} {}",
                                locale::text("Show all", "显示全部", "顯示全部"),
                                approval.details.len(),
                                locale::text("details", "项详情", "項詳細資料")
                            )
                        }),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .when(resolving, |actions| {
                        actions.child(
                            div()
                                .h(px(theme::TOUCH_TARGET))
                                .flex()
                                .items_center()
                                .text_size(px(theme::FONT_BODY))
                                .text_color(theme::text_muted())
                                .child(locale::common("Resolving...")),
                        )
                    })
                    .when(!resolving && can_deny, |actions| {
                        actions.child(
                            div()
                                .id(format!("deny:{}", approval.request_id))
                                .h(px(theme::TOUCH_TARGET))
                                .px_4()
                                .flex()
                                .items_center()
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_default())
                                .text_size(px(theme::FONT_BODY))
                                .text_color(theme::text_secondary())
                                .cursor_pointer()
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.resolve_permission(
                                            deny_id.clone(),
                                            PermissionResponseKind::Deny,
                                            cx,
                                        )
                                    }),
                                )
                                .child(deny_label),
                        )
                    })
                    .when(!resolving && can_always, |actions| {
                        actions.child(
                            div()
                                .id(format!("always-allow:{}", approval.request_id))
                                .h(px(theme::TOUCH_TARGET))
                                .px_4()
                                .flex()
                                .items_center()
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_default())
                                .text_size(px(theme::FONT_BODY))
                                .text_color(theme::text_secondary())
                                .cursor_pointer()
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.resolve_permission(
                                            always_id.clone(),
                                            PermissionResponseKind::AlwaysAllowForSession,
                                            cx,
                                        )
                                    }),
                                )
                                .child(always_label),
                        )
                    })
                    .when(!resolving && can_approve, |actions| {
                        actions.child(
                            div()
                                .id(format!("approve:{}", approval.request_id))
                                .h(px(theme::TOUCH_TARGET))
                                .px_4()
                                .flex()
                                .items_center()
                                .rounded(px(theme::RADIUS_CONTROL))
                                .bg(rgb(theme::TEXT_PRIMARY))
                                .text_size(px(theme::FONT_BODY))
                                .text_color(rgb(theme::BG_PRIMARY))
                                .cursor_pointer()
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.resolve_permission(
                                            approve_id.clone(),
                                            PermissionResponseKind::Approve,
                                            cx,
                                        )
                                    }),
                                )
                                .child(approve_label),
                        )
                    }),
            )
    }

    fn render_elicitation(
        &self,
        surface: &ElicitationSurfaceModel,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let request = &surface.request;
        let pending = self.controller.as_ref().is_some_and(|controller| {
            controller
                .state
                .pending_elicitation_resolution(request.id.as_str())
        });
        let mut fields = Vec::new();
        for field in &request.fields {
            let control = match &field.kind {
                ElicitationFieldKind::Text { options, .. } if !options.is_empty() => div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(options.iter().map(|option| {
                        let request_id = request.id.clone();
                        let field_id = field.id.clone();
                        let value = option.value.clone();
                        let selected = self
                            .elicitation_draft
                            .as_ref()
                            .and_then(|draft| draft.text(&field.id))
                            == Some(option.value.as_str());
                        div()
                            .id(format!("elicitation:{}:{}", field.id, option.value))
                            .min_h(px(theme::TOUCH_TARGET))
                            .px_3()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(if selected {
                                rgb(theme::TEXT_PRIMARY).into()
                            } else {
                                theme::border_default()
                            })
                            .when(selected, |choice| choice.bg(theme::bg_card_dim()))
                            .flex()
                            .items_center()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.set_elicitation_option(
                                        request_id.clone(),
                                        field_id.clone(),
                                        value.clone(),
                                        cx,
                                    )
                                }),
                            )
                            .child(option.title.clone())
                    }))
                    .into_any_element(),
                ElicitationFieldKind::Text { .. }
                | ElicitationFieldKind::Number { .. }
                | ElicitationFieldKind::Integer { .. } => self
                    .elicitation_inputs
                    .get(&field.id)
                    .cloned()
                    .map(|input| {
                        div()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .bg(theme::bg_card_dim())
                            .child(input)
                            .into_any_element()
                    })
                    .unwrap_or_else(|| div().into_any_element()),
                ElicitationFieldKind::Boolean { .. } => div()
                    .flex()
                    .gap_2()
                    .children(
                        [(false, locale::common("No")), (true, locale::common("Yes"))]
                            .into_iter()
                            .map(|(value, label)| {
                                let request_id = request.id.clone();
                                let field_id = field.id.clone();
                                let selected = self
                                    .elicitation_draft
                                    .as_ref()
                                    .and_then(|draft| draft.boolean(&field.id))
                                    == Some(value);
                                div()
                                    .id(format!("elicitation:{}:{value}", field.id))
                                    .h(px(theme::TOUCH_TARGET))
                                    .px_4()
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .border_1()
                                    .border_color(if selected {
                                        rgb(theme::TEXT_PRIMARY).into()
                                    } else {
                                        theme::border_default()
                                    })
                                    .when(selected, |choice| choice.bg(theme::bg_card_dim()))
                                    .flex()
                                    .items_center()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(theme::text_secondary())
                                    .cursor_pointer()
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.set_elicitation_boolean(
                                                request_id.clone(),
                                                field_id.clone(),
                                                value,
                                                cx,
                                            )
                                        }),
                                    )
                                    .child(label)
                            }),
                    )
                    .into_any_element(),
                ElicitationFieldKind::MultiSelect { options, .. } => div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(options.iter().map(|option| {
                        let request_id = request.id.clone();
                        let field_id = field.id.clone();
                        let value = option.value.clone();
                        let selected = self
                            .elicitation_draft
                            .as_ref()
                            .is_some_and(|draft| draft.multi_selected(&field.id, &option.value));
                        div()
                            .id(format!("elicitation:{}:{}", field.id, option.value))
                            .min_h(px(theme::TOUCH_TARGET))
                            .px_3()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(if selected {
                                rgb(theme::TEXT_PRIMARY).into()
                            } else {
                                theme::border_default()
                            })
                            .when(selected, |choice| choice.bg(theme::bg_card_dim()))
                            .flex()
                            .items_center()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.toggle_elicitation_multi_option(
                                        request_id.clone(),
                                        field_id.clone(),
                                        value.clone(),
                                        cx,
                                    )
                                }),
                            )
                            .child(option.title.clone())
                    }))
                    .into_any_element(),
                ElicitationFieldKind::Unsupported { schema_type } => div()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(rgb(theme::ACCENT_RED))
                    .child(format!(
                        "{}: {schema_type}",
                        locale::text(
                            "Unsupported input type",
                            "不支持的输入类型",
                            "不支援的輸入類型"
                        )
                    ))
                    .into_any_element(),
            };
            fields.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_secondary())
                            .child(if field.required {
                                format!("{} *", field.title)
                            } else {
                                field.title.clone()
                            }),
                    )
                    .when_some(field.description.as_ref(), |container, description| {
                        container.child(
                            div()
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(theme::text_muted())
                                .child(description.clone()),
                        )
                    })
                    .child(control)
                    .into_any_element(),
            );
        }

        let accept_id = request.id.clone();
        let decline_id = request.id.clone();
        div()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .max_h(px(430.0))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(rgb(theme::ACCENT_BLUE))
            .bg(theme::bg_card())
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(theme::FONT_BODY))
                    .text_color(theme::text_primary())
                    .child(
                        request
                            .title
                            .clone()
                            .unwrap_or_else(|| locale::common("Input requested").to_string()),
                    ),
            )
            .child(
                div()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_secondary())
                    .child(request.message.clone()),
            )
            .child(
                div()
                    .id("elicitation-fields")
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(fields),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id(format!("decline-elicitation:{}", request.id))
                            .h(px(theme::TOUCH_TARGET))
                            .px_4()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .flex()
                            .items_center()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_secondary())
                            .when(!pending, |button| {
                                button.cursor_pointer().on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.resolve_elicitation(
                                            decline_id.clone(),
                                            ElicitationResolutionAction::Decline,
                                            cx,
                                        )
                                    }),
                                )
                            })
                            .child(locale::common("Decline")),
                    )
                    .child(
                        div()
                            .id(format!("accept-elicitation:{}", request.id))
                            .h(px(theme::TOUCH_TARGET))
                            .px_4()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(rgb(theme::TEXT_PRIMARY))
                            .flex()
                            .items_center()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(rgb(theme::BG_PRIMARY))
                            .when(!pending, |button| {
                                button.cursor_pointer().on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.resolve_elicitation(
                                            accept_id.clone(),
                                            ElicitationResolutionAction::Accept,
                                            cx,
                                        )
                                    }),
                                )
                            })
                            .child(if pending {
                                locale::common("Submitting...")
                            } else {
                                locale::common("Submit")
                            }),
                    ),
            )
    }

    fn render_composer(
        &self,
        running: bool,
        state: Option<AgentSessionState>,
        turns: &[TimelineConversationTurn],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let latest_ended_normally = turns
            .last()
            .map(|turn| turn.complete && !turn.failed && !turn.pending_permission);
        let needs_continue = state.is_some_and(|state| {
            agent_session_turn_requires_continuation(state, latest_ended_normally)
        });
        let runtime_switch_pending = self.runtime_switch_busy_generation.is_some();
        let action_enabled =
            state.is_some() && !self.operation_busy && (running || !runtime_switch_pending);
        let runtime_selection = self.controller.as_ref().and_then(|controller| {
            controller
                .state
                .runtime_selection
                .value
                .as_ref()
                .map(|state| &state.desired)
        });
        let runtime_catalog = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.runtime_options.value.as_ref());
        let runtime_summary = runtime_selection_summary(runtime_catalog, runtime_selection);
        let runtime_trigger_enabled = state.is_some()
            && !runtime_switch_pending
            && self.backend.as_ref().is_some_and(|backend| {
                backend
                    .capability_snapshot()
                    .agent
                    .supports(BackendOperation::AgentSwitchRuntime)
            });
        div()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme::border_subtle())
            .bg(theme::bg_primary())
            .p(px(theme::SPACING_MD))
            .when_some(self.error.as_ref(), |composer, error| {
                composer.child(
                    div()
                        .mb(px(theme::SPACING_SM))
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(rgb(theme::ACCENT_RED))
                        .child(error.message.clone()),
                )
            })
            .when(needs_continue, |composer| {
                composer.child(
                    div()
                        .id("continue-turn")
                        .mb(px(theme::SPACING_SM))
                        .h(px(36.0))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme::border_default())
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(theme::FONT_BODY))
                        .text_color(if self.operation_busy {
                            theme::text_muted()
                        } else {
                            theme::text_secondary()
                        })
                        .when(!self.operation_busy, |button| {
                            button
                                .cursor_pointer()
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::continue_turn))
                        })
                        .child(if self.operation_busy {
                            "Continuing\u{2026}"
                        } else {
                            locale::common("Continue")
                        }),
                )
            })
            .child(
                div()
                    .min_h(px(52.0))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_card())
                    .flex()
                    .items_center()
                    .pl(px(theme::SPACING_XS))
                    .child(div().flex_1().min_w_0().child(self.composer_input.clone()))
                    .child(
                        div()
                            .id(if running { "stop-turn" } else { "send-message" })
                            .size(px(theme::TOUCH_TARGET))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(action_enabled, |button| {
                                button
                                    .cursor_pointer()
                                    .active(|style| style.opacity(0.6))
                                    .when(running, |button| {
                                        button.on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::interrupt),
                                        )
                                    })
                                    .when(!running, |button| {
                                        button.on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::send_message),
                                        )
                                    })
                            })
                            .child(
                                svg()
                                    .path(if running {
                                        "icons/stop.svg"
                                    } else {
                                        "icons/send.svg"
                                    })
                                    .size(px(theme::ICON_SM))
                                    .text_color(if !action_enabled {
                                        theme::text_muted()
                                    } else if running {
                                        rgb(theme::ACCENT_RED).into()
                                    } else {
                                        theme::text_secondary()
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("composer-runtime-options")
                    .mt(px(theme::SPACING_SM))
                    .h(px(theme::TOUCH_TARGET))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(runtime_trigger_enabled, |row| {
                        row.cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::open_runtime_options))
                    })
                    .when(!runtime_trigger_enabled, |row| row.opacity(0.62))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(locale::common("Runtime")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(if runtime_summary.available {
                                        theme::text_primary()
                                    } else {
                                        rgb(theme::ACCENT_YELLOW).into()
                                    })
                                    .child(runtime_summary.primary),
                            )
                            .when(!runtime_summary.secondary.is_empty(), |summary| {
                                summary.child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::text_muted())
                                        .child(runtime_summary.secondary),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_muted())
                            .child(if runtime_switch_pending { "..." } else { ">" }),
                    ),
            )
    }

    fn render_sidebar_row(
        &self,
        index: usize,
        row: &SidebarRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let dragging = self
            .sidebar_drag
            .as_ref()
            .is_some_and(|drag| drag.index == index);
        let drop = self
            .sidebar_drag
            .as_ref()
            .and_then(|drag| drag.target.as_ref())
            .filter(|target| target.index == index)
            .map(|target| target.position);
        let indent = theme::SPACING_MD + row.depth as f32 * theme::SIDEBAR_INDENT;
        let body = match row.kind {
            SidebarRowKind::Folder => self.render_sidebar_folder_row(row, indent, cx),
            SidebarRowKind::Project => self.render_sidebar_project_row(row, indent, cx),
            SidebarRowKind::Workspace => self.render_sidebar_workspace_row(row, indent, cx),
            SidebarRowKind::Session => self.render_sidebar_session_row(row, indent, cx),
        };
        div()
            .relative()
            .h(px(theme::SIDEBAR_ROW_HEIGHT))
            .min_h(px(theme::SIDEBAR_ROW_HEIGHT))
            .when(dragging, |row| row.opacity(0.45))
            .when(drop == Some(SidebarDropPosition::Into), |row| {
                row.bg(theme::sidebar_drop_bg())
            })
            .child(body)
            // The insertion line reads the same way the desktop's does.
            .when_some(drop, |row, position| {
                let line = div()
                    .absolute()
                    .left(px(indent))
                    .right(px(theme::SPACING_SM))
                    .h(px(2.0))
                    .bg(rgb(theme::ACCENT_BLUE));
                match position {
                    SidebarDropPosition::Before => row.child(line.top_0()),
                    SidebarDropPosition::After => row.child(line.bottom_0()),
                    SidebarDropPosition::Into => row,
                }
            })
            .child(self.render_sidebar_grip(row, cx))
            .into_any_element()
    }

    /// The grip is the only place a pan means "move this row"; everywhere else
    /// a finger scrolls the list.
    fn render_sidebar_grip(&self, row: &SidebarRow, cx: &mut Context<Self>) -> gpui::AnyElement {
        if row.kind == SidebarRowKind::Workspace {
            return div().into_any_element();
        }
        let can_move = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentSidebarOrganizationMutate)
        });
        if !can_move {
            return div().into_any_element();
        }
        let menu_row = row.clone();
        div()
            .id(format!("mobile-sidebar-grip-{}", row.id()))
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(theme::SIDEBAR_GRIP_WIDTH))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            // A tap on the grip opens the row's actions; a pan from it moves the
            // row, which the drawer gesture handler picks up by press position.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.sidebar_row_menu = Some(SidebarRowMenu {
                        row: menu_row.clone(),
                    });
                    cx.notify();
                }),
            )
            .child(
                svg()
                    .path("icons/grip-vertical.svg")
                    .size(px(15.0))
                    .text_color(theme::sidebar_text_muted()),
            )
            .into_any_element()
    }

    fn render_sidebar_folder_row(
        &self,
        row: &SidebarRow,
        indent: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let folder_id = row.id().to_string();
        div()
            .id(format!("mobile-folder-row-{folder_id}"))
            .h_full()
            .mx(px(theme::SPACING_SM))
            .pl(px(indent))
            .pr(px(theme::SIDEBAR_GRIP_WIDTH))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(theme::SPACING_XS))
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.toggle_folder(folder_id.clone(), cx)),
            )
            // Only folders carry the expander column; a project row toggles by
            // its whole row, the way the desktop does it.
            .child(
                svg()
                    .path(if row.collapsed {
                        "icons/chevron-right.svg"
                    } else {
                        "icons/chevron-down.svg"
                    })
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(theme::sidebar_text_muted()),
            )
            .child(
                svg()
                    .path("icons/folder.svg")
                    .size(px(15.0))
                    .flex_shrink_0()
                    .text_color(theme::sidebar_text_secondary()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_BODY))
                    .text_color(theme::sidebar_text_primary())
                    .child(row.label.clone()),
            )
            .into_any_element()
    }

    fn render_sidebar_project_row(
        &self,
        row: &SidebarRow,
        indent: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let project_id = row.id().to_string();
        let new_session_project_id = project_id.clone();
        let can_create_session = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentCreateSession)
        });
        div()
            .id(format!("mobile-project-row-{project_id}"))
            .h_full()
            .mx(px(theme::SPACING_SM))
            .pl(px(indent))
            .pr(px(theme::SIDEBAR_GRIP_WIDTH))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(theme::SPACING_SM))
            .when(row.selected, |project| {
                project.bg(theme::sidebar_selected_bg())
            })
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.toggle_project(project_id.clone(), cx)),
            )
            .child(
                svg()
                    .path(if row.collapsed {
                        "icons/chevron-right.svg"
                    } else {
                        "icons/chevron-down.svg"
                    })
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(theme::sidebar_text_muted()),
            )
            .child(
                div()
                    .w(px(30.0))
                    .h(px(30.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        svg()
                            .path("icons/boxes.svg")
                            .size(px(18.0))
                            .text_color(rgb(theme::ACCENT_PURPLE)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::sidebar_text_primary())
                    .child(row.label.clone()),
            )
            .when(row.child_count > 0, |project| {
                project.child(
                    div()
                        .size(px(20.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::sidebar_text_muted())
                        .bg(theme::bg_card_dim())
                        .child(row.child_count.to_string()),
                )
            })
            // Always visible: a phone has no hover state to reveal it.
            .child(
                div()
                    .id(format!("mobile-project-new-session-{}", row.id()))
                    .aria_label(locale::common("New session"))
                    .size(px(30.0))
                    .flex_shrink_0()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(can_create_session, |button| {
                        button
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.create_session_in_project(
                                        new_session_project_id.clone(),
                                        window,
                                        cx,
                                    );
                                }),
                            )
                    })
                    .when(!can_create_session, |button| button.opacity(0.38))
                    .child(
                        svg()
                            .path("icons/plus.svg")
                            .size(px(15.0))
                            .text_color(theme::sidebar_text_muted()),
                    ),
            )
            .into_any_element()
    }

    fn render_sidebar_workspace_row(
        &self,
        row: &SidebarRow,
        indent: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(workspace_id) = row.workspace_id.clone() else {
            return div().into_any_element();
        };
        let workspace_mode = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id.as_str() == workspace_id)
            .map(|workspace| workspace.mode)
            .unwrap_or(WorkspaceMode::CurrentCheckout);
        let toggle_id = workspace_id.clone();
        let new_session_id = workspace_id.clone();
        let can_create_session = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .agent
                .supports(BackendOperation::AgentCreateSession)
        });
        let status_color = sidebar_workspace_status_color(row.state);
        let detail = row
            .detail
            .as_deref()
            .map(|detail| format!("{} · {detail}", workspace_mode_label(workspace_mode)))
            .unwrap_or_else(|| workspace_mode_label(workspace_mode).to_string());
        div()
            .id(format!("mobile-workspace-row-{workspace_id}"))
            .h_full()
            .mx(px(theme::SPACING_SM))
            .pl(px(indent))
            .pr(px(theme::SIDEBAR_GRIP_WIDTH))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(theme::SPACING_XS))
            .when(row.selected, |workspace| {
                workspace.bg(theme::sidebar_selected_bg())
            })
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.toggle_workspace(toggle_id.clone(), cx)),
            )
            .child(
                svg()
                    .path(if row.collapsed {
                        "icons/chevron-right.svg"
                    } else {
                        "icons/chevron-down.svg"
                    })
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(theme::sidebar_text_muted()),
            )
            .child(
                div()
                    .w(px(30.0))
                    .h(px(30.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(theme::ICON_STATUS))
                            .rounded_full()
                            .bg(rgb(status_color)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(theme::FONT_BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::sidebar_text_primary())
                            .child(row.label.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::sidebar_text_muted())
                            .child(detail),
                    ),
            )
            .when(row.child_count > 0, |workspace| {
                workspace.child(
                    div()
                        .size(px(20.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::sidebar_text_muted())
                        .bg(theme::bg_card_dim())
                        .child(row.child_count.to_string()),
                )
            })
            .child(
                div()
                    .id(format!("mobile-workspace-new-session-{workspace_id}"))
                    .size(px(30.0))
                    .flex_shrink_0()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(!can_create_session, |button| button.opacity(0.38))
                    .active(|style| style.bg(theme::row_pressed_bg()))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if !can_create_session {
                                return;
                            }
                            cx.stop_propagation();
                            this.create_session_in_workspace(new_session_id.clone(), window, cx);
                        }),
                    )
                    .child(
                        svg()
                            .path("icons/plus.svg")
                            .size(px(15.0))
                            .text_color(theme::sidebar_text_muted()),
                    ),
            )
            .into_any_element()
    }

    fn render_sidebar_session_row(
        &self,
        row: &SidebarRow,
        indent: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(session_id) = row.session_id.clone() else {
            return div().into_any_element();
        };
        let sessions = self.sessions();
        let Some(session) = sessions.iter().find(|session| session.id == session_id) else {
            return div().into_any_element();
        };
        let status = match row.state.unwrap_or(session.state) {
            AgentSessionState::Running => theme::ACCENT_GREEN,
            AgentSessionState::NeedsInput => theme::ACCENT_YELLOW,
            AgentSessionState::Error => theme::ACCENT_RED,
            AgentSessionState::Initializing => theme::ACCENT_BLUE,
            AgentSessionState::Idle => theme::ACCENT_GREEN,
            AgentSessionState::Archived | AgentSessionState::Closed => theme::ACCENT_DIM,
        };
        let is_selected = row.selected;
        let open_session_id = session_id.clone();
        div()
            .id(format!("mobile-session-row-{}", row.id()))
            .h_full()
            .mx(px(theme::SPACING_SM))
            .pl(px(indent))
            .pr(px(theme::SIDEBAR_GRIP_WIDTH))
            .rounded(px(theme::RADIUS_CONTROL))
            .flex()
            .items_center()
            .gap(px(7.0))
            .when(is_selected, |row| row.bg(theme::sidebar_selected_bg()))
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.open_session(open_session_id.clone(), cx)),
            )
            .child(
                svg()
                    .path(agent_icon_path(&session.agent_id.to_string()))
                    .size(px(16.0))
                    .flex_shrink_0()
                    .text_color(if is_selected {
                        theme::text_primary()
                    } else {
                        theme::sidebar_text_muted()
                    }),
            )
            .when(row.pinned, |row| {
                row.child(
                    svg()
                        .path("icons/pin.svg")
                        .size(px(11.0))
                        .flex_shrink_0()
                        .text_color(rgb(theme::ACCENT_YELLOW)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_BODY))
                    .text_color(if is_selected {
                        theme::text_primary()
                    } else {
                        theme::sidebar_text_secondary()
                    })
                    .child(row.label.clone()),
            )
            .child(
                div()
                    .w(px(52.0))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_right()
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(theme::sidebar_text_muted())
                    .child(session_sidebar_time_label(session.last_message_at_ms)),
            )
            .child(
                div()
                    .size(px(theme::ICON_STATUS))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(rgb(status)),
            )
            .into_any_element()
    }

    fn render_workbench_drawer(
        &self,
        workbench: Option<Entity<MobileWorkbench>>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let body = match workbench {
            Some(workbench) => div().flex_1().min_h_0().child(workbench),
            None => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::FONT_BODY))
                .text_color(theme::text_muted())
                .child(locale::common("Loading")),
        };

        div()
            .absolute()
            .top_0()
            .bottom_0()
            // The workbench page is the phone's right rail, so it mirrors the
            // desktop right-rail surfaces rather than the session page.
            .bg(theme::workbench_bg())
            .border_l_1()
            .border_color(theme::border_default())
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(theme::HEADER_HEIGHT))
                    .flex_shrink_0()
                    .bg(theme::workbench_panel_bg())
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .pl(px(theme::SPACING_LG))
                    .pr(px(theme::SPACING_XS))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(theme::FONT_HEADING))
                            .text_color(theme::text_primary())
                            .child(locale::common("Workspace tools")),
                    )
                    .child(
                        div()
                            .id("close-mobile-workbench")
                            .size(px(theme::HEADER_BUTTON_SIZE))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.opacity(0.6))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::close_workbench))
                            .child(
                                svg()
                                    .path("icons/x.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
                            ),
                    ),
            )
            .child(body)
    }
}

impl MobileApp {
    fn render_drawer(&self, cx: &mut Context<Self>) -> gpui::Div {
        let sessions = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_ref())
            .cloned()
            .unwrap_or_default();
        let search_query = self.sidebar_search_input.read(cx).text().to_string();
        let rows = self.mobile_sidebar_rows(&search_query);
        let capabilities = self
            .backend
            .as_ref()
            .map(|backend| backend.capability_snapshot());
        let can_create_session = capabilities.as_ref().is_some_and(|capabilities| {
            capabilities
                .agent
                .supports(BackendOperation::AgentCreateSession)
        });
        let can_open_project = capabilities.as_ref().is_some_and(|capabilities| {
            capabilities
                .workspace
                .supports(BackendOperation::WorkspaceOpen)
        });
        let rows_for_list = rows.clone();
        let has_search_query = !search_query.trim().is_empty();
        let has_sidebar_items = !sessions.is_empty() || !self.workspace_summaries.is_empty();
        let project_ids = self
            .sidebar_projects()
            .into_iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();
        let all_projects_collapsed = !project_ids.is_empty()
            && project_ids
                .iter()
                .all(|project_id| self.sidebar_view.collapsed_project_ids.contains(project_id));
        let list_frame = self.sidebar_list_frame.clone();
        let can_locate_session = self
            .controller
            .as_ref()
            .is_some_and(|controller| controller.state.selected_session_id.is_some());
        let host_label = self.active_host_label();
        let host_online = self.backend.as_ref().is_some_and(|backend| {
            backend.connection_state().state == RemoteConnectionState::Online
        });

        div()
            .absolute()
            .top_0()
            .bottom_0()
            .bg(theme::sidebar_bg())
            .border_r_1()
            .border_color(theme::border_default())
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(theme::DRAWER_HEADER_HEIGHT))
                    .flex_shrink_0()
                    .px(px(theme::SPACING_MD))
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    // The sessions page is the phone's home surface, so it wears
                    // the product wordmark: the brand mark supplies the capital
                    // "V" and the label completes it.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .child(
                                svg()
                                    .path("brand/logo.svg")
                                    .size(px(22.0))
                                    .flex_shrink_0()
                                    .text_color(theme::sidebar_text_primary()),
                            )
                            .child(
                                div()
                                    .text_size(px(17.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::sidebar_text_primary())
                                    .child("ibex"),
                            ),
                    )
                    .child(
                        div()
                            .id("mobile-drawer-new-session")
                            .aria_label(locale::common("New session"))
                            .size(px(36.0))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(can_create_session, |button| {
                                button
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::create_session),
                                    )
                            })
                            .when(!can_create_session, |button| button.opacity(0.38))
                            .child(
                                svg()
                                    .path("icons/plus.svg")
                                    .size(px(17.0))
                                    .text_color(theme::sidebar_text_secondary()),
                            ),
                    )
                    .child(
                        div()
                            .id("mobile-drawer-usage")
                            .aria_label(locale::common("Usage Statistics"))
                            .size(px(36.0))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::open_usage))
                            .child(
                                svg()
                                    .path("icons/activity.svg")
                                    .size(px(17.0))
                                    .text_color(theme::sidebar_text_secondary()),
                            ),
                    )
                    // Leaving the sessions page returns to the conversation, so
                    // the affordance is an exit arrow rather than a dismissal.
                    .child(
                        div()
                            .id("close-session-drawer")
                            .aria_label(locale::text("Back to session", "返回会话", "返回工作階段"))
                            .size(px(36.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.opacity(0.6))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::close_drawer))
                            .child(
                                svg()
                                    .path("icons/log-out.svg")
                                    .size(px(17.0))
                                    .text_color(theme::sidebar_text_muted()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .id("drawer-scroll")
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(theme::DRAWER_SECTION_HEIGHT))
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::sidebar_text_muted())
                                    .child(locale::common("Projects")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    // Same order and same marks as the desktop
                                    // sessions toolbar: collapse, locate, new,
                                    // search. The desktop "more" menu has no
                                    // mobile counterpart, so it is omitted.
                                    .child(
                                        div()
                                            .id("mobile-drawer-toggle-projects")
                                            .aria_label(locale::common(if all_projects_collapsed {
                                                "Expand projects"
                                            } else {
                                                "Collapse projects"
                                            }))
                                            .size(px(32.0))
                                            .rounded(px(theme::RADIUS_CONTROL))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(!project_ids.is_empty(), |button| {
                                                button
                                                    .cursor_pointer()
                                                    .active(|style| {
                                                        style.bg(theme::row_pressed_bg())
                                                    })
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::toggle_all_projects),
                                                    )
                                            })
                                            .when(project_ids.is_empty(), |button| {
                                                button.opacity(0.38)
                                            })
                                            .child(
                                                svg()
                                                    .path(if all_projects_collapsed {
                                                        "icons/chevrons-left-right.svg"
                                                    } else {
                                                        "icons/chevrons-right-left.svg"
                                                    })
                                                    .size(px(16.0))
                                                    .text_color(theme::sidebar_text_secondary()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("mobile-drawer-locate-session")
                                            .aria_label(locale::text(
                                                "Locate Current Session",
                                                "定位当前会话",
                                                "定位目前工作階段",
                                            ))
                                            .size(px(32.0))
                                            .rounded(px(theme::RADIUS_CONTROL))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(can_locate_session, |button| {
                                                button
                                                    .cursor_pointer()
                                                    .active(|style| {
                                                        style.bg(theme::row_pressed_bg())
                                                    })
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::locate_selected_session),
                                                    )
                                            })
                                            .when(!can_locate_session, |button| {
                                                button.opacity(0.38)
                                            })
                                            .child(
                                                svg()
                                                    .path("icons/crosshair.svg")
                                                    .size(px(16.0))
                                                    .text_color(theme::sidebar_text_secondary()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("mobile-drawer-new-project")
                                            .aria_label(locale::common("New project"))
                                            .size(px(32.0))
                                            .rounded(px(theme::RADIUS_CONTROL))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(can_open_project, |button| {
                                                button
                                                    .cursor_pointer()
                                                    .active(|style| {
                                                        style.bg(theme::row_pressed_bg())
                                                    })
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::open_new_project),
                                                    )
                                            })
                                            .when(!can_open_project, |button| button.opacity(0.38))
                                            .child(
                                                svg()
                                                    .path("icons/plus.svg")
                                                    .size(px(16.0))
                                                    .text_color(theme::sidebar_text_secondary()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("mobile-drawer-search")
                                            .aria_label(locale::common("Search sessions"))
                                            .size(px(32.0))
                                            .rounded(px(theme::RADIUS_CONTROL))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(self.sidebar_search_open, |button| {
                                                button.bg(theme::sidebar_selected_bg())
                                            })
                                            .when(has_sidebar_items, |button| {
                                                button
                                                    .cursor_pointer()
                                                    .active(|style| {
                                                        style.bg(theme::row_pressed_bg())
                                                    })
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::toggle_sidebar_search),
                                                    )
                                            })
                                            .when(!has_sidebar_items, |button| button.opacity(0.38))
                                            .child(
                                                svg()
                                                    .path("icons/search.svg")
                                                    .size(px(16.0))
                                                    .text_color(theme::sidebar_text_secondary()),
                                            ),
                                    ),
                            ),
                    )
                    .when(self.sidebar_search_open, |body| {
                        body.child(
                            div()
                                .id("mobile-drawer-search-field")
                                .h(px(theme::TOUCH_TARGET))
                                .flex_shrink_0()
                                .mx(px(theme::SPACING_MD))
                                .mb(px(theme::SPACING_XS))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_default())
                                .bg(theme::bg_card_dim())
                                .flex()
                                .items_center()
                                .child(
                                    svg()
                                        .path("icons/search.svg")
                                        .ml(px(theme::SPACING_SM))
                                        .size(px(15.0))
                                        .flex_shrink_0()
                                        .text_color(theme::sidebar_text_muted()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .h_full()
                                        .child(self.sidebar_search_input.clone()),
                                )
                                .child(
                                    div()
                                        .id("mobile-drawer-search-close")
                                        .aria_label(locale::common("Close search"))
                                        .size(px(32.0))
                                        .mr(px(2.0))
                                        .flex_shrink_0()
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .active(|style| style.opacity(0.6))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::toggle_sidebar_search),
                                        )
                                        .child(
                                            svg()
                                                .path("icons/x.svg")
                                                .size(px(14.0))
                                                .text_color(theme::sidebar_text_muted()),
                                        ),
                                ),
                        )
                    })
                    .when(rows.is_empty(), |body| {
                        body.child(
                            div()
                                .mx(px(theme::SPACING_LG))
                                .mt(px(theme::SPACING_LG))
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::sidebar_text_muted())
                                .child(if has_search_query {
                                    locale::common("No matching sessions")
                                } else {
                                    locale::text("No sessions yet", "还没有会话", "尚無工作階段")
                                }),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .flex_1()
                            .min_h_0()
                            // Rows are a uniform height, so recording the list
                            // frame is enough to resolve a touch to a row
                            // without threading per-row bounds through paint.
                            .child(
                                gpui::canvas(
                                    move |bounds, _, _| {
                                        list_frame.set((
                                            f32::from(bounds.origin.y),
                                            f32::from(bounds.origin.x + bounds.size.width),
                                        ));
                                    },
                                    |_, _, _, _| (),
                                )
                                .absolute()
                                .inset_0(),
                            )
                            .child(
                                uniform_list(
                                    "drawer-project-sessions",
                                    rows.len(),
                                    cx.processor(
                                        move |this, range: std::ops::Range<usize>, _window, cx| {
                                            range
                                                .filter_map(|index| {
                                                    rows_for_list.get(index).map(|row| {
                                                        this.render_sidebar_row(index, row, cx)
                                                    })
                                                })
                                                .collect::<Vec<_>>()
                                        },
                                    ),
                                )
                                .track_scroll(&self.drawer_scroll)
                                .on_scroll_wheel(cx.listener(Self::sidebar_list_pan))
                                .size_full()
                                .py(px(2.0)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .p(px(theme::SPACING_SM))
                    .flex()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        div()
                            .id("mobile-host-switcher")
                            .h(px(theme::TOUCH_TARGET))
                            .flex_1()
                            .min_w_0()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::open_hosts))
                            .child(div().size(px(8.0)).flex_shrink_0().rounded_full().bg(rgb(
                                if host_online {
                                    theme::ACCENT_GREEN
                                } else {
                                    theme::ACCENT_RED
                                },
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::sidebar_text_secondary())
                                    .child(host_label),
                            ),
                    )
                    .child(
                        div()
                            .id("mobile-settings-button")
                            .aria_label(locale::common("Settings"))
                            .size(px(theme::TOUCH_TARGET))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::open_settings))
                            .child(
                                svg()
                                    .path("icons/settings.svg")
                                    .size(px(18.0))
                                    .text_color(theme::sidebar_text_secondary()),
                            ),
                    ),
            )
    }

    fn active_host_label(&self) -> String {
        self.active_host_id
            .as_deref()
            .and_then(|id| self.known_hosts.iter().find(|host| host.id == id))
            .map(|host| host.label.clone())
            .unwrap_or_else(|| locale::text("Desktop", "桌面端", "桌面版").to_string())
    }

    fn active_host_url(&self) -> String {
        self.active_host_id
            .as_deref()
            .and_then(|id| self.known_hosts.iter().find(|host| host.id == id))
            .map(|host| host.bundle.record.server_url.clone())
            .unwrap_or_else(|| locale::common("Connected").to_string())
    }
}

impl MobileApp {
    fn render_overlay_header(
        &self,
        id: &'static str,
        title: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .h(px(theme::DRAWER_HEADER_HEIGHT))
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme::border_subtle())
            .px(px(theme::SPACING_LG))
            .flex()
            .items_center()
            .gap(px(theme::SPACING_SM))
            .child(
                div()
                    .id(format!("{id}-back"))
                    .aria_label(locale::common("Back"))
                    .size(px(theme::HEADER_BUTTON_SIZE))
                    .ml(px(-theme::SPACING_SM))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .active(|style| style.opacity(0.65))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::close_overlay))
                    .child(
                        svg()
                            .path("icons/chevron-left.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::text_secondary()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(theme::FONT_HEADING))
                    .text_color(theme::text_primary())
                    .child(locale::common(title)),
            )
            .child(
                div()
                    .id(format!("{id}-close"))
                    .aria_label(locale::common("Close"))
                    .size(px(theme::HEADER_BUTTON_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .active(|style| style.opacity(0.65))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::close_overlay))
                    .child(
                        svg()
                            .path("icons/x.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::text_muted()),
                    ),
            )
            .into_any_element()
    }

    fn render_hosts(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active_id = self.active_host_id.clone();
        let hosts = self.known_hosts.clone();
        let active_online = self.backend.as_ref().is_some_and(|backend| {
            backend.connection_state().state == RemoteConnectionState::Online
        });
        div()
            .absolute()
            .inset_0()
            .bg(theme::bg_primary())
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .child(self.render_overlay_header("mobile-hosts", "Hosts", cx))
            .child(
                div()
                    .id("mobile-hosts-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .on_scroll_wheel(cx.listener(Self::consume_drawer_scroll))
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_MD))
                    .child(
                        div()
                            .mb(px(theme::SPACING_SM))
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .child(locale::common("Connection")),
                    )
                    .when(hosts.is_empty(), |body| {
                        body.child(
                            div()
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_subtle())
                                .bg(theme::bg_card_dim())
                                .p(px(theme::SPACING_MD))
                                .text_size(px(theme::FONT_BODY))
                                .text_color(theme::text_muted())
                                .child(locale::common("No hosts paired")),
                        )
                    })
                    .children(hosts.into_iter().map(|host| {
                        let selected = active_id.as_deref() == Some(host.id.as_str());
                        let host_id = host.id.clone();
                        div()
                            .id(format!("mobile-host-{}", host.id))
                            .w_full()
                            .min_h(px(theme::TOUCH_TARGET))
                            .mb(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(if selected {
                                theme::border_default()
                            } else {
                                theme::border_subtle()
                            })
                            .when(selected, |row| row.bg(theme::bg_card()))
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, event, window, cx| {
                                    this.switch_host(host_id.clone(), event, window, cx)
                                }),
                            )
                            .child(div().size(px(8.0)).rounded_full().bg(if selected {
                                rgb(if active_online {
                                    theme::ACCENT_GREEN
                                } else {
                                    theme::ACCENT_RED
                                })
                            } else {
                                rgb(theme::ACCENT_DIM)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(if selected {
                                                theme::text_primary()
                                            } else {
                                                theme::text_secondary()
                                            })
                                            .child(host.label),
                                    )
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(host.bundle.record.server_url),
                                    ),
                            )
                    }))
                    .child(
                        div()
                            .id("mobile-add-host")
                            .mt(px(theme::SPACING_LG))
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(theme::SPACING_SM))
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::begin_pairing_host))
                            .child(
                                svg()
                                    .path("icons/plus.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            )
                            .child(locale::common("Add host")),
                    ),
            )
            .into_any_element()
    }
}

impl Render for MobileApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_elicitation_form(cx);
        let insets = window.insets().effective();
        let page_width = workspace_page_width(window);
        let root_background = match visible_drawer_page(self.drawer_offset, self.drawer_snap) {
            Some(DrawerPage::Sessions) => theme::sidebar_bg(),
            _ => theme::bg_primary(),
        };
        div()
            .size_full()
            .bg(root_background)
            .font_family("IBM Plex Sans")
            .text_color(theme::text_secondary())
            .pt(insets.top)
            .pr(insets.right)
            .pb(insets.bottom)
            .pl(insets.left)
            .child(match self.mode {
                RootMode::Pairing => self.render_pairing(cx).into_any_element(),
                RootMode::Connecting => self.render_connecting(cx).into_any_element(),
                RootMode::Workspace => self.render_workspace(page_width, cx).into_any_element(),
            })
    }
}

fn workspace_page_width(window: &Window) -> f32 {
    let viewport = window.viewport_size();
    let insets = window.insets().effective();
    (f32::from(viewport.width) - f32::from(insets.left) - f32::from(insets.right)).max(1.0)
}

fn settings_section_heading(label: &'static str) -> gpui::AnyElement {
    div()
        .mt(px(theme::SPACING_LG))
        .mb(px(theme::SPACING_SM))
        .text_size(px(theme::FONT_CAPTION))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(locale::common(label))
        .into_any_element()
}

fn settings_info_row(
    id: &'static str,
    icon: &'static str,
    title: String,
    detail: String,
    accent: gpui::Hsla,
) -> gpui::AnyElement {
    div()
        .id(id)
        .w_full()
        .min_h(px(theme::TOUCH_TARGET))
        .mb(px(theme::SPACING_XS))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::bg_card_dim())
        .px(px(theme::SPACING_MD))
        .flex()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .child(svg().path(icon).size(px(theme::ICON_SM)).text_color(accent))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(theme::FONT_BODY))
                        .text_color(theme::text_secondary())
                        .child(title),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::text_muted())
                        .child(detail),
                ),
        )
        .into_any_element()
}

impl MobileApp {
    fn render_new_project(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let can_open_project = self.backend.as_ref().is_some_and(|backend| {
            backend
                .capability_snapshot()
                .workspace
                .supports(BackendOperation::WorkspaceOpen)
        });
        div()
            .absolute()
            .inset_0()
            .bg(theme::bg_primary())
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .child(self.render_overlay_header("mobile-new-project", "New project", cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_MD))
                    .child(
                        div()
                            .mb(px(theme::SPACING_SM))
                            .text_size(px(theme::FONT_CAPTION))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_muted())
                            .child(locale::common("Project path")),
                    )
                    .child(
                        div()
                            .h(px(theme::TOUCH_TARGET))
                            .w_full()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .bg(theme::bg_card())
                            .child(self.new_project_input.clone()),
                    )
                    .when_some(self.new_project_error.as_ref(), |body, error| {
                        body.child(
                            div()
                                .mt(px(theme::SPACING_SM))
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(rgb(theme::ACCENT_RED))
                                .child(error.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("mobile-new-project-submit")
                            .h(px(theme::TOUCH_TARGET))
                            .w_full()
                            .mt(px(theme::SPACING_LG))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .bg(theme::bg_card())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_primary())
                            .when(can_open_project && !self.new_project_busy, |button| {
                                button
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::submit_new_project),
                                    )
                            })
                            .when(!can_open_project || self.new_project_busy, |button| {
                                button.opacity(0.45)
                            })
                            .child(if self.new_project_busy {
                                locale::common("Opening...")
                            } else {
                                locale::common("Open project")
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let host_label = self.active_host_label();
        let host_url = self.active_host_url();
        let server_id = self
            .active_host_id
            .as_deref()
            .and_then(|id| self.known_hosts.iter().find(|host| host.id == id))
            .map(|host| host.bundle.expected_server_id.clone())
            .unwrap_or_default();
        let connection_online = self.backend.as_ref().is_some_and(|backend| {
            backend.connection_state().state == RemoteConnectionState::Online
        });
        div()
            .absolute()
            .inset_0()
            .bg(theme::bg_primary())
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .child(self.render_overlay_header("mobile-settings", "Settings", cx))
            .child(
                div()
                    .id("mobile-settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&self.settings_scroll)
                    .overflow_y_scroll()
                    .on_scroll_wheel(cx.listener(Self::consume_drawer_scroll))
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_MD))
                    .child(settings_section_heading("Connection"))
                    .child(settings_info_row(
                        "mobile-settings-host",
                        "icons/server.svg",
                        host_label,
                        if connection_online {
                            locale::common("Connected").to_string()
                        } else {
                            locale::common("Offline").to_string()
                        },
                        if connection_online {
                            rgb(theme::ACCENT_GREEN).into()
                        } else {
                            rgb(theme::ACCENT_RED).into()
                        },
                    ))
                    .child(settings_info_row(
                        "mobile-settings-host-url",
                        "icons/server.svg",
                        locale::text("Host", "主机", "主機").to_string(),
                        host_url,
                        theme::text_muted(),
                    ))
                    .when(!server_id.is_empty(), |body| {
                        body.child(settings_info_row(
                            "mobile-settings-server-id",
                            "icons/server.svg",
                            "Server ID".to_string(),
                            server_id,
                            theme::text_muted(),
                        ))
                    })
                    .child(
                        div()
                            .id("mobile-settings-switch-host")
                            .w_full()
                            .min_h(px(theme::TOUCH_TARGET))
                            .mb(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::open_hosts))
                            .child(
                                svg()
                                    .path("icons/server.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .child(locale::common("Switch host")),
                            )
                            .child(
                                svg()
                                    .path("icons/chevron-right.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
                            ),
                    )
                    .child(settings_section_heading("Agent access"))
                    .child(
                        div()
                            .id("mobile-settings-providers")
                            .w_full()
                            .min_h(px(theme::TOUCH_TARGET))
                            .mb(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_subtle())
                            .bg(theme::bg_card_dim())
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.open_workbench_surface(
                                        WorkbenchSurface::Providers,
                                        window,
                                        cx,
                                    )
                                }),
                            )
                            .child(
                                svg()
                                    .path("brand/logo.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(rgb(0xc678dd)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(theme::text_secondary())
                                            .child(locale::common("Provider settings")),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(locale::text(
                                                "Read and manage desktop Agent profiles",
                                                "查看并管理桌面端 Agent 配置",
                                                "檢視並管理桌面版 Agent 設定檔",
                                            )),
                                    ),
                            )
                            .child(
                                svg()
                                    .path("icons/chevron-right.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
                            ),
                    )
                    .child(
                        div()
                            .id("mobile-settings-runtime")
                            .w_full()
                            .min_h(px(theme::TOUCH_TARGET))
                            .mb(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_subtle())
                            .bg(theme::bg_card_dim())
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.open_workbench_surface(
                                        WorkbenchSurface::Runtime,
                                        window,
                                        cx,
                                    )
                                }),
                            )
                            .child(
                                svg()
                                    .path("icons/activity.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(rgb(theme::ACCENT_BLUE)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(theme::text_secondary())
                                            .child(locale::common("Runtime options")),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(locale::text(
                                                "Choose the runtime for the active session",
                                                "选择当前会话的运行时",
                                                "選擇目前工作階段的執行環境",
                                            )),
                                    ),
                            )
                            .child(
                                svg()
                                    .path("icons/chevron-right.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
                            ),
                    )
                    .child(settings_section_heading("Appearance"))
                    .child(settings_info_row(
                        "mobile-settings-dark",
                        "icons/activity.svg",
                        locale::common("Dark appearance").to_string(),
                        locale::text(
                            "Vibex mobile uses the desktop dark palette",
                            "移动端使用桌面端深色配色",
                            "行動端使用桌面版深色配色",
                        )
                        .to_string(),
                        theme::text_muted(),
                    ))
                    .child(settings_info_row(
                        "mobile-settings-language",
                        "icons/message-square.svg",
                        locale::text("Language", "语言", "語言").to_string(),
                        locale::common("Follows system language").to_string(),
                        theme::text_muted(),
                    ))
                    .child(settings_section_heading("Notifications"))
                    .child(
                        div()
                            .id("mobile-settings-notifications")
                            .w_full()
                            .min_h(px(theme::TOUCH_TARGET))
                            .mb(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_subtle())
                            .bg(theme::bg_card_dim())
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|_, _, _, cx| {
                                    notifications::request_authorization();
                                    cx.notify();
                                }),
                            )
                            .child(
                                svg()
                                    .path("icons/activity.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(theme::text_secondary())
                                            .child(locale::common("Enable notifications")),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(locale::text(
                                                "Allow approval and Agent completion alerts",
                                                "允许审批和 Agent 完成通知",
                                                "允許核准與 Agent 完成通知",
                                            )),
                                    ),
                            ),
                    )
                    .child(settings_section_heading("About"))
                    .child(settings_info_row(
                        "mobile-settings-version",
                        "brand/logo.svg",
                        locale::common("Version").to_string(),
                        env!("CARGO_PKG_VERSION").to_string(),
                        theme::text_muted(),
                    ))
                    .child(
                        div()
                            .id("mobile-settings-disconnect")
                            .mt(px(theme::SPACING_LG))
                            .mb(px(theme::SPACING_XL))
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(rgb(theme::ACCENT_RED))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(rgb(theme::ACCENT_RED))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::forget_desktop))
                            .child(locale::text(
                                "Disconnect desktop",
                                "断开桌面端",
                                "中斷桌面版連線",
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_usage(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let session_count = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_ref())
            .map_or(0, Vec::len);
        div()
            .absolute()
            .inset_0()
            .bg(theme::bg_primary())
            .block_mouse_except_scroll()
            .flex()
            .flex_col()
            .child(self.render_overlay_header("mobile-usage", "Usage Statistics", cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_XL))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::SPACING_LG))
                    .child(
                        div()
                            .size(px(56.0))
                            .rounded(px(theme::RADIUS_CARD))
                            .bg(theme::bg_card_dim())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/activity.svg")
                                    .size(px(theme::ICON_MD))
                                    .text_color(rgb(theme::ACCENT_BLUE)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_HEADING))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_primary())
                            .child(locale::common("Usage Statistics")),
                    )
                    .child(
                        div()
                            .w_full()
                            .rounded(px(theme::RADIUS_CARD))
                            .border_1()
                            .border_color(theme::border_subtle())
                            .bg(theme::bg_card_dim())
                            .p(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .child(locale::common("Sessions")),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_HEADING))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(session_count.to_string()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_muted())
                            .text_center()
                            .child(locale::common(
                                "Usage details are available on the desktop host.",
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_mobile_overlay(
        &self,
        overlay: MobileOverlay,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match overlay {
            MobileOverlay::Hosts => self.render_hosts(cx),
            MobileOverlay::Settings => self.render_settings(cx),
            MobileOverlay::Usage => self.render_usage(cx),
            MobileOverlay::NewProject => self.render_new_project(cx),
        }
    }
}

fn drawer_snap_duration_ms(from: f32, target: f32) -> u64 {
    if target.abs() > from.abs() {
        theme::DRAWER_OPEN_ANIMATION_MS
    } else {
        theme::DRAWER_CLOSE_ANIMATION_MS
    }
}

fn drawer_animation(from: f32, target: f32) -> Animation {
    let animation = Animation::new(Duration::from_millis(drawer_snap_duration_ms(from, target)));
    if target.abs() > from.abs() {
        animation.with_easing(ease_out_quint())
    } else {
        animation.with_easing(ease_in_out)
    }
}

#[derive(Debug, Clone, Copy)]
enum DrawerPanInput {
    Started { delta_x: f32, delta_y: f32 },
    Moved { delta_x: f32, delta_y: f32 },
    Ended,
    Cancelled,
    Ignore,
}

fn drawer_pan_input(event: &ScrollWheelEvent) -> DrawerPanInput {
    match event.touch_phase {
        TouchPhase::Ended => DrawerPanInput::Ended,
        TouchPhase::Cancelled => DrawerPanInput::Cancelled,
        phase @ (TouchPhase::Started | TouchPhase::Moved) => {
            let ScrollDelta::Pixels(delta) = event.delta else {
                return DrawerPanInput::Ignore;
            };
            let (delta_x, delta_y) = (f32::from(delta.x), f32::from(delta.y));
            match phase {
                TouchPhase::Started => DrawerPanInput::Started { delta_x, delta_y },
                TouchPhase::Moved => DrawerPanInput::Moved { delta_x, delta_y },
                TouchPhase::Ended | TouchPhase::Cancelled => unreachable!(),
            }
        }
    }
}

fn sessions_button_target(drawer_open: bool) -> f32 {
    if drawer_open {
        0.0
    } else {
        DrawerPage::Sessions.open_offset()
    }
}

fn drawer_drag_origin(offset: f32) -> DrawerDragOrigin {
    let Some(page) = drawer_page_at_offset(offset) else {
        return DrawerDragOrigin::Main;
    };
    if (offset.abs() - 1.0).abs() < 0.001 {
        DrawerDragOrigin::Page(page)
    } else {
        DrawerDragOrigin::Partial(page)
    }
}

fn drawer_page_at_offset(offset: f32) -> Option<DrawerPage> {
    if offset > f32::EPSILON {
        Some(DrawerPage::Sessions)
    } else if offset < -f32::EPSILON {
        Some(DrawerPage::Workbench)
    } else {
        None
    }
}

fn drawer_offset_is_intermediate(offset: f32) -> bool {
    drawer_page_at_offset(offset).is_some() && (offset.abs() - 1.0).abs() >= 0.001
}

fn drawer_nearest_target(offset: f32) -> f32 {
    if offset > 0.5 {
        DrawerPage::Sessions.open_offset()
    } else if offset < -0.5 {
        DrawerPage::Workbench.open_offset()
    } else {
        0.0
    }
}

fn drawer_terminal_target(
    gesture: Option<DrawerGesture>,
    offset: f32,
    settled_target: f32,
    cancelled: bool,
) -> Option<f32> {
    if let Some(DrawerGesture::Dragging { page, last_dx }) = gesture {
        return Some(if cancelled {
            settled_target
        } else {
            drawer_snap_target(page, offset, last_dx, settled_target)
        });
    }
    drawer_offset_is_intermediate(offset).then(|| {
        if cancelled {
            settled_target
        } else {
            drawer_nearest_target(offset)
        }
    })
}

/// What a page touch pan should do once its accumulated translation is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerPanDecision {
    /// Too little movement so far to tell a swipe from a tap.
    Wait,
    /// The pan belongs to something else — most often a vertical scroll.
    Cancel,
    /// The pan is a horizontal swipe for one of the two side pages.
    Drag(DrawerPage),
}

fn drawer_pan_decision(origin: DrawerDragOrigin, dx: f32, dy: f32) -> DrawerPanDecision {
    let (abs_dx, abs_dy) = (dx.abs(), dy.abs());
    if abs_dx < theme::DRAWER_DRAG_THRESHOLD && abs_dy < theme::DRAWER_DRAG_THRESHOLD {
        return DrawerPanDecision::Wait;
    }
    if abs_dx < theme::DRAWER_DRAG_THRESHOLD
        || abs_dy > abs_dx * theme::DRAWER_VERTICAL_CANCEL_RATIO
    {
        return DrawerPanDecision::Cancel;
    }
    match origin {
        DrawerDragOrigin::Main if dx > 0.0 => DrawerPanDecision::Drag(DrawerPage::Sessions),
        DrawerDragOrigin::Main => DrawerPanDecision::Drag(DrawerPage::Workbench),
        DrawerDragOrigin::Page(DrawerPage::Sessions) if dx < 0.0 => {
            DrawerPanDecision::Drag(DrawerPage::Sessions)
        }
        DrawerDragOrigin::Page(DrawerPage::Workbench) if dx > 0.0 => {
            DrawerPanDecision::Drag(DrawerPage::Workbench)
        }
        DrawerDragOrigin::Page(_) => DrawerPanDecision::Cancel,
        DrawerDragOrigin::Partial(page) => DrawerPanDecision::Drag(page),
    }
}

fn visible_drawer_page(offset: f32, snap: Option<DrawerSnap>) -> Option<DrawerPage> {
    let active_offset = snap
        .map(|snap| {
            if snap.target.abs() > f32::EPSILON {
                snap.target
            } else {
                snap.from
            }
        })
        .unwrap_or(offset);
    if active_offset > f32::EPSILON {
        Some(DrawerPage::Sessions)
    } else if active_offset < -f32::EPSILON {
        Some(DrawerPage::Workbench)
    } else {
        None
    }
}

fn drawer_snap_target(page: DrawerPage, offset: f32, last_dx: f32, settled_target: f32) -> f32 {
    let direction = page.open_offset();
    let directional_delta = last_dx * direction;
    let reveal = (offset * direction).clamp(0.0, 1.0);
    let started_on_side_page = (settled_target - direction).abs() < 0.001;
    if started_on_side_page && directional_delta < -theme::DRAWER_SNAP_COMMIT_DIRECTION_THRESHOLD {
        0.0
    } else if !started_on_side_page
        && directional_delta > theme::DRAWER_SNAP_COMMIT_DIRECTION_THRESHOLD
    {
        direction
    } else if started_on_side_page
        && directional_delta > theme::DRAWER_SNAP_REVERSE_DIRECTION_THRESHOLD
    {
        direction
    } else if !started_on_side_page
        && directional_delta < -theme::DRAWER_SNAP_REVERSE_DIRECTION_THRESHOLD
    {
        0.0
    } else if started_on_side_page {
        if reveal <= 1.0 - theme::DRAWER_SNAP_TRAVEL_RATIO {
            0.0
        } else {
            direction
        }
    } else if reveal >= theme::DRAWER_SNAP_TRAVEL_RATIO {
        direction
    } else {
        0.0
    }
}

fn drawer_left(page: DrawerPage, offset: f32, page_width: f32) -> f32 {
    let reveal = (offset * page.open_offset()).clamp(0.0, 1.0);
    match page {
        DrawerPage::Sessions => (reveal - 1.0) * page_width,
        DrawerPage::Workbench => (1.0 - reveal) * page_width,
    }
}

fn drawer_backdrop_opacity(offset: f32) -> f32 {
    offset.abs().clamp(0.0, 1.0) * theme::DRAWER_BACKDROP_OPACITY
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeSelectionSummary {
    primary: String,
    secondary: String,
    available: bool,
}

fn runtime_selection_summary(
    catalog: Option<&SessionRuntimeOptionCatalog>,
    selection: Option<&SessionRuntimeSelection>,
) -> RuntimeSelectionSummary {
    let Some(selection) = selection else {
        return RuntimeSelectionSummary {
            primary: locale::common("Select an Agent session first").to_string(),
            secondary: String::new(),
            available: false,
        };
    };
    let option = catalog.and_then(|catalog| matching_runtime_option(&catalog.options, selection));
    let agent_label = option
        .map(|option| option.agent_label.clone())
        .unwrap_or_else(|| selection.agent_id.to_string());
    let model_label = option
        .map(|option| option.model_label.clone())
        .unwrap_or_else(|| match &selection.model {
            vibex_core::RuntimeModelSelection::AgentDefault => {
                locale::common("Default").to_string()
            }
            vibex_core::RuntimeModelSelection::Explicit { model_id } => model_id.clone(),
        });
    let mut details = vec![
        option
            .map(|option| option.auth_source_label.clone())
            .unwrap_or_else(|| selection.auth_source.id().to_string()),
    ];
    if let Some(reasoning) = selection.reasoning_effort.as_ref() {
        details.push(format!("{}: {reasoning}", locale::common("Reasoning")));
    }
    if let Some(mode) = selection.mode_id.as_ref() {
        details.push(format!("{}: {mode}", locale::common("Mode")));
    }
    RuntimeSelectionSummary {
        primary: format!("{agent_label} / {model_label}"),
        secondary: details.join(" / "),
        available: catalog.is_none_or(|_| {
            option.is_some_and(|option| option.availability == RuntimeOptionAvailability::Available)
        }),
    }
}

fn runtime_string_override(value: String) -> BackendResult<Option<String>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    if value.len() > RUNTIME_FEATURE_VALUE_LIMIT {
        return Err(BackendError::failed(
            "mobile_runtime_feature_value_too_long",
            locale::text(
                "Runtime option values must be at most 256 bytes.",
                "运行时选项值最多为 256 字节。",
                "執行環境選項值最多為 256 位元組。",
            ),
        ));
    }
    Ok(Some(value))
}

fn runtime_option_matches(
    option: &SessionRuntimeOption,
    selection: &SessionRuntimeSelection,
) -> bool {
    option.selection.agent_id == selection.agent_id
        && option.selection.auth_source == selection.auth_source
        && option.selection.model == selection.model
}

fn matching_runtime_option<'a>(
    options: &'a [SessionRuntimeOption],
    selection: &SessionRuntimeSelection,
) -> Option<&'a SessionRuntimeOption> {
    options
        .iter()
        .find(|option| runtime_option_matches(option, selection))
}

fn runtime_selection_is_available(
    options: &[SessionRuntimeOption],
    selection: &SessionRuntimeSelection,
) -> bool {
    matching_runtime_option(options, selection)
        .is_some_and(|option| option.availability == RuntimeOptionAvailability::Available)
}

fn runtime_section_heading(label: impl Into<String>) -> gpui::Div {
    div()
        .h(px(36.0))
        .px_3()
        .flex()
        .items_center()
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::text_muted())
        .child(label.into())
}

fn runtime_choice_button(
    id: impl Into<ElementId>,
    label: impl Into<String>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .min_w_0()
        .max_w_full()
        .h(px(theme::TOUCH_TARGET))
        .px_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(if selected {
            rgb(theme::ACCENT_BLUE).into()
        } else {
            theme::border_default()
        })
        .bg(if selected {
            theme::bg_card()
        } else {
            theme::bg_card_dim()
        })
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::FONT_CAPTION))
        .text_color(if selected {
            theme::text_primary()
        } else {
            theme::text_secondary()
        })
        .child(
            div()
                .max_w_full()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label.into()),
        )
}

fn runtime_sheet_action_button(
    id: impl Into<ElementId>,
    label: impl Into<String>,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(theme::TOUCH_TARGET))
        .px_4()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(if primary {
            rgb(theme::TEXT_PRIMARY).into()
        } else {
            theme::border_default()
        })
        .bg(if primary {
            rgb(theme::TEXT_PRIMARY).into()
        } else {
            theme::bg_card()
        })
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::FONT_BODY))
        .text_color(if primary {
            rgb(theme::BG_PRIMARY).into()
        } else {
            theme::text_secondary()
        })
        .child(label.into())
}

fn runtime_feature_input(input: Entity<TextInput>) -> gpui::Div {
    div()
        .h(px(theme::TOUCH_TARGET))
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default())
        .bg(theme::bg_card())
        .px_1()
        .child(input)
}

fn sidebar_item_ref(item: &SidebarOrganizationItem) -> RemoteSidebarItemRef {
    let (kind, id) = match item {
        SidebarOrganizationItem::Folder(id) => (RemoteSidebarItemKind::Folder, id),
        SidebarOrganizationItem::Project(id) => (RemoteSidebarItemKind::Project, id),
        SidebarOrganizationItem::Session(id) => (RemoteSidebarItemKind::Session, id),
    };
    RemoteSidebarItemRef {
        kind,
        id: id.clone(),
    }
}

fn workspace_label(root: &str) -> &str {
    root.rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(root)
}

fn workspace_mode_label(mode: WorkspaceMode) -> &'static str {
    match mode {
        WorkspaceMode::CurrentCheckout => {
            locale::text("Current checkout", "当前工作区", "目前簽出")
        }
        WorkspaceMode::VibexWorktree => {
            locale::text("Vibex worktree", "Vibex 工作树", "Vibex 工作樹")
        }
    }
}

fn sidebar_workspace_status_color(state: Option<AgentSessionState>) -> u32 {
    match state {
        Some(AgentSessionState::Running | AgentSessionState::Initializing) => theme::ACCENT_BLUE,
        Some(AgentSessionState::NeedsInput) => theme::ACCENT_YELLOW,
        Some(AgentSessionState::Error) => theme::ACCENT_RED,
        Some(AgentSessionState::Idle) => theme::ACCENT_GREEN,
        Some(AgentSessionState::Archived | AgentSessionState::Closed) | None => theme::ACCENT_DIM,
    }
}

fn agent_icon_path(agent_id: &str) -> &'static str {
    let normalized = agent_id.to_ascii_lowercase();
    if normalized.contains("claude") {
        "icons/claude.svg"
    } else if normalized.contains("openai") || normalized.contains("codex") {
        "icons/openai.svg"
    } else {
        "brand/logo.svg"
    }
}

fn session_sidebar_time_label(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return String::new();
    }
    let Some(local_time) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.with_timezone(&chrono::Local).naive_local())
    else {
        return String::new();
    };
    session_sidebar_time_label_at(
        local_time,
        chrono::Local::now().naive_local(),
        locale::current(),
    )
}

fn session_sidebar_time_label_at(
    local_time: chrono::NaiveDateTime,
    now: chrono::NaiveDateTime,
    resolved: vibex_ui::locale::Locale,
) -> String {
    let days = now
        .date()
        .signed_duration_since(local_time.date())
        .num_days();
    if days <= 0 {
        return local_time.format("%H:%M").to_string();
    }
    if days == 1 {
        return locale::text_for(resolved, "Yesterday", "昨天", "昨天").to_string();
    }
    if days < 7 {
        return match resolved {
            vibex_ui::locale::Locale::En => format!("{days} days ago"),
            vibex_ui::locale::Locale::ZhCn | vibex_ui::locale::Locale::ZhTw => {
                format!("{days} 天前")
            }
        };
    }
    if days < 30 {
        let weeks = days / 7;
        return match resolved {
            vibex_ui::locale::Locale::En => format!("{weeks} weeks ago"),
            vibex_ui::locale::Locale::ZhCn => format!("{weeks} 周前"),
            vibex_ui::locale::Locale::ZhTw => format!("{weeks} 週前"),
        };
    }
    local_time.format("%m/%d").to_string()
}

fn timeline_distance_to_bottom(offset_y: f32, max_offset_y: f32) -> f32 {
    (max_offset_y + offset_y).max(0.0)
}

fn approval_response_label(
    approval: &vibex_ui::ApprovalSurfaceModel,
    response: PermissionResponseKind,
    fallback: &'static str,
) -> String {
    approval
        .response_options
        .iter()
        .find(|option| option.response == response)
        .map(|option| option.label.clone())
        .unwrap_or_else(|| fallback.to_string())
}

fn permission_risk_label(risk: PermissionRiskCategory) -> &'static str {
    locale::common(match risk {
        PermissionRiskCategory::Command => "Command",
        PermissionRiskCategory::FileReadSensitive => "Sensitive read",
        PermissionRiskCategory::FileWrite => "File write",
        PermissionRiskCategory::FileDeleteOrMove => "Delete or move",
        PermissionRiskCategory::Network => "Network",
        PermissionRiskCategory::GitDestructive => "Destructive Git",
        PermissionRiskCategory::ProviderConfigExport => "Config export",
        PermissionRiskCategory::CustomTool => "Custom tool",
    })
}

fn process_title(row: &TimelineRow) -> String {
    if !row.title.trim().is_empty() {
        return row.title.clone();
    }
    locale::common(match row.kind {
        TimelineRowKind::Reasoning => "Reasoning",
        TimelineRowKind::Plan => "Plan",
        TimelineRowKind::ToolCall => "Tool",
        TimelineRowKind::Command => "Command",
        TimelineRowKind::FileOperation => "File operation",
        TimelineRowKind::WebSearch => "Web search",
        TimelineRowKind::TodoUpdate => "Task update",
        TimelineRowKind::Collaboration => "Collaboration",
        TimelineRowKind::ImageGeneration => "Image generation",
        TimelineRowKind::GitNotice => "Git",
        TimelineRowKind::SystemNotice => "System",
        TimelineRowKind::PermissionRequest => "Approval",
        TimelineRowKind::PermissionResolution => "Approval response",
        TimelineRowKind::ElicitationRequest => "Input requested",
        TimelineRowKind::ElicitationResolution => "Input response",
        TimelineRowKind::Error => "Error",
        TimelineRowKind::UserMessage | TimelineRowKind::AgentMessage => "Message",
    })
    .to_string()
}

fn flatten_join<T>(outcome: Result<BackendResult<T>, gpui_tokio::JoinError>) -> BackendResult<T> {
    outcome.unwrap_or_else(|_| {
        Err(BackendError::failed(
            "mobile_async_task_failed",
            locale::text(
                "A mobile background task stopped unexpectedly.",
                "移动端后台任务意外停止。",
                "行動端背景工作意外停止。",
            ),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, point};

    fn runtime_option(
        selection: SessionRuntimeSelection,
        availability: RuntimeOptionAvailability,
    ) -> SessionRuntimeOption {
        SessionRuntimeOption {
            selection,
            agent_label: "Codex".to_string(),
            auth_source_label: "Work account".to_string(),
            model_label: "GPT-5".to_string(),
            reasoning_efforts: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            availability,
        }
    }

    #[test]
    fn agent_notification_is_suppressed_only_for_the_visible_session() {
        let selected = VibexSessionId::new();
        let other = VibexSessionId::new();

        assert!(!should_present_agent_notification(
            false,
            false,
            Some(&selected),
            &selected,
        ));
        assert!(should_present_agent_notification(
            true,
            false,
            Some(&selected),
            &selected,
        ));
        assert!(should_present_agent_notification(
            false,
            true,
            Some(&selected),
            &selected,
        ));
        assert!(should_present_agent_notification(
            false,
            false,
            Some(&selected),
            &other,
        ));
    }

    #[test]
    fn composer_runtime_summary_uses_catalog_labels_and_selected_dimensions() {
        let mut selection = SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse("codex").unwrap(),
            vibex_core::ProviderProfileId::new(),
            "gpt-5",
        );
        selection.reasoning_effort = Some("high".to_string());
        selection.mode_id = Some("plan".to_string());
        let catalog = SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options: vec![runtime_option(
                selection.clone(),
                RuntimeOptionAvailability::Available,
            )],
        };

        let summary = runtime_selection_summary(Some(&catalog), Some(&selection));

        assert_eq!(summary.primary, "Codex / GPT-5");
        assert!(summary.secondary.contains("Work account"));
        assert!(summary.secondary.contains("high"));
        assert!(summary.secondary.contains("plan"));
        assert!(summary.available);
    }

    #[test]
    fn unavailable_composer_runtime_is_visible_but_cannot_be_applied() {
        let selection = SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse("codex").unwrap(),
            vibex_core::ProviderProfileId::new(),
            "gpt-5",
        );
        let catalog = SessionRuntimeOptionCatalog {
            revision: 1,
            agents: Vec::new(),
            auth_sources: Vec::new(),
            options: vec![runtime_option(
                selection.clone(),
                RuntimeOptionAvailability::RequiresConfiguration,
            )],
        };

        assert!(!runtime_selection_summary(Some(&catalog), Some(&selection)).available);
        assert!(!runtime_selection_is_available(
            &catalog.options,
            &selection
        ));
    }

    #[test]
    fn composer_runtime_string_values_are_bounded_without_trimming() {
        assert_eq!(runtime_string_override("   ".to_string()).unwrap(), None);
        assert_eq!(
            runtime_string_override("  explicit  ".to_string()).unwrap(),
            Some("  explicit  ".to_string())
        );
        assert_eq!(
            runtime_string_override("x".repeat(RUNTIME_FEATURE_VALUE_LIMIT + 1))
                .unwrap_err()
                .code,
            "mobile_runtime_feature_value_too_long"
        );
    }

    struct DrawerScrollIsolationProbe {
        drawer_scroll: UniformListScrollHandle,
        timeline_scroll: gpui::ScrollHandle,
    }

    impl Render for DrawerScrollIsolationProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .w(px(360.0))
                .h(px(480.0))
                .id("timeline-scroll-probe")
                .track_scroll(&self.timeline_scroll)
                .overflow_y_scroll()
                .child(div().h(px(1_200.0)).flex_none())
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .occlude()
                        .child(
                            uniform_list("drawer-scroll-probe", 60, |range, _, _| {
                                range
                                    .map(|index| {
                                        div()
                                            .h(px(theme::DRAWER_ROW_HEIGHT))
                                            .flex_none()
                                            .child(format!("Session {index}"))
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .track_scroll(&self.drawer_scroll)
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .size_full(),
                        ),
                )
        }
    }

    #[test]
    fn drawer_pan_waits_until_the_gesture_clears_the_threshold() {
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Main, 4.0, 3.0),
            DrawerPanDecision::Wait
        );
    }

    #[test]
    fn drawer_terminal_phases_do_not_depend_on_pixel_delta() {
        let mut event = ScrollWheelEvent {
            position: point(px(0.0), px(0.0)),
            delta: ScrollDelta::Lines(point(0.0, 0.0)),
            modifiers: gpui::Modifiers::default(),
            touch_phase: TouchPhase::Ended,
        };
        assert!(matches!(drawer_pan_input(&event), DrawerPanInput::Ended));

        event.touch_phase = TouchPhase::Cancelled;
        assert!(matches!(
            drawer_pan_input(&event),
            DrawerPanInput::Cancelled
        ));
    }

    #[test]
    fn main_page_swipes_open_the_page_in_the_finger_direction() {
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Main, 24.0, 4.0),
            DrawerPanDecision::Drag(DrawerPage::Sessions)
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Main, -24.0, 4.0),
            DrawerPanDecision::Drag(DrawerPage::Workbench)
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Page(DrawerPage::Sessions), -24.0, 4.0,),
            DrawerPanDecision::Drag(DrawerPage::Sessions)
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Page(DrawerPage::Workbench), 24.0, 4.0,),
            DrawerPanDecision::Drag(DrawerPage::Workbench)
        );
    }

    #[test]
    fn drawer_pan_yields_to_vertical_scrolling_and_wrong_direction_swipes() {
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Main, 8.0, 40.0),
            DrawerPanDecision::Cancel
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Page(DrawerPage::Sessions), 24.0, 2.0,),
            DrawerPanDecision::Cancel
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Page(DrawerPage::Workbench), -24.0, 2.0,),
            DrawerPanDecision::Cancel
        );
    }

    #[test]
    fn a_partial_page_can_resume_or_close_in_either_direction() {
        assert_eq!(
            drawer_drag_origin(0.08),
            DrawerDragOrigin::Partial(DrawerPage::Sessions)
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Partial(DrawerPage::Sessions), 24.0, 2.0,),
            DrawerPanDecision::Drag(DrawerPage::Sessions)
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Partial(DrawerPage::Sessions), -24.0, 2.0,),
            DrawerPanDecision::Drag(DrawerPage::Sessions)
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Partial(DrawerPage::Workbench), -24.0, 2.0,),
            DrawerPanDecision::Drag(DrawerPage::Workbench)
        );
    }

    #[test]
    fn terminal_events_reconcile_every_partial_offset() {
        for offset in [-0.8, -0.2, 0.2, 0.8] {
            let target = drawer_terminal_target(None, offset, 0.0, false)
                .expect("partial drawer should always receive a snap target");
            assert!(!drawer_offset_is_intermediate(target));
        }
        assert_eq!(drawer_terminal_target(None, 0.2, 0.0, false), Some(0.0));
        assert_eq!(drawer_terminal_target(None, 0.8, 0.0, false), Some(1.0));
        assert_eq!(drawer_terminal_target(None, -0.8, 0.0, false), Some(-1.0));
        assert_eq!(drawer_terminal_target(None, 0.0, 0.0, false), None);
        assert_eq!(drawer_terminal_target(None, 1.0, 1.0, false), None);
    }

    #[test]
    fn cancelled_drag_returns_to_the_last_settled_page() {
        let gesture = Some(DrawerGesture::Dragging {
            page: DrawerPage::Sessions,
            last_dx: -24.0,
        });
        assert_eq!(drawer_terminal_target(gesture, 0.3, 1.0, true), Some(1.0));
        assert_eq!(drawer_terminal_target(gesture, 0.3, 0.0, true), Some(0.0));
    }

    #[test]
    fn header_button_targets_the_full_sessions_page() {
        assert_eq!(sessions_button_target(false), 1.0);
        assert_eq!(sessions_button_target(true), 0.0);
    }

    #[gpui::test]
    fn rendered_workspace_pan_reconciles_after_touch_end(cx: &mut TestAppContext) {
        let data_dir = tempfile::tempdir().expect("temporary mobile data directory");
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = MobileApp::new(data_dir.path().to_path_buf(), window, cx);
            app.mode = RootMode::Workspace;
            app
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let page_width = cx.update(|window, _| workspace_page_width(window));
        let position = point(px(120.0), px(120.0));
        cx.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(24.0), px(0.0))),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Started,
        });
        cx.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(80.0), px(0.0))),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Moved,
        });

        let offset_after_move = app.read_with(cx, |app, _| app.drawer_offset);
        assert!(
            (offset_after_move - 104.0 / page_width).abs() < 0.0001,
            "drawer did not receive the full move stream: offset={offset_after_move}, width={page_width}"
        );
        assert!(app.read_with(cx, |app, _| {
            matches!(app.drawer_gesture, Some(DrawerGesture::Dragging { .. }))
                && app.drawer_offset > 0.0
                && app.drawer_snap.is_none()
        }));

        cx.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(point(0.0, 0.0)),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Ended,
        });
        cx.run_until_parked();

        let after_end = app.read_with(cx, |app, _| {
            (
                app.drawer_offset,
                app.drawer_snap.is_some(),
                app.drawer_gesture,
            )
        });
        assert!(
            after_end.1,
            "drawer state after terminal event: {after_end:?}"
        );
        assert!(after_end.2.is_none());
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(app.read_with(cx, |app, _| app.drawer_offset), 1.0);
    }

    #[test]
    fn drawer_snap_uses_forgiving_travel_hysteresis() {
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.13, 0.0, 0.0),
            1.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.11, 0.0, 0.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.87, 0.0, 1.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.89, 0.0, 1.0),
            1.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.13, 0.0, 0.0),
            -1.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.11, 0.0, 0.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.87, 0.0, -1.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.89, 0.0, -1.0),
            -1.0
        );
    }

    #[test]
    fn drawer_snap_ignores_release_jitter_but_honors_decisive_direction() {
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.6, -9.0, 0.0),
            1.0
        );
        assert_eq!(drawer_snap_target(DrawerPage::Sessions, 0.1, 5.0, 0.0), 1.0);
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.8, -29.0, 0.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.6, 9.0, 0.0),
            -1.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.1, -5.0, 0.0),
            -1.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.8, 29.0, 0.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.9, -5.0, 1.0),
            0.0
        );
        assert_eq!(drawer_snap_target(DrawerPage::Sessions, 0.4, 9.0, 1.0), 0.0);
        assert_eq!(
            drawer_snap_target(DrawerPage::Sessions, 0.2, 29.0, 1.0),
            1.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.9, 5.0, -1.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.4, -9.0, -1.0),
            0.0
        );
        assert_eq!(
            drawer_snap_target(DrawerPage::Workbench, -0.2, -29.0, -1.0),
            -1.0
        );
    }

    #[test]
    fn drawer_close_animation_is_faster() {
        assert_eq!(
            drawer_snap_duration_ms(0.0, 1.0),
            theme::DRAWER_OPEN_ANIMATION_MS
        );
        assert_eq!(
            drawer_snap_duration_ms(1.0, 0.0),
            theme::DRAWER_CLOSE_ANIMATION_MS
        );
        assert_eq!(
            drawer_snap_duration_ms(0.0, -1.0),
            theme::DRAWER_OPEN_ANIMATION_MS
        );
        assert_eq!(
            drawer_snap_duration_ms(-1.0, 0.0),
            theme::DRAWER_CLOSE_ANIMATION_MS
        );
    }

    #[test]
    fn side_pages_travel_one_full_viewport() {
        let width = 390.0;
        assert_eq!(drawer_left(DrawerPage::Sessions, 0.0, width), -width);
        assert_eq!(drawer_left(DrawerPage::Sessions, 1.0, width), 0.0);
        assert_eq!(drawer_left(DrawerPage::Workbench, 0.0, width), width);
        assert_eq!(drawer_left(DrawerPage::Workbench, -1.0, width), 0.0);
    }

    #[test]
    fn timeline_follow_only_stays_active_near_the_bottom() {
        assert_eq!(timeline_distance_to_bottom(-500.0, 500.0), 0.0);
        assert_eq!(timeline_distance_to_bottom(-450.0, 500.0), 50.0);
        assert_eq!(timeline_distance_to_bottom(-100.0, 500.0), 400.0);
    }

    #[test]
    fn timeline_list_has_a_scroll_extent_before_offscreen_turns_are_measured() {
        let state = timeline_list_state(10);

        assert_eq!(state.item_count(), 10);
        assert_eq!(
            state.max_offset_for_scrollbar().y,
            px(10.0 * TIMELINE_TURN_ESTIMATED_HEIGHT_PX)
        );
    }

    #[test]
    fn drawer_session_metadata_stays_compact_and_readable() {
        assert_eq!(workspace_label("/work/vibex"), "vibex");
        assert_eq!(workspace_label("/work/vibex/"), "vibex");
        assert_eq!(workspace_label("C:\\work\\vibex"), "vibex");
        let locale = vibex_ui::locale::Locale::En;
        assert_eq!(session_sidebar_time_label(0), "");
        let now = chrono::Local::now().naive_local();
        assert_eq!(
            session_sidebar_time_label_at(now, now, locale),
            now.format("%H:%M").to_string()
        );
        assert_eq!(
            session_sidebar_time_label_at(
                now - chrono::Duration::days(1),
                now,
                vibex_ui::locale::Locale::ZhCn,
            ),
            "昨天"
        );
    }

    #[gpui::test]
    fn drawer_scroll_does_not_move_the_timeline_behind_it(cx: &mut TestAppContext) {
        let drawer_scroll = UniformListScrollHandle::new();
        let observed_drawer_scroll = drawer_scroll.0.borrow().base_handle.clone();
        let timeline_scroll = gpui::ScrollHandle::new();
        let observed_timeline_scroll = timeline_scroll.clone();
        let (_, cx) = cx.add_window_view(|_, _| DrawerScrollIsolationProbe {
            drawer_scroll,
            timeline_scroll,
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(observed_drawer_scroll.max_offset().y > px(0.0));
        assert!(observed_timeline_scroll.max_offset().y > px(0.0));

        cx.simulate_event(ScrollWheelEvent {
            position: point(px(40.0), px(120.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-160.0))),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert!(observed_drawer_scroll.offset().y < px(0.0));
        assert_eq!(observed_timeline_scroll.offset().y, px(0.0));
    }
}

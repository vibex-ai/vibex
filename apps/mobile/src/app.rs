use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_channel::mpsc;
use futures_util::StreamExt as _;
use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, Context, ElementId, Entity, FontWeight,
    IntoElement, KeyBinding, ListAlignment, ListState, MouseButton, MouseUpEvent,
    ParentElement as _, Render, ScrollDelta, ScrollWheelEvent, Styled as _, Task, TouchPhase,
    UniformListScrollHandle, WeakEntity, Window, div, ease_in_out, ease_out_quint, list,
    prelude::*, px, rgb, svg, uniform_list,
};
use vibex_backend::{
    AgentBackend as _, BackendError, BackendEvent, BackendFuture, BackendOperation, BackendResult,
    MutationRequest, WorkspaceBackend as _,
};
use vibex_core::{
    AgentSessionState, ContinueAgentTurnRequest, CreateAgentSessionRequest, ElicitationFieldKind,
    ElicitationResolutionAction, PermissionResolution, PermissionResponseKind,
    PermissionRiskCategory, RemoteLanPairingRequestState, RenameAgentSessionRequest, RequestId,
    ResolvePermissionRequest, RuntimeOptionAvailability, SendAgentMessageRequest, TimelinePayload,
    VibexSessionId, WorkspaceMode, WorkspaceRecord, agent_session_turn_requires_continuation,
    unix_timestamp_ms,
};
use vibex_desktop_model::{TimelineConversationTurn, TimelineRow, TimelineRowKind};
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
use crate::storage::CredentialStorage;
use crate::workbench::MobileWorkbench;
use crate::{locale, markdown, scanner, theme};

const TIMELINE_NEAR_BOTTOM_PX: f32 = 96.0;
const TIMELINE_LIST_OVERDRAW_PX: f32 = 800.0;
const TIMELINE_TURN_ESTIMATED_HEIGHT_PX: f32 = 180.0;
const RESUME_RECOVERY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RESUME_RECOVERY_POLL_ATTEMPTS: usize = 600;

fn timeline_list_state(turn_count: usize) -> ListState {
    ListState::new(
        turn_count,
        ListAlignment::Top,
        px(TIMELINE_LIST_OVERDRAW_PX),
    )
    .with_uniform_item_height(px(TIMELINE_TURN_ESTIMATED_HEIGHT_PX))
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
    workbench_open: bool,
    composer_input: Entity<TextInput>,
    timeline_turns: Arc<Vec<TimelineConversationTurn>>,
    timeline_list: ListState,
    drawer_scroll: UniformListScrollHandle,
    drawer_open: bool,
    drawer_offset: f32,
    drawer_gesture: Option<DrawerGesture>,
    drawer_snap: Option<DrawerSnap>,
    drawer_animation_id: u64,
    drawer_snap_task: Option<Task<()>>,
    expanded_process: BTreeSet<String>,
    expanded_approval: BTreeSet<String>,
    workspaces: Vec<WorkspaceRecord>,
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
    session_action: Option<SessionActionPrompt>,
    session_action_input: Entity<TextInput>,
    session_action_busy: bool,
    notice: Option<String>,
    error: Option<BackendError>,
    app_backgrounded: bool,
    event_stream_generation: u64,
    event_forward_task: Option<Task<Result<(), gpui_tokio::JoinError>>>,
    event_consumer_task: Option<Task<()>>,
    resume_recovery_task: Option<Task<()>>,
    tasks: Vec<Task<()>>,
}

impl MobileApp {
    pub fn new(data_dir: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let storage = CredentialStorage::new(data_dir);
        let stored = storage.load();
        let mode = if matches!(stored, Ok(Some(_))) {
            RootMode::Connecting
        } else {
            RootMode::Pairing
        };
        let mut app = Self {
            storage,
            mode,
            backend: None,
            controller: None,
            workbench: None,
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
            drawer_open: false,
            drawer_offset: 0.0,
            drawer_gesture: None,
            drawer_snap: None,
            drawer_animation_id: 0,
            drawer_snap_task: None,
            expanded_process: BTreeSet::new(),
            expanded_approval: BTreeSet::new(),
            workspaces: Vec::new(),
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
            session_action: None,
            session_action_input: cx.new(|cx| {
                TextInput::new(locale::text("Session name", "会话名称", "工作階段名稱"), cx)
            }),
            session_action_busy: false,
            notice: None,
            error: stored.as_ref().err().cloned(),
            app_backgrounded: false,
            event_stream_generation: 0,
            event_forward_task: None,
            event_consumer_task: None,
            resume_recovery_task: None,
            tasks: Vec::new(),
        };
        if let Ok(Some(bundle)) = stored {
            app.defer_bundle_install(bundle, cx);
        }
        app.start_scanner_result_stream(cx);
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

    fn defer_bundle_install(&mut self, bundle: MobileCredentialBundle, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = entity.update(cx, |this, cx| this.install_bundle(bundle, cx));
        });
        self.tasks.push(task);
    }

    fn install_bundle(&mut self, bundle: MobileCredentialBundle, cx: &mut Context<Self>) {
        crate::discovery::stop();
        self.stop_connection_tasks();
        self.lan_pairing_task = None;
        self.nearby_candidates.clear();
        self.nearby_pairing_state = NearbyPairingState::Idle;
        match bundle.backend() {
            Ok(backend) => {
                if let Some(workbench) = self.workbench.take() {
                    workbench.update(cx, |workbench, _| workbench.suspend());
                }
                self.reset_drawers();
                self.mode = RootMode::Connecting;
                self.error = None;
                self.backend = Some(backend.clone());
                self.connect_backend(backend, cx);
            }
            Err(error) => {
                self.mode = RootMode::Pairing;
                self.error = Some(error);
                let _ = self.storage.clear();
                cx.notify();
            }
        }
    }

    fn connect_backend(&mut self, backend: Arc<WebRemoteBackend>, cx: &mut Context<Self>) {
        self.operation_busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, {
            let backend = backend.clone();
            async move { backend.connect().await }
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
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
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let Ok(mut subscription) = backend.subscribe() else {
            return;
        };
        self.event_stream_generation = self.event_stream_generation.wrapping_add(1);
        let generation = self.event_stream_generation;
        let (sender, mut receiver) = mpsc::unbounded::<BackendEvent>();
        self.event_forward_task = Some(gpui_tokio::Tokio::spawn(cx, async move {
            loop {
                match subscription.next().await {
                    Ok(Some(event)) => {
                        if sender.unbounded_send(event).is_err() {
                            break;
                        }
                    }
                    Ok(None) | Err(_) => {
                        let _ = sender.unbounded_send(BackendEvent::Disconnected);
                        break;
                    }
                }
            }
        }));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            while let Some(event) = receiver.next().await {
                let needs_refetch = entity
                    .update(cx, |this, cx| {
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
            let _ = entity.update(cx, |this, _| {
                if this.event_stream_generation == generation {
                    this.event_forward_task = None;
                }
            });
        });
        self.event_consumer_task = Some(task);
    }

    fn stop_connection_tasks(&mut self) {
        self.event_stream_generation = self.event_stream_generation.wrapping_add(1);
        self.event_forward_task = None;
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
                    this.start_lan_pairing_poll(session, cx);
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
                    Ok(Ok(LanPairingOutcome::Bundle(bundle))) => match this.storage.save(&bundle) {
                        Ok(()) => this.install_bundle(*bundle, cx),
                        Err(error) => {
                            this.nearby_pairing_state = NearbyPairingState::Failed {
                                message: error.message,
                            }
                        }
                    },
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
                if let Some(controller) = this.controller.as_mut()
                    && let Err(error) = controller.apply_runtime_options(outcome)
                {
                    this.error = Some(error);
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
                        this.workspaces = summaries
                            .into_iter()
                            .map(|summary| summary.workspace)
                            .collect();
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
        if let Some(workbench) = self.workbench.as_ref() {
            workbench.update(cx, |workbench, cx| {
                workbench.set_workspace(workspace_id, cx);
                workbench.set_session(session_id, cx);
            });
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.workbench =
            Some(cx.new(|cx| MobileWorkbench::new(backend, workspace_id, session_id, cx)));
    }

    fn close_workbench(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.start_drawer_snap(0.0, Some(window), cx);
    }

    fn create_session(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
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
        let workspace = active_session
            .map(|session| (session.workspace_root.clone(), session.workspace_mode))
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
        if self.operation_busy {
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
        if let Some(backend) = self.backend.take() {
            gpui_tokio::Tokio::spawn(cx, async move { backend.disconnect().await }).detach();
        }
        let _ = self.storage.clear();
        if let Some(workbench) = self.workbench.take() {
            workbench.update(cx, |workbench, _| workbench.suspend());
        }
        self.controller = None;
        self.mode = RootMode::Pairing;
        self.timeline_turns = Arc::new(Vec::new());
        self.timeline_list.reset(0);
        self.reset_drawers();
        self.workspaces.clear();
        self.expanded_process.clear();
        self.expanded_approval.clear();
        self.elicitation_request_id = None;
        self.elicitation_inputs.clear();
        self.elicitation_draft = None;
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
        let ScrollDelta::Pixels(delta) = event.delta else {
            return;
        };
        let (delta_x, delta_y) = (f32::from(delta.x), f32::from(delta.y));

        match event.touch_phase {
            TouchPhase::Started => {
                self.drawer_gesture = None;
                if self.drawer_snap.is_some() || self.session_action.is_some() {
                    return;
                }
                let origin = if self.drawer_offset > 0.0 {
                    DrawerDragOrigin::Page(DrawerPage::Sessions)
                } else if self.drawer_offset < 0.0 {
                    DrawerDragOrigin::Page(DrawerPage::Workbench)
                } else {
                    DrawerDragOrigin::Main
                };
                self.drawer_gesture = Some(DrawerGesture::Pending {
                    origin,
                    dx: 0.0,
                    dy: 0.0,
                });
                // Android reports the translation that broke its touch slop on this
                // very event, so fold it in rather than waiting for the next one.
                self.advance_drawer_pan(delta_x, delta_y, window, cx);
            }
            TouchPhase::Moved => self.advance_drawer_pan(delta_x, delta_y, window, cx),
            TouchPhase::Ended => {
                if let Some(DrawerGesture::Dragging { page, last_dx }) = self.drawer_gesture.take()
                {
                    cx.stop_propagation();
                    let target = drawer_snap_target(page, self.drawer_offset, last_dx);
                    self.start_drawer_snap(target, Some(window), cx);
                }
            }
            TouchPhase::Cancelled => {
                if self.drawer_gesture.take().is_some() {
                    let target = self.settled_drawer_target();
                    self.start_drawer_snap(target, Some(window), cx);
                }
            }
        }
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
                    DrawerPanDecision::Cancel => self.drawer_gesture = None,
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
                    ),
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
                    }),
            )
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
                    .occlude()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(Self::close_drawer_from_backdrop),
                    );
                let drawer_base = match page {
                    DrawerPage::Sessions => self.render_drawer(cx),
                    DrawerPage::Workbench => self.render_workbench_drawer(workbench.clone(), cx),
                }
                .w(px(page_width))
                .occlude();
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
            .when_some(session_action, |root, prompt| {
                root.child(self.render_session_action_prompt(&prompt, cx))
            })
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
            .child(div().size(px(theme::HEADER_BUTTON_SIZE)).flex_shrink_0())
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
        let action_enabled = state.is_some() && !self.operation_busy;
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
    }

    fn render_drawer_session(
        &self,
        session: &vibex_core::AgentSession,
        selected: Option<&VibexSessionId>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let session_id = session.id.clone();
        let is_selected = selected == Some(&session.id);
        let status = match session.state {
            AgentSessionState::Running => theme::ACCENT_GREEN,
            AgentSessionState::NeedsInput => theme::ACCENT_YELLOW,
            AgentSessionState::Error => theme::ACCENT_RED,
            AgentSessionState::Initializing => theme::ACCENT_BLUE,
            _ => theme::ACCENT_DIM,
        };
        let workspace = workspace_label(&session.workspace_root);
        let age = session_age_label(session.last_message_at_ms);

        div()
            .id(format!("session:{}", session.id))
            .mx(px(theme::SPACING_SM))
            .h(px(theme::DRAWER_ROW_HEIGHT))
            .min_h(px(theme::DRAWER_ROW_HEIGHT))
            .rounded(px(theme::RADIUS_CONTROL))
            .px(px(theme::SPACING_MD))
            .flex()
            .items_center()
            .gap(px(theme::SPACING_MD))
            .when(is_selected, |row| {
                row.bg(theme::bg_card())
                    .border_1()
                    .border_color(theme::border_default())
            })
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.open_session(session_id.clone(), cx)),
            )
            .child(
                div()
                    .size(px(theme::ICON_STATUS))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(rgb(status)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.0))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(if is_selected {
                                theme::text_primary()
                            } else {
                                theme::text_secondary()
                            })
                            .child(session.title.clone()),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(format!("{workspace}  {age}")),
                    ),
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
            .bg(theme::bg_primary())
            .border_l_1()
            .border_color(theme::border_default())
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(theme::HEADER_HEIGHT))
                    .flex_shrink_0()
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

    fn render_drawer(&self, cx: &mut Context<Self>) -> gpui::Div {
        let sessions = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.sessions.value.as_ref())
            .cloned()
            .unwrap_or_default();
        let selected = self
            .controller
            .as_ref()
            .and_then(|controller| controller.state.selected_session_id.clone());
        let selected_session = selected.as_ref().and_then(|session_id| {
            sessions
                .iter()
                .find(|session| &session.id == session_id)
                .cloned()
        });
        let capabilities = self
            .backend
            .as_ref()
            .map(|backend| backend.capability_snapshot().agent);
        let can_create_session = capabilities.as_ref().is_some_and(|capabilities| {
            capabilities.supports(BackendOperation::AgentCreateSession)
        });
        let can_manage_session = capabilities.as_ref().is_some_and(|capabilities| {
            capabilities.supports(BackendOperation::AgentManageSession)
        });
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .bg(theme::bg_primary())
            .border_r_1()
            .border_color(theme::border_default())
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(theme::HEADER_HEIGHT))
                    .flex_shrink_0()
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
                            .child(locale::common("Sessions")),
                    )
                    .child(
                        div()
                            .id("close-session-drawer")
                            .size(px(theme::HEADER_BUTTON_SIZE))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.opacity(0.6))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::close_drawer))
                            .child(
                                svg()
                                    .path("icons/x.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
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
                            .id("create-session")
                            .flex_shrink_0()
                            .h(px(theme::DRAWER_ACTION_HEIGHT))
                            .mx(px(theme::SPACING_SM))
                            .mt(px(theme::SPACING_SM))
                            .px(px(theme::SPACING_MD))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_secondary())
                            .when(can_create_session, |button| {
                                button
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::create_session),
                                    )
                            })
                            .when(!can_create_session, |button| button.opacity(0.55))
                            .child(
                                svg()
                                    .path("icons/plus.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            )
                            .child(locale::common("New session")),
                    )
                    .when_some(selected_session, |drawer, session| {
                        let rename_id = session.id.clone();
                        let archive_id = session.id.clone();
                        let delete_id = session.id.clone();
                        let rename_title = session.title.clone();
                        let archive_title = session.title.clone();
                        let delete_title = session.title.clone();
                        drawer.child(
                            div()
                                .flex_shrink_0()
                                .min_h(px(theme::DRAWER_ACTION_HEIGHT))
                                .mx(px(theme::SPACING_SM))
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .id("rename-selected-session")
                                        .h(px(36.0))
                                        .flex_1()
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::text_muted())
                                        .when(can_manage_session, |button| {
                                            button
                                                .cursor_pointer()
                                                .active(|style| style.bg(theme::row_pressed_bg()))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.begin_session_action(
                                                            SessionActionKind::Rename,
                                                            rename_id.clone(),
                                                            rename_title.clone(),
                                                            cx,
                                                        )
                                                    }),
                                                )
                                        })
                                        .when(!can_manage_session, |button| button.opacity(0.55))
                                        .child(locale::common("Rename")),
                                )
                                .child(
                                    div()
                                        .id("archive-selected-session")
                                        .h(px(36.0))
                                        .flex_1()
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::text_muted())
                                        .when(can_manage_session, |button| {
                                            button
                                                .cursor_pointer()
                                                .active(|style| style.bg(theme::row_pressed_bg()))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.begin_session_action(
                                                            SessionActionKind::Archive,
                                                            archive_id.clone(),
                                                            archive_title.clone(),
                                                            cx,
                                                        )
                                                    }),
                                                )
                                        })
                                        .when(!can_manage_session, |button| button.opacity(0.55))
                                        .child(locale::common("Archive")),
                                )
                                .child(
                                    div()
                                        .id("delete-selected-session")
                                        .h(px(36.0))
                                        .flex_1()
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(rgb(theme::ACCENT_RED))
                                        .when(can_manage_session, |button| {
                                            button
                                                .cursor_pointer()
                                                .active(|style| style.bg(theme::row_pressed_bg()))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.begin_session_action(
                                                            SessionActionKind::Delete,
                                                            delete_id.clone(),
                                                            delete_title.clone(),
                                                            cx,
                                                        )
                                                    }),
                                                )
                                        })
                                        .when(!can_manage_session, |button| button.opacity(0.55))
                                        .child(locale::common("Delete")),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(theme::DRAWER_SECTION_HEIGHT))
                            .px(px(theme::SPACING_LG))
                            .flex()
                            .items_end()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .child(locale::common("Conversations")),
                    )
                    .child(
                        uniform_list(
                            "drawer-sessions",
                            sessions.len(),
                            cx.processor(
                                move |this, range: std::ops::Range<usize>, _window, cx| {
                                    range
                                        .filter_map(|index| {
                                            sessions.get(index).map(|session| {
                                                this.render_drawer_session(
                                                    session,
                                                    selected.as_ref(),
                                                    cx,
                                                )
                                            })
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .track_scroll(&self.drawer_scroll)
                        .on_scroll_wheel(cx.listener(Self::consume_drawer_scroll))
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .py(px(theme::SPACING_SM)),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .p(px(theme::SPACING_SM))
                    .child(
                        div()
                            .id("forget-desktop")
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .px(px(theme::SPACING_MD))
                            .flex()
                            .items_center()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
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
    }
}

impl Render for MobileApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_elicitation_form(cx);
        let insets = window.insets().effective();
        let page_width = workspace_page_width(window);
        div()
            .size_full()
            .bg(theme::bg_primary())
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

fn drawer_snap_target(page: DrawerPage, offset: f32, last_dx: f32) -> f32 {
    let direction = page.open_offset();
    let directional_delta = last_dx * direction;
    let reveal = (offset * direction).clamp(0.0, 1.0);
    if directional_delta < -2.0 {
        0.0
    } else if directional_delta > 2.0 || reveal > 0.5 {
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

fn workspace_label(root: &str) -> &str {
    root.rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(root)
}

fn session_age_label(timestamp_ms: i64) -> String {
    session_age_label_for(locale::current(), timestamp_ms)
}

fn session_age_label_for(resolved: vibex_ui::locale::Locale, timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return String::new();
    }
    let elapsed_seconds = unix_timestamp_ms().saturating_sub(timestamp_ms).max(0) / 1_000;
    match elapsed_seconds {
        0..=59 => locale::text_for(resolved, "now", "刚刚", "剛剛").to_string(),
        60..=3_599 => match resolved {
            vibex_ui::locale::Locale::En => format!("{}m", elapsed_seconds / 60),
            vibex_ui::locale::Locale::ZhCn => format!("{} 分钟", elapsed_seconds / 60),
            vibex_ui::locale::Locale::ZhTw => format!("{} 分鐘", elapsed_seconds / 60),
        },
        3_600..=86_399 => match resolved {
            vibex_ui::locale::Locale::En => format!("{}h", elapsed_seconds / 3_600),
            vibex_ui::locale::Locale::ZhCn => format!("{} 小时", elapsed_seconds / 3_600),
            vibex_ui::locale::Locale::ZhTw => format!("{} 小時", elapsed_seconds / 3_600),
        },
        days => match resolved {
            vibex_ui::locale::Locale::En => format!("{}d", days / 86_400),
            vibex_ui::locale::Locale::ZhCn => format!("{} 天", days / 86_400),
            vibex_ui::locale::Locale::ZhTw => format!("{} 天", days / 86_400),
        },
    }
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
    fn drawer_snap_uses_direction_then_half_page() {
        assert_eq!(drawer_snap_target(DrawerPage::Sessions, 0.2, 3.0), 1.0);
        assert_eq!(drawer_snap_target(DrawerPage::Sessions, 0.8, -3.0), 0.0);
        assert_eq!(drawer_snap_target(DrawerPage::Sessions, 0.6, 0.0), 1.0);
        assert_eq!(drawer_snap_target(DrawerPage::Sessions, 0.4, 0.0), 0.0);
        assert_eq!(drawer_snap_target(DrawerPage::Workbench, -0.2, -3.0), -1.0);
        assert_eq!(drawer_snap_target(DrawerPage::Workbench, -0.8, 3.0), 0.0);
        assert_eq!(drawer_snap_target(DrawerPage::Workbench, -0.6, 0.0), -1.0);
        assert_eq!(drawer_snap_target(DrawerPage::Workbench, -0.4, 0.0), 0.0);
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
        assert_eq!(session_age_label_for(locale, 0), "");
        assert_eq!(session_age_label_for(locale, unix_timestamp_ms()), "now");
        assert_eq!(
            session_age_label_for(locale, unix_timestamp_ms() - 60_000),
            "1m"
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

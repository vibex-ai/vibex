use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_channel::mpsc;
use futures_util::StreamExt as _;
use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, Context, ElementId, Entity, FontWeight,
    IntoElement, KeyBinding, MouseButton, MouseUpEvent, ParentElement as _, Render, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, Styled as _, Task, TouchPhase, WeakEntity, Window, div,
    ease_in_out, ease_out_quint, prelude::*, px, rgb, svg,
};
use vibex_backend::{
    AgentBackend as _, BackendError, BackendEvent, BackendResult, MutationRequest,
    WorkspaceBackend as _,
};
use vibex_core::{
    AgentSessionState, ContinueAgentTurnRequest, CreateAgentSessionRequest, ElicitationFieldKind,
    ElicitationResolutionAction, PermissionResolution, PermissionResponseKind,
    PermissionRiskCategory, RequestId, ResolvePermissionRequest, RuntimeOptionAvailability,
    SendAgentMessageRequest, TimelinePayload, VibexSessionId, WorkspaceMode, WorkspaceRecord,
    agent_session_turn_requires_continuation, unix_timestamp_ms,
};
use vibex_desktop_model::{TimelineConversationTurn, TimelineRow, TimelineRowKind};
use vibex_remote_client::WebRemoteBackend;
use vibex_ui::{
    AgentEventDecision, AgentMutationTicket, AgentWorkflowController, AsyncPhase,
    ElicitationFormDraft, ElicitationSurfaceModel, ShellKind,
};

use crate::input::{
    Backspace, Copy, Cut, Delete, Left, Paste, Right, SelectAll, SelectLeft, SelectRight, TextInput,
};
use crate::pairing::{MobileCredentialBundle, claim_pairing_link};
use crate::storage::CredentialStorage;
use crate::{markdown, theme};

const TIMELINE_NEAR_BOTTOM_PX: f32 = 96.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootMode {
    Pairing,
    Connecting,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerDragOrigin {
    Panel,
    Edge,
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
        KeyBinding::new("left", Left, Some("MobileTextInput")),
        KeyBinding::new("right", Right, Some("MobileTextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("MobileTextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("MobileTextInput")),
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
    pairing_input: Entity<TextInput>,
    composer_input: Entity<TextInput>,
    timeline_scroll: ScrollHandle,
    drawer_scroll: ScrollHandle,
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
    operation_busy: bool,
    new_session_busy: bool,
    notice: Option<String>,
    error: Option<BackendError>,
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
            pairing_input: cx.new(|cx| TextInput::new("Paste pairing link", cx)),
            composer_input: cx.new(|cx| TextInput::new("Message Vibex", cx)),
            timeline_scroll: ScrollHandle::new(),
            drawer_scroll: ScrollHandle::new(),
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
            operation_busy: false,
            new_session_busy: false,
            notice: None,
            error: stored.as_ref().err().cloned(),
            tasks: Vec::new(),
        };
        if let Ok(Some(bundle)) = stored {
            app.defer_bundle_install(bundle, cx);
        }
        app
    }

    fn defer_bundle_install(&mut self, bundle: MobileCredentialBundle, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let _ = entity.update(cx, |this, cx| this.install_bundle(bundle, cx));
        });
        self.tasks.push(task);
    }

    fn install_bundle(&mut self, bundle: MobileCredentialBundle, cx: &mut Context<Self>) {
        match bundle.backend() {
            Ok(backend) => {
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
                            "native mobile connection task stopped unexpectedly",
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
        let (sender, mut receiver) = mpsc::unbounded::<BackendEvent>();
        gpui_tokio::Tokio::spawn(cx, async move {
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
        })
        .detach();
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
                                if should_follow {
                                    this.timeline_scroll.scroll_to_bottom();
                                }
                            }
                            Some(AgentEventDecision::Disconnected) => {
                                this.notice = Some("Desktop connection lost".to_string());
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
        self.tasks.push(task);
    }

    fn claim_pairing(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing_busy {
            return;
        }
        let link = self.pairing_input.read(cx).text().trim().to_string();
        if link.is_empty() {
            self.error = Some(BackendError::failed(
                "remote_pairing_fragment_invalid",
                "pairing link is empty",
            ));
            cx.notify();
            return;
        }
        self.pairing_busy = true;
        self.error = None;
        window.hide_soft_keyboard();
        let runner = gpui_tokio::Tokio::spawn(cx, async move { claim_pairing_link(link).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.pairing_busy = false;
                match outcome {
                    Ok(Ok(bundle)) => match this.storage.save(&bundle) {
                        Ok(()) => {
                            this.pairing_input
                                .update(cx, |input, cx| input.set_text("", cx));
                            this.install_bundle(bundle, cx);
                        }
                        Err(error) => this.error = Some(error),
                    },
                    Ok(Err(error)) => this.error = Some(error),
                    Err(_) => {
                        this.error = Some(BackendError::failed(
                            "remote_pairing_task_failed",
                            "pairing task stopped unexpectedly",
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
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
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
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
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
                "No available Agent runtime has been published by the desktop yet",
            ));
            self.refresh_runtime_options(cx);
            cx.notify();
            return;
        };
        let Some((workspace_root, workspace_mode)) = workspace else {
            self.error = Some(BackendError::failed(
                "mobile_workspace_required",
                "Open a workspace on the desktop before creating a mobile session",
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
        self.drawer_open = false;
        self.drawer_offset = 0.0;
        self.drawer_gesture = None;
        self.drawer_snap = None;
        self.drawer_snap_task = None;
        self.expanded_process.clear();
        self.expanded_approval.clear();
        self.timeline_scroll = ScrollHandle::new();
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if let Some(controller) = this.controller.as_mut()
                    && controller.apply_session_snapshot(&ticket, outcome)
                {
                    this.timeline_scroll.scroll_to_bottom();
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn reload_selected_session(&mut self, cx: &mut Context<Self>) {
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
                    && should_follow
                {
                    this.timeline_scroll.scroll_to_bottom();
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
                "session runtime selection is not available yet",
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
        self.timeline_scroll.scroll_to_bottom();
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
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn toggle_drawer(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.drawer_open {
            0.0
        } else {
            theme::DRAWER_WIDTH
        };
        self.start_drawer_snap(target, Some(window), cx);
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
        let target = target.clamp(0.0, theme::DRAWER_WIDTH);
        let from = self.drawer_offset;
        self.drawer_gesture = None;
        self.drawer_open = target > 0.0;
        self.drawer_snap_task = None;
        if let Some(window) = window {
            window.hide_soft_keyboard();
        }
        if (from - target).abs() < 1.0 {
            self.drawer_offset = target;
            self.drawer_snap = None;
            cx.notify();
            return;
        }

        self.drawer_animation_id = self.drawer_animation_id.wrapping_add(1);
        self.drawer_snap = Some(DrawerSnap {
            from,
            target,
            animation_id: self.drawer_animation_id,
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
                    .is_some_and(|snap| snap.target == target && snap.from == from);
                if is_current {
                    this.drawer_offset = target;
                    this.drawer_snap = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
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
            f32::from(self.timeline_scroll.offset().y),
            f32::from(self.timeline_scroll.max_offset().y),
        ) <= TIMELINE_NEAR_BOTTOM_PX
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
        if let Some(backend) = self.backend.take() {
            gpui_tokio::Tokio::spawn(cx, async move { backend.disconnect().await }).detach();
        }
        let _ = self.storage.clear();
        self.controller = None;
        self.mode = RootMode::Pairing;
        self.drawer_open = false;
        self.drawer_offset = 0.0;
        self.drawer_gesture = None;
        self.drawer_snap = None;
        self.drawer_snap_task = None;
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

    /// Drives the drawer from the platform touch-pan stream. Android and iOS both
    /// synthesize a finger drag as `ScrollWheel` events carrying a `TouchPhase`;
    /// mouse-move events only ever describe a tap, so they cannot drive a swipe.
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
                if self.drawer_snap.is_some() {
                    return;
                }
                let x = f32::from(event.position.x);
                let origin = if self.drawer_offset > 0.0 && x <= self.drawer_offset {
                    DrawerDragOrigin::Panel
                } else if self.drawer_offset <= 0.0 && x <= theme::DRAWER_EDGE_ZONE {
                    DrawerDragOrigin::Edge
                } else {
                    return;
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
                if let Some(DrawerGesture::Dragging { last_dx }) = self.drawer_gesture.take() {
                    cx.stop_propagation();
                    let target = drawer_snap_target(self.drawer_offset, last_dx);
                    self.start_drawer_snap(target, Some(window), cx);
                }
            }
            TouchPhase::Cancelled => {
                if self.drawer_gesture.take().is_some() {
                    let target = if self.drawer_open {
                        theme::DRAWER_WIDTH
                    } else {
                        0.0
                    };
                    self.start_drawer_snap(target, Some(window), cx);
                }
            }
        }
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
                    DrawerPanDecision::Drag => {
                        window.hide_soft_keyboard();
                        self.drawer_gesture = Some(DrawerGesture::Dragging { last_dx: 0.0 });
                        self.apply_drawer_drag(dx, cx);
                        cx.stop_propagation();
                    }
                }
            }
            Some(DrawerGesture::Dragging { .. }) => {
                self.apply_drawer_drag(delta_x, cx);
                cx.stop_propagation();
            }
            None => {}
        }
    }

    fn apply_drawer_drag(&mut self, delta_x: f32, cx: &mut Context<Self>) {
        self.drawer_offset = (self.drawer_offset + delta_x).clamp(0.0, theme::DRAWER_WIDTH);
        self.drawer_open = self.drawer_offset > 0.0;
        self.drawer_gesture = Some(DrawerGesture::Dragging { last_dx: delta_x });
        cx.notify();
    }

    fn render_pairing(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .mb(px(theme::SPACING_XL))
                    .child(
                        svg()
                            .path("brand/logo.svg")
                            .size(px(60.0))
                            .text_color(theme::text_primary())
                            .mb(px(theme::SPACING_SM)),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_APP_TITLE))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(theme::text_primary())
                            .child("Vibex"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_muted())
                            .child("Drive your desktop Agent sessions from anywhere."),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(theme::CARD_WIDTH))
                    .flex()
                    .flex_col()
                    .gap(px(theme::SPACING_SM))
                    .child(
                        div()
                            .text_size(px(theme::FONT_DETAIL))
                            .text_color(theme::text_muted())
                            .child("Paste the pairing link from Vibex desktop."),
                    )
                    .child(
                        div()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .bg(theme::bg_card())
                            .child(self.pairing_input.clone()),
                    )
                    .when_some(self.error.as_ref(), |panel, error| {
                        panel.child(
                            div()
                                .rounded(px(theme::RADIUS_CARD))
                                .border_1()
                                .border_color(rgb(theme::ACCENT_RED))
                                .bg(theme::bg_card_dim())
                                .p(px(theme::SPACING_MD))
                                .text_size(px(theme::FONT_DETAIL))
                                .text_color(rgb(theme::ACCENT_RED))
                                .child(error.message.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("pair-desktop")
                            .h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(rgb(theme::TEXT_PRIMARY))
                            .text_color(rgb(theme::BG_PRIMARY))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(theme::FONT_HEADING))
                            .cursor_pointer()
                            .active(|style| style.opacity(0.7))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::claim_pairing))
                            .child(if self.pairing_busy {
                                "Pairing\u{2026}"
                            } else {
                                "Pair"
                            }),
                    ),
            )
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
                                    "Connecting to desktop\u{2026}"
                                } else {
                                    "Desktop is unavailable"
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
                                    .child("Retry"),
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
                                    .child("Disconnect"),
                            )
                    }),
            )
    }

    fn render_workspace(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let controller = self.controller.as_ref();
        let title = controller
            .and_then(|controller| controller.state.active_session.value.as_ref())
            .map(|session| session.title.as_str())
            .unwrap_or("Vibex");
        let state = controller
            .and_then(|controller| controller.state.active_session.value.as_ref())
            .map(|session| session.state);
        let running = state == Some(AgentSessionState::Running);
        let timeline_loading = controller.is_some_and(|controller| {
            controller.state.timeline_status.phase == AsyncPhase::Loading
        });
        let turns = controller
            .map(|controller| controller.state.conversation_turns())
            .unwrap_or_default();
        let approvals = controller
            .map(|controller| controller.state.approval_surfaces(ShellKind::Compact))
            .unwrap_or_default();
        let elicitations = controller
            .map(|controller| controller.state.elicitation_surfaces(ShellKind::Compact))
            .unwrap_or_default();
        let turn_elements = turns
            .iter()
            .map(|turn| self.render_turn(turn, cx).into_any_element())
            .collect::<Vec<_>>();
        let show_drawer = self.drawer_offset > 0.0 || self.drawer_snap.is_some();

        div()
            .size_full()
            .relative()
            .capture_scroll_wheel(cx.listener(Self::drawer_pan))
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(title, state, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .id("timeline-scroll")
                            .overflow_y_scroll()
                            .track_scroll(&self.timeline_scroll)
                            .px_4()
                            .py_4()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .when(timeline_loading && turns.is_empty(), |timeline| {
                                timeline.child(
                                    div()
                                        .py_8()
                                        .text_size(px(theme::FONT_BODY))
                                        .text_color(theme::text_muted())
                                        .text_center()
                                        .child("Loading conversation..."),
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
                                        .child("No messages yet")
                                        .when(
                                            controller.is_some_and(|controller| {
                                                controller.state.selected_session_id.is_none()
                                            }),
                                            |empty| {
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
                                                        .cursor_pointer()
                                                        .on_mouse_up(
                                                            MouseButton::Left,
                                                            cx.listener(Self::create_session),
                                                        )
                                                        .child("New session"),
                                                )
                                            },
                                        ),
                                )
                            })
                            .children(turn_elements),
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
            .when(show_drawer, |root| {
                let backdrop_base = div()
                    .id("drawer-backdrop")
                    .absolute()
                    .inset_0()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(Self::close_drawer_from_backdrop),
                    );
                let drawer_base = self.render_drawer(cx);
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
                                    element.left(px(offset - theme::DRAWER_WIDTH))
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
                            .left(px(self.drawer_offset - theme::DRAWER_WIDTH))
                            .into_any_element(),
                    )
                };
                root.child(backdrop).child(drawer)
            })
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
                                        .child(
                                            turn.live_status
                                                .clone()
                                                .unwrap_or_else(|| "Process".to_string()),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::text_muted())
                                        .child(if expanded {
                                            "Hide".to_string()
                                        } else {
                                            format!("{} items", turn.process_rows.len())
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
                                    .unwrap_or_else(|| "Working...".to_string()),
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
        let approve_label =
            approval_response_label(approval, PermissionResponseKind::Approve, "Approve");
        let deny_label = approval_response_label(approval, PermissionResponseKind::Deny, "Deny");
        let always_label = approval_response_label(
            approval,
            PermissionResponseKind::AlwaysAllowForSession,
            "Always allow",
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
                            "Show less".to_string()
                        } else {
                            format!("Show all {} details", approval.details.len())
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
                                .child("Resolving..."),
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
            let control =
                match &field.kind {
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
                        .children([(false, "No"), (true, "Yes")].into_iter().map(
                            |(value, label)| {
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
                            },
                        ))
                        .into_any_element(),
                    ElicitationFieldKind::MultiSelect { options, .. } => div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(options.iter().map(|option| {
                            let request_id = request.id.clone();
                            let field_id = field.id.clone();
                            let value = option.value.clone();
                            let selected = self.elicitation_draft.as_ref().is_some_and(|draft| {
                                draft.multi_selected(&field.id, &option.value)
                            });
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
                        .child(format!("Unsupported input type: {schema_type}"))
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
                            .unwrap_or_else(|| "Input requested".to_string()),
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
                            .child("Decline"),
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
                            .child(if pending { "Submitting..." } else { "Submit" }),
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
                            "Continue"
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
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(theme::DRAWER_WIDTH))
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
                            .child("Sessions"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .id("create-session")
                                    .size(px(theme::HEADER_BUTTON_SIZE))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .active(|style| style.opacity(0.6))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::create_session),
                                    )
                                    .child(
                                        svg()
                                            .path("icons/plus.svg")
                                            .size(px(theme::ICON_MD))
                                            .text_color(theme::text_secondary()),
                                    ),
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
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .id("drawer-scroll")
                    .overflow_y_scroll()
                    .track_scroll(&self.drawer_scroll)
                    .py(px(theme::SPACING_SM))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .children(sessions.into_iter().map(|session| {
                        let session_id = session.id.clone();
                        let is_selected = selected.as_ref() == Some(&session.id);
                        let status = match session.state {
                            AgentSessionState::Running => theme::ACCENT_GREEN,
                            AgentSessionState::Error => theme::ACCENT_RED,
                            _ => theme::ACCENT_DIM,
                        };
                        div()
                            .id(format!("session:{}", session.id))
                            .mx(px(theme::SPACING_SM))
                            .min_h(px(theme::TOUCH_TARGET))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .px(px(theme::SPACING_MD))
                            .py(px(theme::SPACING_SM))
                            .flex()
                            .items_center()
                            .gap(px(theme::SPACING_MD))
                            .when(is_selected, |row| row.bg(theme::bg_card()))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.open_session(session_id.clone(), cx)
                                }),
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
                                            .child(session.title),
                                    )
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(theme::FONT_CAPTION))
                                            .text_color(theme::text_muted())
                                            .child(session.workspace_root),
                                    ),
                            )
                    })),
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
                            .child("Disconnect desktop"),
                    ),
            )
    }
}

impl Render for MobileApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_elicitation_form(cx);
        let insets = window.insets().effective();
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
                RootMode::Workspace => self.render_workspace(cx).into_any_element(),
            })
    }
}

fn drawer_snap_duration_ms(from: f32, target: f32) -> u64 {
    if target > from {
        theme::DRAWER_OPEN_ANIMATION_MS
    } else {
        theme::DRAWER_CLOSE_ANIMATION_MS
    }
}

fn drawer_animation(from: f32, target: f32) -> Animation {
    let animation = Animation::new(Duration::from_millis(drawer_snap_duration_ms(from, target)));
    if target > from {
        animation.with_easing(ease_out_quint())
    } else {
        animation.with_easing(ease_in_out)
    }
}

/// What an in-progress edge/panel touch pan should do once its accumulated
/// translation is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerPanDecision {
    /// Too little movement so far to tell a swipe from a tap.
    Wait,
    /// The pan belongs to something else — most often a vertical scroll.
    Cancel,
    /// The pan is a horizontal drawer swipe.
    Drag,
}

fn drawer_pan_decision(origin: DrawerDragOrigin, dx: f32, dy: f32) -> DrawerPanDecision {
    let (abs_dx, abs_dy) = (dx.abs(), dy.abs());
    if abs_dx < theme::DRAWER_DRAG_THRESHOLD && abs_dy < theme::DRAWER_DRAG_THRESHOLD {
        return DrawerPanDecision::Wait;
    }
    let toward_drawer = match origin {
        DrawerDragOrigin::Panel => dx < 0.0,
        DrawerDragOrigin::Edge => dx > 0.0,
    };
    if !toward_drawer
        || abs_dx < theme::DRAWER_DRAG_THRESHOLD
        || abs_dy > abs_dx * theme::DRAWER_VERTICAL_CANCEL_RATIO
    {
        return DrawerPanDecision::Cancel;
    }
    DrawerPanDecision::Drag
}

fn drawer_snap_target(offset: f32, last_dx: f32) -> f32 {
    if last_dx < -2.0 {
        0.0
    } else if last_dx > 2.0 || offset > theme::DRAWER_WIDTH / 2.0 {
        theme::DRAWER_WIDTH
    } else {
        0.0
    }
}

fn drawer_backdrop_opacity(offset: f32) -> f32 {
    (offset / theme::DRAWER_WIDTH).clamp(0.0, 1.0) * theme::DRAWER_BACKDROP_OPACITY
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
    match risk {
        PermissionRiskCategory::Command => "Command",
        PermissionRiskCategory::FileReadSensitive => "Sensitive read",
        PermissionRiskCategory::FileWrite => "File write",
        PermissionRiskCategory::FileDeleteOrMove => "Delete or move",
        PermissionRiskCategory::Network => "Network",
        PermissionRiskCategory::GitDestructive => "Destructive Git",
        PermissionRiskCategory::ProviderConfigExport => "Config export",
        PermissionRiskCategory::CustomTool => "Custom tool",
    }
}

fn process_title(row: &TimelineRow) -> String {
    if !row.title.trim().is_empty() {
        return row.title.clone();
    }
    match row.kind {
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
    }
    .to_string()
}

fn flatten_join<T>(outcome: Result<BackendResult<T>, gpui_tokio::JoinError>) -> BackendResult<T> {
    outcome.unwrap_or_else(|_| {
        Err(BackendError::failed(
            "mobile_async_task_failed",
            "native mobile background task stopped unexpectedly",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawer_pan_waits_until_the_gesture_clears_the_threshold() {
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Edge, 4.0, 3.0),
            DrawerPanDecision::Wait
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Edge, 24.0, 4.0),
            DrawerPanDecision::Drag
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Panel, -24.0, 4.0),
            DrawerPanDecision::Drag
        );
    }

    #[test]
    fn drawer_pan_yields_to_vertical_scrolling_and_wrong_direction_swipes() {
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Edge, 8.0, 40.0),
            DrawerPanDecision::Cancel
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Edge, -24.0, 2.0),
            DrawerPanDecision::Cancel
        );
        assert_eq!(
            drawer_pan_decision(DrawerDragOrigin::Panel, 24.0, 2.0),
            DrawerPanDecision::Cancel
        );
    }

    #[test]
    fn drawer_snap_uses_direction_then_half_width() {
        assert_eq!(drawer_snap_target(40.0, 3.0), theme::DRAWER_WIDTH);
        assert_eq!(drawer_snap_target(250.0, -3.0), 0.0);
        assert_eq!(drawer_snap_target(160.0, 0.0), theme::DRAWER_WIDTH);
        assert_eq!(drawer_snap_target(100.0, 0.0), 0.0);
    }

    #[test]
    fn drawer_close_animation_is_faster() {
        assert_eq!(
            drawer_snap_duration_ms(0.0, theme::DRAWER_WIDTH),
            theme::DRAWER_OPEN_ANIMATION_MS
        );
        assert_eq!(
            drawer_snap_duration_ms(theme::DRAWER_WIDTH, 0.0),
            theme::DRAWER_CLOSE_ANIMATION_MS
        );
    }

    #[test]
    fn timeline_follow_only_stays_active_near_the_bottom() {
        assert_eq!(timeline_distance_to_bottom(-500.0, 500.0), 0.0);
        assert_eq!(timeline_distance_to_bottom(-450.0, 500.0), 50.0);
        assert_eq!(timeline_distance_to_bottom(-100.0, 500.0), 400.0);
    }
}

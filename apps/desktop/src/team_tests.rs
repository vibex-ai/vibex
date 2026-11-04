use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use std::sync::{Mutex, mpsc as sync_mpsc};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendEventSubscription, BackendFuture, DisconnectedBackend,
};
use vibex_core::*;
use vibex_db::{
    GroupPresentationRepository, SessionOwnershipRepository, SessionRepository,
    WorkspaceRepository, open_database,
};

enum Observed {
    Message(HumanAgentMessageRequest),
    Permission(ResolvePermissionRequest),
    Timeline(VibexSessionId),
    Delegation(MutationRequest<HumanDelegationRequest>),
}

struct TeamAgent {
    sessions: BTreeMap<String, AgentSession>,
    observed: sync_mpsc::Sender<Observed>,
    received: Mutex<Vec<HumanAgentMessageRequest>>,
}

fn selection(model: &str) -> SessionRuntimeSelection {
    SessionRuntimeSelection::provider(
        AgentId::parse("codex").unwrap(),
        ProviderProfileId::parse("provider_test").unwrap(),
        model,
    )
}

fn selection_state(model: &str) -> AgentSessionRuntimeSelectionState {
    AgentSessionRuntimeSelectionState {
        desired: selection(model),
        effective: selection(model),
        status: SessionRuntimeSelectionStatus::Ready,
        session_revision: 1,
        selection_revision: 1,
        current_binding_id: None,
        activation_generation: 1,
        pending_switch_id: None,
        actionable_error: None,
    }
}

macro_rules! offline_methods {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {$ (
        fn $name(&self, $($arg: $ty),*) -> BackendFuture<'_, $result> {
            Box::pin(async { Err(BackendError::offline("test_offline", "Not used by this fixture")) })
        }
    )*};
}

impl vibex_backend::AgentBackend for TeamAgent {
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>> {
        Err(BackendError::offline("test_offline", "No event stream"))
    }
    fn open_session(&self, id: VibexSessionId) -> BackendFuture<'_, AgentSession> {
        let session = self.sessions.get(id.as_str()).cloned();
        Box::pin(async move {
            session
                .ok_or_else(|| BackendError::failed("session_not_found", "Missing fixture session"))
        })
    }
    fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage> {
        let _ = self
            .observed
            .send(Observed::Timeline(request.session_id.clone()));
        Box::pin(async move {
            Ok(TimelinePage {
                session_id: request.session_id,
                items: Vec::new(),
                start_sequence: None,
                end_sequence: None,
                has_older: false,
                has_newer: false,
                starts_at_turn_start: true,
                leading_turn_start: None,
                omitted: None,
            })
        })
    }
    fn runtime_selection(
        &self,
        _: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState> {
        Box::pin(async { Ok(selection_state("worker-model")) })
    }
    fn send_message_with_mentions(
        &self,
        request: MutationRequest<HumanAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>> {
        self.received.lock().unwrap().push(request.payload.clone());
        let _ = self.observed.send(Observed::Message(request.payload));
        Box::pin(async { Ok(Vec::new()) })
    }
    fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem> {
        let _ = self.observed.send(Observed::Permission(request.payload));
        Box::pin(async {
            Err(BackendError::offline(
                "test_offline",
                "Permission was captured",
            ))
        })
    }
    fn delegate_session(
        &self,
        request: MutationRequest<HumanDelegationRequest>,
    ) -> BackendFuture<'_, HumanDelegationResult> {
        let _ = self.observed.send(Observed::Delegation(request));
        Box::pin(async { Err(BackendError::offline("test_retry", "Retry this request")) })
    }
    offline_methods! {
        list_sessions(_archived: bool) -> Vec<AgentSession>;
        create_session(_request: MutationRequest<CreateAgentSessionRequest>) -> AgentSession;
        send_message(_request: MutationRequest<SendAgentMessageRequest>) -> Vec<TimelineItem>;
        continue_turn(_request: MutationRequest<ContinueAgentTurnRequest>) -> Vec<TimelineItem>;
        interrupt(_request: MutationRequest<VibexSessionId>) -> bool;
        resolve_elicitation(_request: MutationRequest<ResolveElicitationRequest>) -> TimelineItem;
        replace_user_message(_request: MutationRequest<ReplaceUserMessagePayload>) -> Vec<TimelineItem>;
        rename_session(_request: MutationRequest<RenameAgentSessionRequest>) -> AgentSession;
        archive_session(_request: MutationRequest<VibexSessionId>) -> ();
        unarchive_session(_request: MutationRequest<VibexSessionId>) -> AgentSession;
        delete_session(_request: MutationRequest<VibexSessionId>) -> ();
        list_runtime_options() -> SessionRuntimeOptionCatalog;
        probe_agent_runtime_options(_request: MutationRequest<AgentRuntimeOptionProbeRequest>) -> AgentRuntimeOptionProbeResult;
        ensure_default_agent_auth_context(_request: MutationRequest<AgentId>) -> AgentAuthContext;
        refresh_agent_auth_methods(_request: MutationRequest<AgentId>) -> AgentAuthCatalog;
        set_desired_runtime(_request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>) -> AgentSessionRuntimeSelectionState;
        cancel_runtime_switch(_request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>) -> AgentSessionRuntimeSelectionState;
    }
}

struct TeamSidebarHost {
    workbench: Entity<VibexWorkbench>,
    _subscription: Subscription,
}

impl Render for TeamSidebarHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w_80().h_full().child(
            self.workbench
                .update(cx, |workbench, cx| workbench.render_agent_sidebar(cx)),
        )
    }
}

fn session(index: usize) -> AgentSession {
    AgentSession {
        id: VibexSessionId::parse(format!("session_team_{index}")).unwrap(),
        title: format!("Session {index}"),
        project_id: ProjectId::parse("project_team").unwrap(),
        workspace_id: WorkspaceId::parse("workspace_team").unwrap(),
        workspace_root: "/team-test".into(),
        workspace_mode: WorkspaceMode::CurrentCheckout,
        agent_id: AgentId::parse("codex").unwrap(),
        state: AgentSessionState::Idle,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: index as i64,
        updated_at_ms: index as i64,
        last_message_at_ms: index as i64,
        archived_at_ms: None,
        deleted_at_ms: None,
        tools: Default::default(),
        transport: vibex_core::SessionTransport::Acp,
        terminal: None,
    }
}

fn node(index: usize, parent: Option<usize>, children: usize) -> SessionTreeNode {
    SessionTreeNode {
        session_ref: VibexUseRef::session(&session(index).id),
        parent_session_ref: parent.map(|id| VibexUseRef::session(&session(id).id)),
        title: format!("Session {index}"),
        agent_id: Some(session(index).agent_id),
        agent_label: Some("Codex".into()),
        task_ref: None,
        task_title: None,
        task_phase: None,
        completion_policy: None,
        child_count: children,
        has_more_children: false,
        blocked_on: None,
        active_descendants: 0,
        blocked_descendants: 0,
        current_task_ref: None,
        updated_at_ms: 1,
    }
}

fn fixture(
    cx: &mut TestAppContext,
    count: usize,
) -> (
    Entity<VibexWorkbench>,
    &mut VisualTestContext,
    sync_mpsc::Receiver<Observed>,
) {
    // Production Backend calls complete on Tokio's executor.
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
        cx.set_reduce_motion(true);
    });
    let (sender, receiver) = sync_mpsc::channel();
    let agent = Arc::new(TeamAgent {
        sessions: (0..count)
            .map(|index| {
                let session = session(index);
                (session.id.to_string(), session)
            })
            .collect(),
        observed: sender,
        received: Mutex::new(Vec::new()),
    });
    let offline = Arc::new(DisconnectedBackend);
    let facade = BackendFacade::new(
        BackendCapabilitySnapshot::disconnected_v1(),
        agent,
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline.clone(),
        offline,
    );
    let mut workbench = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = VibexWorkbench::with_initial_state(
                None,
                DesktopUiStateV1::default(),
                None,
                window,
                cx,
            );
            view.backend = Some(facade);
            view.sessions = vec![session(0)];
            view.sessions_loaded = true;
            view.selected_session_id = Some(session(0).id);
            view.workspaces = vec![(
                ProjectRecord {
                    id: session(0).project_id,
                    name: "Team project".into(),
                    root_path: "/team-test".into(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                WorkspaceRecord {
                    id: session(0).workspace_id,
                    project_id: session(0).project_id,
                    root_path: "/team-test".into(),
                    mode: WorkspaceMode::CurrentCheckout,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            )];
            for index in 1..count {
                view.delegated_sessions
                    .insert(session(index).id.to_string(), session(index));
            }
            view.borrow_session_view(&session(0).id);
            view.runtime_selection = Some(selection_state("root-model"));
            view
        });
        workbench = Some(view.clone());
        let host = cx.new(|cx| TeamSidebarHost {
            _subscription: cx.observe(&view, |_, _, cx| cx.notify()),
            workbench: view,
        });
        Root::new(host, window, cx)
    });
    (workbench.unwrap(), cx, receiver)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }
}

/// Puts the pointer on a row so the branch toggle it keeps hidden is painted.
///
/// A session row reveals its disclosure instead of parking it at the left edge,
/// and an unpainted element takes no hit box: the click that follows only lands
/// once the row it belongs to is hovered and the frame has been drawn again.
fn reveal_branch_toggle(cx: &mut VisualTestContext, point: gpui::Point<gpui::Pixels>) {
    cx.update(|window, cx| {
        window.simulate_mouse_move(point, cx);
        let _ = window.draw(cx);
    });
}

/// Sends the mouse-down/up pair a double click on a row produces.
///
/// The platform counts the clicks; the test states the count outright so the
/// gesture does not depend on how fast the harness delivers two clicks.
fn double_click(cx: &mut VisualTestContext, point: gpui::Point<gpui::Pixels>) {
    cx.simulate_event(gpui::MouseDownEvent {
        position: point,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: point,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
}

fn group(workbench: &mut VibexWorkbench, count: usize) {
    let members: Vec<_> = (0..count)
        .map(|index| session(index).id.to_string())
        .collect();
    let workspaces = members
        .iter()
        .map(|id| (id.clone(), "workspace_team".into()))
        .collect();
    assert!(workbench.ui_state.sidebar.organization.create_group(
        "team",
        "Team",
        "project_team",
        "workspace_team",
        &members,
        &workspaces,
        None
    ));
}

async fn attach_team_runtime(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
) -> (Arc<DesktopRuntime>, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().to_path_buf();
    let runtime = cx
        .update(|_, cx| {
            gpui_tokio::Tokio::spawn(cx, async move {
                DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home_path))
                    .await
                    .unwrap()
            })
        })
        .await
        .unwrap();
    let conn = open_database(&runtime.config().database_path).unwrap();
    let (project, workspace) = WorkspaceRepository::ensure(
        &conn,
        home.path().join("workspace"),
        WorkspaceMode::CurrentCheckout,
    )
    .unwrap();
    workbench.update(cx, |workbench, _| {
        workbench.runtime = Some(runtime.clone());
        workbench.workspaces = vec![(project.clone(), workspace.clone())];
        for session in workbench
            .sessions
            .iter_mut()
            .chain(workbench.delegated_sessions.values_mut())
        {
            session.project_id = project.id.clone();
            session.workspace_id = workspace.id.clone();
            session.workspace_root = workspace.root_path.clone();
            SessionRepository::insert(&conn, session).unwrap();
        }
    });
    (runtime, home)
}

fn queued_worker_message() -> ComposerQueueMessage {
    ComposerQueueMessage {
        id: 1,
        session_id: session(1).id,
        desired_runtime: selection("worker-model"),
        text: "Ask @Lead".into(),
        attachments: Vec::new(),
        command_invocation: None,
        scheduled_at_ms: Some(i64::MAX),
        mentions: vec![VibexUseMention::session(&session(0).id, Some("@Lead"))],
        dispatch_key: mint_composer_queue_dispatch_key(),
    }
}

/// One message for a pre-send-queue test: an ordinary entry unless `at_ms` gives
/// it a deadline.
fn head_queue_message(
    id: u64,
    session_id: &VibexSessionId,
    at_ms: Option<i64>,
) -> ComposerQueueMessage {
    ComposerQueueMessage {
        id,
        session_id: session_id.clone(),
        desired_runtime: selection("root-model"),
        text: format!("queued message {id}"),
        attachments: Vec::new(),
        command_invocation: None,
        scheduled_at_ms: at_ms,
        mentions: Vec::new(),
        dispatch_key: mint_composer_queue_dispatch_key(),
    }
}

/// The queue hands over its head as soon as the session in front of it is
/// settled — for a due schedule and for a message the reader moved ahead of one
/// alike — while a turn that still occupies a session holds it back.
#[gpui::test]
fn the_pre_send_queue_sends_its_head_when_the_session_is_settled(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let now = unix_timestamp_ms();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let session_id = session(0).id.clone();
            // The last turn ended normally and no turn is in flight.
            workbench.cache_auto_continue_turn_status(
                &session_id,
                session(0).updated_at_ms,
                Some(true),
                cx,
            );
            workbench.composer_queue = vec![
                head_queue_message(1, &session_id, Some(now - 1_000)),
                head_queue_message(2, &session_id, Some(now + 60_000)),
            ];
            // The due head leaves the queue even though a schedule waits behind
            // it.
            assert!(workbench.maybe_dispatch_next_composer_queue_message(
                None,
                ComposerQueueDispatchBehavior::Automatic,
                window,
                cx,
            ));
            assert_eq!(workbench.composer_queue.len(), 1);
            assert_eq!(workbench.composer_queue[0].id, 2);
        });
    });
}

/// The schedule clock is armed inside a window, and the workbench outlives that
/// window when the reader closes it to the tray. The clock must not stop for
/// good there: reopening the window has to arm it again and release whatever
/// fell due while no window could take the send.
#[gpui::test]
fn a_schedule_survives_a_window_closed_to_the_tray(cx: &mut TestAppContext) {
    fn open_workbench_window(
        cx: &mut TestAppContext,
        workbench: &Entity<VibexWorkbench>,
    ) -> VisualTestContext {
        let workbench = workbench.clone();
        let window = cx.open_window(gpui::size(px(1_280.0), px(800.0)), move |window, cx| {
            Root::new(workbench, window, cx)
        });
        let handle: AnyWindowHandle = window.into();
        VisualTestContext::from_window(handle, cx)
    }

    let (workbench, cx, receiver) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.composer_queue = vec![head_queue_message(
                1,
                &session_id,
                Some(unix_timestamp_ms() - 1_000),
            )];
            workbench.start_message_schedule_timer(window, cx);
        });
    });
    assert!(workbench.read_with(&cx.cx, |workbench, _| {
        workbench.message_schedule_task.is_some()
    }));

    // Close the window the clock was armed in. The tray keeps the workbench
    // alive, but the entity no longer has a window to update in.
    cx.update(|window, _| window.remove_window());
    cx.cx.run_until_parked();
    // A tick runs with no window and cannot dispatch. It must not leave the
    // clock pointing at a task that has already stopped.
    cx.background_executor.advance_clock(Duration::from_secs(2));
    cx.cx.run_until_parked();
    assert!(
        workbench.read_with(&cx.cx, |workbench, _| workbench
            .message_schedule_task
            .is_none()),
        "a tick with no window must release the clock it cannot run"
    );

    // Reopen the window, the way the tray's own "Open Vibex" item does.
    let mut reopened = open_workbench_window(&mut cx.cx, &workbench);
    reopened.update(|window, cx| {
        workbench.update(cx, |workbench, cx| workbench.bind_to_window(window, cx));
    });
    for _ in 0..3 {
        reopened.cx.run_until_parked();
        reopened
            .cx
            .background_executor
            .advance_clock(Duration::from_secs(1));
        reopened.cx.run_until_parked();
    }

    assert!(
        workbench.read_with(&reopened, |workbench, _| workbench
            .composer_queue
            .is_empty()),
        "the due message left the queue once the window came back"
    );
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(1)),
        Ok(Observed::Message(_))
    ));
}

/// The turn a dispatch handed over is what holds the queue: while it occupies
/// the session nothing behind it leaves, and a settled session releases it
/// instead of waiting on an asynchronous completion verdict.
#[gpui::test]
fn a_settled_session_releases_the_pre_send_queue_fence(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let now = unix_timestamp_ms();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let session_id = session(0).id.clone();
            workbench.composer_queue = vec![head_queue_message(1, &session_id, Some(now - 1_000))];
            workbench.composer_queue_dispatch_fence = Some(ComposerQueueDispatchFence {
                session_id: session_id.clone(),
                message_id: 99,
                awaits_turn: true,
            });
            workbench.set_session_turn_pending(&session_id, true);
            assert!(!workbench.maybe_dispatch_next_composer_queue_message(
                None,
                ComposerQueueDispatchBehavior::Automatic,
                window,
                cx,
            ));
            assert_eq!(workbench.composer_queue.len(), 1);

            // The turn is over. The settled session releases the fence, so the
            // due schedule fires even though nothing probed its outcome.
            workbench.set_session_turn_pending(&session_id, false);
            assert!(workbench.maybe_dispatch_next_composer_queue_message(
                None,
                ComposerQueueDispatchBehavior::Automatic,
                window,
                cx,
            ));
            assert!(workbench.composer_queue.is_empty());
        });
    });
}

/// A due schedule is not held by a turn another session's queued message
/// started.
///
/// The dispatch fence keeps the queue one message at a time behind the turn the
/// last hand-over started. A schedule is a promise about a moment, though, and
/// an Agent still working in another session has no claim on that moment: the
/// scheduled head's own session is settled, so it goes out even while the fence
/// waits. The unblocked case is the one that left a due message sitting behind
/// an unrelated long-running turn until the reader sent it by hand.
#[gpui::test]
fn a_due_schedule_is_not_held_by_another_sessions_turn(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 2);
    let now = unix_timestamp_ms();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            // The message that last left the queue belongs to another session,
            // whose turn is still occupying it.
            workbench.sessions[0].state = AgentSessionState::Running;
            workbench.composer_queue_dispatch_fence = Some(ComposerQueueDispatchFence {
                session_id: session(0).id,
                message_id: 99,
                awaits_turn: true,
            });
            workbench.composer_queue =
                vec![head_queue_message(1, &session(1).id, Some(now - 1_000))];
            assert!(
                workbench.composer_queue_fence_blocks(),
                "the fixture's other session is still running"
            );

            assert!(
                workbench.maybe_dispatch_next_composer_queue_message(
                    None,
                    ComposerQueueDispatchBehavior::Automatic,
                    window,
                    cx,
                ),
                "the due schedule leaves even though the other session's turn runs"
            );
            assert!(workbench.composer_queue.is_empty());
        });
    });
}

/// Moving a message in front of a schedule makes it the head, and a session
/// that is already settled sends it without waiting for the deadline that used
/// to stand in front of it.
#[gpui::test]
fn moving_a_message_in_front_of_a_schedule_sends_it(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let now = unix_timestamp_ms();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let session_id = session(0).id.clone();
            workbench.cache_auto_continue_turn_status(
                &session_id,
                session(0).updated_at_ms,
                Some(true),
                cx,
            );
            workbench.composer_queue = vec![
                head_queue_message(1, &session_id, Some(now + 3_600_000)),
                head_queue_message(2, &session_id, None),
            ];
            workbench.move_composer_queue_message_relative(2, 1, false, window, cx);
            assert_eq!(workbench.composer_queue.len(), 1);
            assert_eq!(workbench.composer_queue[0].id, 1);
        });
    });
}

/// A settled session's head leaves when no completion verdict can be obtained
/// at all.
///
/// The probe's budget for the revision is spent, so no answer is on its way and
/// none can be asked for again. The settled session is then the best evidence
/// the queue has, and the head must actually leave the queue — not be handed
/// back by the submission path after the dispatcher already reported it gone.
#[gpui::test]
fn the_queue_hands_over_its_head_when_no_verdict_can_be_obtained(cx: &mut TestAppContext) {
    let (workbench, cx, _receiver) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.auto_continue_probe_attempts.insert(
                session_id.to_string(),
                AutoContinueProbeAttempt {
                    session_updated_at_ms: session(0).updated_at_ms,
                    attempted_at: Instant::now() - AUTO_CONTINUE_PROBE_RETRY_BACKOFF,
                    attempts: AUTO_CONTINUE_PROBE_MAX_ATTEMPTS,
                },
            );
            workbench.composer_queue = vec![head_queue_message(1, &session_id, None)];
            assert!(
                workbench.maybe_dispatch_next_composer_queue_message(
                    None,
                    ComposerQueueDispatchBehavior::Automatic,
                    window,
                    cx,
                ),
                "the queue reports the hand-over it decided"
            );
            assert!(
                workbench.composer_queue.is_empty(),
                "a verdict that is not coming cannot hold a settled session's message"
            );
        });
    });
}

/// A hand-over the submission path refuses is not reported as dispatched.
///
/// The local runtime is gone, which is the one case the submission path still
/// hands a message back. The queue must report that honestly: its caller drops
/// the session's re-evaluation on a `true`, and the message would then wait for
/// an event that no longer comes.
#[gpui::test]
fn a_refused_hand_over_is_not_reported_as_dispatched(cx: &mut TestAppContext) {
    let (workbench, cx, _receiver) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.backend = None;
            workbench.composer_queue = vec![head_queue_message(1, &session_id, None)];
            assert!(
                !workbench.maybe_dispatch_next_composer_queue_message(
                    None,
                    ComposerQueueDispatchBehavior::Automatic,
                    window,
                    cx,
                ),
                "a message that stayed in the queue was not dispatched"
            );
            assert_eq!(workbench.composer_queue.len(), 1);
            assert!(
                workbench
                    .composer_queue_paused_session_ids
                    .contains(session_id.as_str()),
                "the local-runtime fallback is the pause the reader can see"
            );
        });
    });
}

/// A live view settles a revision only for the completion it can actually see.
///
/// The streamed answer arrives as deltas, so a view's tail is routinely
/// mid-turn while the session has already settled. Caching that reading as an
/// abnormal stop pins it to the settled revision — no later event can revise
/// it — and the queued auto-send behind it waits for a verdict that will never
/// change. The final answer is the one item that proves the turn ended, and
/// only that reading settles the revision here.
#[gpui::test]
fn a_live_view_settles_only_the_completion_it_can_see(cx: &mut TestAppContext) {
    fn live_event(
        session_id: &VibexSessionId,
        sequence: i64,
        payload: TimelinePayload,
    ) -> TimelineLiveEvent {
        TimelineLiveEvent {
            session_id: session_id.clone(),
            sequence,
            item: TimelineItem {
                id: TimelineItemId::new(),
                session_id: session_id.clone(),
                sequence,
                timestamp_ms: sequence,
                source: TimelineSource::Agent,
                kind: payload.kind(),
                correlation_id: None,
                provider_correlation_id: None,
                redaction_state: TimelineRedactionState::None,
                execution_attribution: None,
                payload_depth: vibex_core::TimelinePayloadDepth::Detail,
                payload,
            },
        }
    }

    let (workbench, cx, _receiver) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    let revision = session(0).updated_at_ms;
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            // The fixture keeps the selected session's view borrowed but never
            // loads it, so its timeline is bound the way an authoritative load
            // binds one: a live projection only applies to the view's own
            // session, and the verdict rule is what this test is about.
            workbench.timeline.session_id = Some(session_id.clone());
            assert!(
                workbench.borrow_session_view_unweighed(&session_id),
                "the borrowed view is bound to the session"
            );
            let applied = workbench.apply_timeline_events_to_borrowed_view(
                &session_id,
                vec![live_event(
                    &session_id,
                    1,
                    TimelinePayload::ToolCall(ToolCallPayload {
                        tool_call_id: "tool_1".into(),
                        tool_name: "execute".into(),
                        status: ToolCallStatus::Started,
                        summary: "cargo test".into(),
                        input_summary: None,
                        output_summary: None,
                        raw_extension: None,
                    }),
                )],
                cx,
            );
            workbench.release_session_view_unweighed();
            assert!(applied.0, "the mid-turn item reached the view");
            assert!(
                workbench
                    .cached_auto_continue_turn_status(&session_id, revision)
                    .is_none(),
                "a mid-turn tail cannot judge the settled revision"
            );

            assert!(workbench.borrow_session_view_unweighed(&session_id));
            let applied = workbench.apply_timeline_events_to_borrowed_view(
                &session_id,
                vec![live_event(
                    &session_id,
                    2,
                    TimelinePayload::AgentMessage(AgentMessagePayload {
                        text: "done".into(),
                        is_final: true,
                    }),
                )],
                cx,
            );
            workbench.release_session_view_unweighed();
            assert!(applied.0, "the final answer reached the view");
            assert_eq!(
                workbench
                    .cached_auto_continue_turn_status(&session_id, revision)
                    .map(|status| status.ended_normally),
                Some(Some(true)),
                "a final answer settles the revision as a normal completion"
            );
        });
    });
}

#[gpui::test]
fn team_root_reconciliation_retains_unloaded_members_and_queued_mentions(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            group(workbench, 3);
            workbench.delegated_sessions.clear();
            workbench.selected_session_id = Some(session(1).id);
            workbench.sidebar_state.row_order =
                vec![session(0).id.to_string(), session(1).id.to_string()];
            let queued = queued_worker_message();
            workbench.composer_queue.push(queued.clone());
            workbench.composer_queue_restore_pending = true;
            workbench
                .pending_agent_turn_session_ids
                .insert(session(1).id.to_string());
            workbench.reconcile_sidebar_state();
            workbench.adopt_restored_composer_queue_if_ready(window, cx);
            assert_eq!(
                workbench
                    .ui_state
                    .sidebar
                    .organization
                    .group("team")
                    .unwrap()
                    .member_session_ids
                    .len(),
                3
            );
            assert_eq!(workbench.composer_queue, [queued]);
            assert_eq!(workbench.selected_session_id, Some(session(1).id));
            assert_eq!(
                workbench.sidebar_state.row_order,
                [session(0).id.to_string()]
            );
            assert!(
                workbench
                    .pending_agent_turn_session_ids
                    .contains(session(1).id.as_str())
            );
            assert!(
                !workbench
                    .ui_state
                    .sidebar
                    .organization
                    .placements
                    .iter()
                    .any(|placement| {
                        placement.item
                            == SidebarOrganizationItem::Session(session(1).id.to_string())
                    })
            );
            workbench.optimistically_remove_sessions(
                &BTreeSet::from([session(1).id.to_string()]),
                SidebarSessionRemoval::Delete,
            );
            assert!(workbench.composer_queue.is_empty());
            assert_eq!(
                workbench
                    .ui_state
                    .sidebar
                    .organization
                    .group("team")
                    .unwrap()
                    .member_session_ids
                    .len(),
                2
            );
        })
    });
}

#[gpui::test]
fn team_worker_revision_rearms_its_queue_and_keeps_its_lifecycle_state(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 2);
    workbench.update(cx, |workbench, cx| {
        workbench.composer_queue.push(queued_worker_message());
        workbench.auto_continue_turn_statuses.insert(
            session(1).id.to_string(),
            AutoContinueTurnStatus {
                session_updated_at_ms: 1,
                ended_normally: Some(true),
            },
        );
        let mut worker = session(1);
        worker.updated_at_ms = 10;
        worker.state = AgentSessionState::Running;
        workbench.upsert_session_snapshot(worker.clone());
        assert!(workbench.agent_session_is_active(&worker.id));
        assert!(
            workbench
                .composer_queue_ready_after_continuation_session_ids
                .contains(worker.id.as_str())
        );
        assert!(
            !workbench
                .auto_continue_turn_statuses
                .contains_key(worker.id.as_str())
        );
        assert!(!workbench.sessions.iter().any(|root| root.id == worker.id));
        worker.state = AgentSessionState::Idle;
        worker.updated_at_ms = 11;
        workbench.upsert_session_snapshot(worker.clone());
        assert!(workbench.cache_auto_continue_turn_status(&worker.id, 11, Some(true), cx));
        assert_eq!(
            workbench
                .cached_auto_continue_turn_status(&worker.id, 11)
                .unwrap()
                .ended_normally,
            Some(true)
        );
        assert!(!workbench.agent_session_is_active(&worker.id));
    });
}

/// Counts the timeline probes one session was asked for, draining the fixture's
/// observations so a test can watch them accumulate.
fn observed_timeline_probes(
    receiver: &sync_mpsc::Receiver<Observed>,
    session_id: &VibexSessionId,
) -> usize {
    let mut probes = 0;
    while let Ok(observed) = receiver.try_recv() {
        if matches!(&observed, Observed::Timeline(probed) if probed == session_id) {
            probes += 1;
        }
    }
    probes
}

/// The completion-probe attempts one session has spent on its revision.
fn probe_attempts(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    session_id: &VibexSessionId,
) -> u8 {
    workbench.read_with(cx, |workbench, _| {
        workbench
            .auto_continue_probe_attempts
            .get(session_id.as_str())
            .map_or(0, |attempt| attempt.attempts)
    })
}

/// Whether the completion probe for one session is still in flight.
fn probe_in_flight(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    session_id: &VibexSessionId,
) -> bool {
    workbench.read_with(cx, |workbench, _| {
        workbench
            .auto_continue_probe_tasks
            .contains_key(session_id.as_str())
    })
}

/// Drives the workbench's tasks until `ready` holds, the way
/// `TestAppContext::condition` does: a probe is a backend round trip, so its
/// completion lands after the spawn that made it.
fn pump_until(
    cx: &mut VisualTestContext,
    mut ready: impl FnMut(&mut VisualTestContext) -> bool,
) -> bool {
    for _ in 0..400 {
        cx.run_until_parked();
        if ready(cx) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// An enabled session whose completion probe cannot judge its revision asks
/// again, on a bounded budget.
///
/// A background session has no other reader: its live events are dropped when
/// no view shows it, so without the retry a failed turn stayed unjudged — and
/// the countdown only appeared once the reader selected the session and its
/// load reconciled the status.
#[gpui::test]
async fn auto_continue_probe_retries_an_unjudged_revision_up_to_its_budget(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let unfinished = session(1);
    workbench.update(cx, |workbench, cx| {
        workbench
            .auto_continue_session_ids
            .insert(unfinished.id.as_str().to_string());
        workbench.sync_auto_continue_for_session(&unfinished.id, cx);
    });
    assert!(
        probe_in_flight(&workbench, cx, &unfinished.id),
        "the sync asks the authority for an unjudged revision"
    );
    assert_eq!(
        probe_attempts(&workbench, cx, &unfinished.id),
        1,
        "the sync asks the authority once"
    );

    for attempt in 2..=AUTO_CONTINUE_PROBE_MAX_ATTEMPTS {
        // The attempt's answer is settled (and its retry armed) before the
        // backoff the retry waits on can be spent.
        assert!(
            pump_until(cx, |cx| !probe_in_flight(&workbench, cx, &unfinished.id)),
            "the attempt answers before its retry"
        );
        cx.executor()
            .advance_clock(AUTO_CONTINUE_PROBE_RETRY_BACKOFF);
        assert!(
            pump_until(cx, |cx| {
                probe_attempts(&workbench, cx, &unfinished.id) == attempt
            }),
            "an answer that cannot judge the revision is asked again"
        );
    }

    assert!(
        pump_until(cx, |cx| !probe_in_flight(&workbench, cx, &unfinished.id)),
        "the last attempt answers"
    );
    cx.executor()
        .advance_clock(AUTO_CONTINUE_PROBE_RETRY_BACKOFF * 4);
    std::thread::sleep(Duration::from_millis(50));
    cx.run_until_parked();
    assert_eq!(
        probe_attempts(&workbench, cx, &unfinished.id),
        AUTO_CONTINUE_PROBE_MAX_ATTEMPTS,
        "the attempt budget bounds the retry"
    );
    assert_eq!(
        observed_timeline_probes(&receiver, &unfinished.id),
        usize::from(AUTO_CONTINUE_PROBE_MAX_ATTEMPTS),
        "every attempt reads the authority's timeline"
    );
}

/// A verdict settles its revision; an unjudged one is probed rather than
/// latched, so the next event (or the probe's own retry) can still decide.
#[gpui::test]
async fn auto_continue_does_not_settle_a_revision_without_a_verdict(cx: &mut TestAppContext) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let unfinished = session(1);
    workbench.update(cx, |workbench, cx| {
        workbench
            .auto_continue_session_ids
            .insert(unfinished.id.as_str().to_string());
        workbench.auto_continue_turn_statuses.insert(
            unfinished.id.to_string(),
            AutoContinueTurnStatus {
                session_updated_at_ms: unfinished.updated_at_ms,
                ended_normally: None,
            },
        );
        workbench.sync_auto_continue_for_session(&unfinished.id, cx);
    });
    assert!(
        pump_until(cx, |_cx| {
            observed_timeline_probes(&receiver, &unfinished.id) >= 1
        }),
        "a cached answer with no verdict does not settle the revision"
    );
    assert!(
        workbench.read_with(cx, |workbench, _| {
            !workbench
                .auto_continue_countdowns
                .contains_key(unfinished.id.as_str())
        }),
        "an idle session with no verdict does not start a countdown"
    );
}

/// A session no view shows reconciles its turn boundary from the live batch
/// that carries it, because the authority need not publish a new session
/// revision for that boundary at all.
#[gpui::test]
async fn auto_continue_asks_about_a_background_turn_boundary_without_a_view(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let unfinished = session(1);
    workbench.update(cx, |workbench, cx| {
        workbench
            .auto_continue_session_ids
            .insert(unfinished.id.as_str().to_string());
        let payload = TimelinePayload::SystemNotice(SystemNoticePayload {
            level: SystemNoticeLevel::Info,
            message: "the turn stopped without an answer".into(),
        });
        workbench.apply_live_timeline_batch(
            vec![TimelineLiveEvent {
                session_id: unfinished.id.clone(),
                sequence: 1,
                item: TimelineItem {
                    id: TimelineItemId::parse("timeline_background_boundary").unwrap(),
                    session_id: unfinished.id.clone(),
                    sequence: 1,
                    timestamp_ms: 1,
                    source: TimelineSource::Provider,
                    kind: payload.kind(),
                    correlation_id: None,
                    provider_correlation_id: None,
                    redaction_state: TimelineRedactionState::None,
                    execution_attribution: None,
                    payload_depth: vibex_core::TimelinePayloadDepth::Summary,
                    payload,
                },
            }],
            cx,
        );
    });
    assert!(
        pump_until(cx, |_cx| {
            observed_timeline_probes(&receiver, &unfinished.id) >= 1
        }),
        "an unseen session's boundary asks the authority for its outcome"
    );
}

#[gpui::test]
async fn team_queued_session_recovery_uses_metadata_and_drops_only_confirmed_deletions(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    // One message, kept so the assertion compares against the entry the
    // recovery actually restored — including the key it submits with.
    let surviving = queued_worker_message();
    let surviving_for_queue = surviving.clone();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.delegated_sessions.clear();
            workbench.composer_queue.push(surviving_for_queue.clone());
            let mut deleted = queued_worker_message();
            deleted.id = 2;
            deleted.session_id = session(2).id;
            workbench.composer_queue.push(deleted);
            for id in [1, 2] {
                workbench.load_queued_session(
                    session(id).id,
                    ComposerQueueDispatchBehavior::Automatic,
                    window,
                    cx,
                );
            }
        })
    });
    cx.condition(&workbench, |workbench, _| {
        workbench.composer_queue_session_loads.is_empty()
    })
    .await;
    workbench.read_with(cx, |workbench, _| {
        assert!(
            workbench
                .registered_session(session(1).id.as_str())
                .is_some()
        );
        assert_eq!(workbench.composer_queue, std::slice::from_ref(&surviving));
        assert_eq!(workbench.sessions.len(), 1);
    });
    assert!(
        receiver.try_recv().is_err(),
        "recovering queued session identities does not open their timelines"
    );
}

#[gpui::test]
fn team_page_refresh_evicts_removed_descendant_branches(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, cx| {
        for node in [
            node(0, None, 2),
            node(1, Some(0), 1),
            node(2, Some(0), 0),
            node(3, Some(1), 0),
        ] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        for (parent, children) in [(0, vec![1, 2]), (1, vec![3]), (3, Vec::new())] {
            workbench.delegated_pages.insert(
                Some(session(parent).id.to_string()),
                DelegatedBranchPage {
                    session_ids: children
                        .into_iter()
                        .map(|id| session(id).id.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        }
        workbench.apply_delegated_page(
            Some(session(0).id.to_string()),
            false,
            SessionTreePage {
                nodes: vec![node(2, Some(0), 0)],
                ancestors: vec![node(0, None, 1)],
                registry: vec![session(0), session(2)],
                ..Default::default()
            },
            cx,
        );
        for id in [1, 3] {
            assert!(
                !workbench
                    .delegated_tree
                    .contains_key(session(id).id.as_str())
            );
            assert!(
                !workbench
                    .delegated_sessions
                    .contains_key(session(id).id.as_str())
            );
            assert!(
                !workbench
                    .delegated_pages
                    .contains_key(&Some(session(id).id.to_string()))
            );
        }
        assert_eq!(
            workbench.delegated_children(session(0).id.as_str()).len(),
            1
        );
    });
}

#[gpui::test]
fn team_tree_collapse_and_keyboard_order_follow_grouped_and_compact_rows(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, _| {
        for node in [
            node(0, None, 2),
            node(1, Some(0), 1),
            node(2, Some(0), 0),
            node(3, Some(1), 0),
        ] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        for (parent, children) in [(0, vec![1, 2]), (1, vec![3])] {
            workbench.delegated_pages.insert(
                Some(session(parent).id.to_string()),
                DelegatedBranchPage {
                    session_ids: children
                        .into_iter()
                        .map(|index| session(index).id.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        }
    });
    draw(cx);
    let expected: Vec<_> = [0, 1, 3, 2]
        .into_iter()
        .map(|index| session(index).id.to_string())
        .collect();
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        expected
    );
    // The toggle is painted only while its row is hovered, so the pointer has to
    // arrive before the button is there to click.
    let root_row = cx
        .debug_bounds("sidebar-session-row-session_team_0")
        .expect("the root session row is rendered");
    assert!(
        cx.debug_bounds("sidebar-delegated-toggle-session_team_0")
            .is_none(),
        "the branch toggle stays out of the row until the pointer arrives"
    );
    reveal_branch_toggle(cx, root_row.center());
    let disclosure = cx
        .debug_bounds("sidebar-delegated-toggle-session_team_0")
        .expect("hovering the row reveals its branch toggle");
    cx.simulate_click(disclosure.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        [session(0).id.to_string()]
    );
    cx.simulate_click(disclosure.center(), Modifiers::none());
    draw(cx);
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            window.focus(&workbench.delegated_row_focus[session(0).id.as_str()], cx);
        });
    });
    cx.simulate_keystrokes("down");
    cx.update(|window, cx| {
        assert!(workbench.read(cx).delegated_row_focus[session(1).id.as_str()].is_focused(window))
    });
    for hierarchy in [
        SidebarHierarchyMode::Compact,
        SidebarHierarchyMode::Detailed,
    ] {
        workbench.update(cx, |workbench, cx| {
            if workbench
                .ui_state
                .sidebar
                .organization
                .group("team")
                .is_none()
            {
                group(workbench, 4);
            }
            workbench.ui_state.sidebar.hierarchy_mode = hierarchy;
            workbench.invalidate_sidebar_projection_cache();
            cx.notify();
        });
        draw(cx);
        assert_eq!(
            workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
            expected
        );
    }
}

/// The branch a session row owns answers to a double click on the row itself.
///
/// The pair is a toggle rather than a fold-only verb: the same gesture opens the
/// branch again, and the single click inside the pair still selects the row, so
/// the gesture never costs the navigation the first click performs.
#[gpui::test]
fn double_clicking_a_session_row_toggles_its_branch(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, _| {
        for node in [
            node(0, None, 2),
            node(1, Some(0), 1),
            node(2, Some(0), 0),
            node(3, Some(1), 0),
        ] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        for (parent, children) in [(0, vec![1, 2]), (1, vec![3])] {
            workbench.delegated_pages.insert(
                Some(session(parent).id.to_string()),
                DelegatedBranchPage {
                    session_ids: children
                        .into_iter()
                        .map(|index| session(index).id.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
        }
    });
    draw(cx);
    let expected: Vec<_> = [0, 1, 3, 2]
        .into_iter()
        .map(|index| session(index).id.to_string())
        .collect();
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        expected
    );
    let root_row = cx
        .debug_bounds("sidebar-session-row-session_team_0")
        .expect("the root session row is rendered");
    double_click(cx, root_row.center());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        [session(0).id.to_string()],
        "a double click folds the branch under the row"
    );
    double_click(cx, root_row.center());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        expected,
        "the same gesture opens the branch again"
    );
    // A child row owns a branch of its own, so the gesture reads the same there.
    let child_row = cx
        .debug_bounds("sidebar-delegated-row-session_team_1")
        .expect("the delegated row is rendered");
    double_click(cx, child_row.center());
    draw(cx);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.delegated_visible_order()),
        [0, 1, 2].map(|index| session(index).id.to_string()),
        "a double click on a child row folds its own children"
    );
}

/// A delegated row is the session row's own shape one level down.
///
/// The branch's trunk leaves the parent row's Agent logo, its elbow lands on the
/// child's logo, and the child's status column ends on the parent's right edge —
/// the three things that make a delegated session read as a session parked under
/// its parent instead of a different kind of row.
#[gpui::test]
fn delegated_rows_branch_from_the_parent_logo_and_share_its_columns(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    workbench.update(cx, |workbench, _| {
        for node in [node(0, None, 2), node(1, Some(0), 0), node(2, Some(0), 0)] {
            workbench
                .delegated_tree
                .insert(node.session_ref.id.clone(), node);
        }
        workbench.delegated_pages.insert(
            Some(session(0).id.to_string()),
            DelegatedBranchPage {
                session_ids: vec![session(1).id.to_string(), session(2).id.to_string()],
                ..Default::default()
            },
        );
    });
    draw(cx);
    let parent_logo = cx
        .debug_bounds("sidebar-session-logo-session_team_0")
        .expect("the root row draws its Agent logo");
    let parent_status = cx
        .debug_bounds("sidebar-session-status-session_team_0")
        .expect("the root row keeps its status column");
    let child_logo = cx
        .debug_bounds("delegated-logo-session_team_1")
        .expect("the delegated row draws its Agent logo");
    let child_status = cx
        .debug_bounds("sidebar-delegated-status-session_team_1")
        .expect("the delegated row paints the session status column every row paints");
    let trunk = cx
        .debug_bounds("delegated-trunk-session_team_1-0")
        .expect("a child row draws the trunk of its own level");
    let elbow = cx
        .debug_bounds("delegated-elbow-session_team_1-0")
        .expect("a child row draws the elbow into its logo");

    assert!(
        (trunk.left() - parent_logo.center().x).abs() <= px(0.5),
        "the trunk leaves the center of the parent row's Agent logo: {trunk:?} vs {parent_logo:?}"
    );
    assert!(
        (elbow.right() - child_logo.left()).abs() <= px(0.5),
        "the elbow stops where the child's Agent logo begins: {elbow:?} vs {child_logo:?}"
    );
    assert!(
        (elbow.center().y - child_logo.center().y).abs() <= px(1.0),
        "the elbow meets the child's logo on its own line"
    );
    assert!(
        (child_status.right() - parent_status.right()).abs() <= px(0.5),
        "both rows end their status on one vertical line: {child_status:?} vs {parent_status:?}"
    );
    assert_eq!(
        child_status.size.width, parent_status.size.width,
        "a delegated row reserves the session row's status column"
    );

    // A branch's trunk carries on to the next sibling; the last child's stops at
    // its own elbow instead of running past the branch.
    let next_trunk = cx
        .debug_bounds("delegated-trunk-session_team_2-0")
        .expect("the last child still draws its elbow's trunk");
    let next_elbow = cx
        .debug_bounds("delegated-elbow-session_team_2-0")
        .expect("the last child draws its elbow");
    assert!(
        trunk.size.height > next_trunk.size.height,
        "the last child stops its trunk at the elbow"
    );
    assert!(
        next_trunk.bottom() <= next_elbow.center().y + px(1.0),
        "the last trunk never runs past the branch it closes"
    );
}

/// A row reports the session it stands for, whatever list draws it.
///
/// The delegated child rows resolve their state through the same projection the
/// navigation rows use, from the child's own live snapshot — never from the
/// ownership node, whose task phase moves when the delegation record does and
/// not when the session does. A task that failed and was then continued keeps
/// `Failed` while its session works again, and the row above and the row below
/// would otherwise describe one session two ways.
#[gpui::test]
fn a_row_reports_the_state_its_session_projection_resolves(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    let child = session(1).id.as_str().to_string();
    let mut working = session(1);
    working.state = AgentSessionState::Running;
    workbench.update(cx, |workbench, _| {
        let mut failed = node(1, Some(0), 0);
        failed.task_phase = Some(DelegationTaskPhase::Failed);
        workbench.delegated_tree.insert(child.clone(), failed);
        workbench
            .delegated_sessions
            .insert(child.clone(), working.clone());
    });
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench
            .sidebar_session_row_state(&working)),
        AgentSessionState::Running,
        "the row reads the session's own snapshot, not the task phase"
    );

    // The projections a session row folds in hold for every row: a local
    // dispatch is optimistic, and a request parked on the user outranks both it
    // and the still-open provider turn.
    let mut stopped = working.clone();
    stopped.state = AgentSessionState::Idle;
    workbench.update(cx, |workbench, _| {
        workbench
            .pending_agent_turn_session_ids
            .insert(child.clone());
    });
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench
            .sidebar_session_row_state(&stopped)),
        AgentSessionState::Running,
        "a dispatched turn shows progress before the authority admits it"
    );
    workbench.update(cx, |workbench, _| {
        workbench
            .pending_user_request_ids
            .insert(child.clone(), BTreeSet::new());
    });
    // A card that arrives mid-turn is the one case the live index must decide:
    // the turn keeps streaming, so the persisted state is still `Running`, and
    // the row has to ask for the answer rather than spin.
    let mut parked_on_card = stopped.clone();
    parked_on_card.state = AgentSessionState::Running;
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench
            .sidebar_session_row_state(&parked_on_card)),
        AgentSessionState::NeedsInput,
        "a request parked on the user is not progress"
    );
    // Once the session settles, the index entry is stale by definition — an
    // interrupted card is cancelled without a resolution item — so the row
    // reports the persisted state instead of asking about a card that is gone.
    workbench.update(cx, |workbench, _| {
        workbench.pending_agent_turn_session_ids.remove(&child);
    });
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench
            .sidebar_session_row_state(&stopped)),
        AgentSessionState::Idle,
        "a settled session cannot still be parked on an abandoned card"
    );
}

/// The authority's own settle ends a locally dispatched turn's optimism.
///
/// The optimistic flag covers the stretch between a local dispatch and the
/// authority admitting the turn, and it must not outlive the turn it speaks
/// for. It used to be cleared only when the submission's future returned, which
/// can trail the settle by the submission's own post-turn work — collapsing a
/// long turn's streamed rows is unbounded — so the sidebar kept painting a
/// finished turn as running while the row's persisted state already read idle.
#[gpui::test]
fn an_authoritative_settle_answers_the_dispatch_it_belongs_to(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    let mut idle = session(0);
    idle.state = AgentSessionState::Idle;
    idle.updated_at_ms = 50;
    let mut running = idle.clone();
    running.state = AgentSessionState::Running;
    running.updated_at_ms = 60;
    let mut settled = idle.clone();
    settled.updated_at_ms = 70;

    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(idle.clone());
        workbench.set_session_turn_pending(&session_id, true);
    });
    assert!(
        workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "a local dispatch shows progress before the authority admits it"
    );

    // A snapshot read before the dispatch still describes the revision the
    // dispatch started from, so it cannot be mistaken for the turn's settle.
    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(idle.clone());
    });
    assert!(
        workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "a snapshot from before the dispatch is not an answer to it"
    );

    // The authority admits the turn; the flag stays for as long as it runs.
    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(running.clone());
    });
    assert!(
        workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "an admitted turn keeps the flag while it runs"
    );

    // The turn settles while the submission that armed the flag is still busy,
    // and the row follows the authority instead of the submission.
    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(settled.clone());
    });
    assert!(
        !workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "the settle answers the dispatch, whatever its submission is still doing"
    );
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench
            .sidebar_session_row_state(&settled)),
        AgentSessionState::Idle,
        "the row reports the state the authority settled on"
    );
}

/// A dispatch armed from a turn that is already running is admitted at once.
///
/// A native steer joins a turn no local submission owns, so no submission
/// future would ever clear its flag: the turn's own settle is the only answer
/// it can get.
#[gpui::test]
fn a_dispatch_that_joins_a_running_turn_is_answered_by_that_turn(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    let mut running = session(0);
    running.state = AgentSessionState::Running;
    running.updated_at_ms = 30;
    let mut settled = running.clone();
    settled.state = AgentSessionState::Idle;
    settled.updated_at_ms = 40;

    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(running.clone());
        workbench.set_session_turn_pending(&session_id, true);
    });
    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(settled.clone());
    });
    assert!(
        !workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "the steered turn's settle is the answer the injection can get"
    );
}

/// A row the workbench never saw settle still reports the dispatch.
///
/// `Initializing` is the runtime being prepared, not a turn the authority
/// admitted, so the optimism the dispatch armed has not been answered yet.
#[gpui::test]
fn a_preparing_runtime_keeps_the_dispatch_optimism(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    let mut preparing = session(0);
    preparing.state = AgentSessionState::Initializing;
    preparing.updated_at_ms = 10;

    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(preparing.clone());
        workbench.set_session_turn_pending(&session_id, true);
        workbench.upsert_session_snapshot(preparing.clone());
    });
    assert!(
        workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "a runtime still being prepared has not answered the dispatch"
    );
}

/// A settle the workbench never saw admitted still answers the dispatch.
///
/// The turn can run and finish between two snapshots this client sees — a fast
/// turn, or a reader whose only session news is the authoritative list. The
/// settled snapshot the authority wrote after the dispatch is the turn's
/// boundary either way. Requiring an observed `Running` in between pinned the
/// flag forever, and because the flag holds the one global pre-send queue
/// fence, one such session froze every queued auto-send behind it.
#[gpui::test]
fn a_settle_without_an_observed_admission_answers_the_dispatch(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    let mut idle = session(0);
    idle.state = AgentSessionState::Idle;
    idle.updated_at_ms = 10;
    let mut settled = idle.clone();
    settled.updated_at_ms = 40;

    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(idle.clone());
        workbench.set_session_turn_pending(&session_id, true);
    });
    assert!(
        workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "the dispatch has not been answered by a snapshot from before it"
    );
    workbench.update(cx, |workbench, _| {
        workbench.upsert_session_snapshot(settled.clone());
    });
    assert!(
        !workbench.read_with(cx, |workbench, _| workbench
            .session_turn_pending(&session_id)),
        "the settle the authority wrote after the dispatch ends the optimism"
    );
}

#[gpui::test]
fn team_presentation_recovers_missing_groups_preserves_manual_layout_and_respects_authority(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 3);
    workbench.update(cx, |workbench, cx| {
        let mut command = GroupPresentationCommand {
            group_id: SessionGroupId::parse("group_team").unwrap(),
            operation_id: None,
            name: "Team".into(),
            workspace_ref: VibexUseRef::new(VibexUseResourceKind::Workspace, "workspace_team"),
            member_session_refs: vec![
                VibexUseRef::session(&session(0).id),
                VibexUseRef::session(&session(1).id),
            ],
            layout: SessionGroupLayoutIntent::default(),
            expected_revision: None,
            activation_policy: PresentationActivationPolicy::IfCurrentTeam,
            focus_session_ref: Some(VibexUseRef::session(&session(1).id)),
            present: true,
            presentation_only: true,
        };
        workbench.selected_session_id = Some(session(2).id);
        let result = workbench.apply_group_presentation(&command, cx).unwrap();
        assert_eq!(result.state, PresentationState::Deferred);
        assert!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group("group_team")
                .is_some()
        );
        assert_eq!(workbench.selected_session_id, Some(session(2).id));
        workbench.selected_session_id = Some(session(0).id);
        workbench.apply_group_presentation(&command, cx).unwrap();
        assert_eq!(workbench.selected_session_id, Some(session(1).id));
        workbench
            .ui_state
            .sidebar
            .organization
            .group_mut("group_team")
            .unwrap()
            .merge_panes();
        workbench.sync_user_session_group("group_team");
        let layout = workbench
            .ui_state
            .sidebar
            .organization
            .group("group_team")
            .unwrap()
            .layout
            .clone();
        command.layout.preset = SessionGroupLayoutPreset::Grid;
        command.focus_session_ref = None;
        workbench.apply_group_presentation(&command, cx).unwrap();
        assert_eq!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group("group_team")
                .unwrap()
                .layout,
            layout
        );
        workbench.ui_state.sidebar.switch_authority("server:other");
        assert_eq!(
            workbench
                .apply_group_presentation(&command, cx)
                .unwrap()
                .reason,
            Some(VibexUseUnavailableReason::ForeignAuthority)
        );
        assert!(workbench.ui_state.sidebar.organization.groups.is_empty());
    });
}

/// A restored entry whose submission already reached the provider is dropped.
///
/// The queue's removal is persisted through a throttled write, so an entry can
/// outlive its own send. Replaying it would either send the message twice or —
/// now that the entry carries its key — be refused as a payload conflict. The
/// adoption pass asks the authority instead, and only a record that may have
/// reached a provider is dropped; a failed or pending submission stays queued
/// so its own worker can finish it.
#[gpui::test]
async fn a_restored_entry_the_authority_already_accepted_is_not_replayed(cx: &mut TestAppContext) {
    use vibex_core::{MessageSubmissionStatus, SendAgentMessageRequest};

    let (workbench, cx, _) = fixture(cx, 1);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let coordinator = runtime
        .agent()
        .manager()
        .message_submission_coordinator()
        .expect("the isolated runtime installs a coordinator");
    let session_id = session(0).id.clone();

    // One message that really was sent, and one whose send never landed.
    let delivered = head_queue_message(1, &session_id, None);
    let pending = head_queue_message(2, &session_id, None);
    {
        let mut conn = open_database(&runtime.config().database_path).unwrap();
        for message in [&delivered, &pending] {
            vibex_db::MessageSubmissionRepository::enqueue(
                &mut conn,
                MessageSubmissionId::new(),
                &SendAgentMessageRequest {
                    session_id: session_id.clone(),
                    message_idempotency_key: message.dispatch_key.clone(),
                    desired_runtime: message.desired_runtime.clone(),
                    text: message.text.clone(),
                    attachments: Vec::new(),
                    reasoning_effort: message.desired_runtime.reasoning_effort.clone(),
                    correlation_id: None,
                    delivery: vibex_core::UserMessageDelivery::Prompt,
                    prompt_context: None,
                    provenance: vibex_core::MessageProvenance::HumanInput,
                    mentions: Vec::new(),
                },
            )
            .unwrap();
        }
        let record = vibex_db::MessageSubmissionRepository::get_by_key(
            &conn,
            &session_id,
            &delivered.dispatch_key,
        )
        .unwrap()
        .unwrap();
        // Walk the submission to the one state that means "the provider may
        // already have this": a replay of it would be a second turn.
        for (from, to) in [
            (
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            ),
            (
                MessageSubmissionStatus::ReadyToDispatch,
                MessageSubmissionStatus::AboutToPrompt,
            ),
            (
                MessageSubmissionStatus::AboutToPrompt,
                MessageSubmissionStatus::AmbiguousPromptDispatch,
            ),
        ] {
            vibex_db::MessageSubmissionRepository::advance_status(
                &conn,
                &record.submission_id,
                from,
                to,
            )
            .unwrap();
        }
    }
    assert!(
        coordinator
            .get_submission(&GetMessageSubmissionRequest {
                session_id: session_id.clone(),
                message_idempotency_key: delivered.dispatch_key.clone(),
            })
            .is_ok()
    );

    cx.update(|_window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.composer_queue = vec![delivered.clone(), pending.clone()];
            workbench.prune_composer_queue_against_submissions(cx);
            assert_eq!(
                workbench.composer_queue,
                vec![pending],
                "only the entry whose send reached the provider is dropped"
            );
        });
    });
}

#[gpui::test]
async fn team_batch_group_moves_and_removals_update_every_durable_definition(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 6);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let conn = open_database(&runtime.config().database_path).unwrap();
    let members = |indexes: &[usize]| {
        indexes
            .iter()
            .map(|index| session(*index).id)
            .collect::<Vec<_>>()
    };
    let session_ids = |indexes: &[usize]| {
        members(indexes)
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
    };
    workbench.update(cx, |workbench, _| {
        let (project, workspace) = &workbench.workspaces[0];
        let session_workspaces = workbench.sidebar_session_workspaces();
        for (group_id, indexes) in [
            ("group_destination", vec![0]),
            ("group_source", vec![1, 2]),
            ("group_emptied", vec![3]),
            ("group_untouched", vec![4, 5]),
        ] {
            GroupPresentationRepository::reserve_or_get(
                &conn,
                None,
                "session:team",
                group_id,
                &SessionGroupScope::Workspace {
                    workspace_ref: VibexUseRef::new(
                        VibexUseResourceKind::Workspace,
                        workspace.id.as_str(),
                    ),
                },
                group_id,
                &members(&indexes),
                &SessionGroupLayoutIntent::default(),
                group_id,
            )
            .unwrap();
            assert!(workbench.ui_state.sidebar.organization.create_group(
                group_id,
                group_id,
                project.id.as_str(),
                workspace.id.as_str(),
                &session_ids(&indexes),
                &session_workspaces,
                None,
            ));
            workbench
                .ui_state
                .sidebar
                .organization
                .group_mut(group_id)
                .unwrap()
                .set_automatic_layout(SessionGroupLayoutIntent::default(), 80.0);
        }
    });
    let revision = workbench.update(cx, |workbench, cx| {
        workbench.add_sessions_to_group_from_menu("group_destination", &session_ids(&[1, 3]), cx);
        assert!(workbench.persistence_note.is_none());
        workbench.sidebar_organization_view().revision
    });
    for (group_id, expected) in [
        ("group_destination", vec![0, 1, 3]),
        ("group_source", vec![2]),
    ] {
        let saved = GroupPresentationRepository::get(&conn, group_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.member_session_ids, members(&expected));
        assert!(!saved.created_by_caller);
        assert_eq!(saved.client_revision, Some(revision));
    }
    assert!(
        GroupPresentationRepository::get(&conn, "group_emptied")
            .unwrap()
            .is_none()
    );
    let untouched = GroupPresentationRepository::get(&conn, "group_untouched")
        .unwrap()
        .unwrap();
    assert_eq!(untouched.member_session_ids, members(&[4, 5]));
    assert!(untouched.created_by_caller);
    assert_eq!(untouched.client_revision, None);

    let revision = workbench.update(cx, |workbench, cx| {
        workbench.remove_sessions_from_groups_from_menu(&session_ids(&[0, 1, 2, 4]), cx);
        assert!(workbench.persistence_note.is_none());
        workbench.sidebar_organization_view().revision
    });
    for (group_id, expected) in [("group_destination", vec![3]), ("group_untouched", vec![5])] {
        let saved = GroupPresentationRepository::get(&conn, group_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.member_session_ids, members(&expected));
        assert!(!saved.created_by_caller);
        assert_eq!(saved.client_revision, Some(revision));
    }
    assert!(
        GroupPresentationRepository::get(&conn, "group_source")
            .unwrap()
            .is_none()
    );

    // Matching ids on another authority cannot remove the local definitions.
    workbench.update(cx, |workbench, _| {
        workbench.ui_state.sidebar.switch_authority("server:other");
        workbench.sync_user_session_groups(&BTreeSet::from([
            "group_destination".to_string(),
            "group_untouched".to_string(),
        ]));
    });
    assert!(
        GroupPresentationRepository::get(&conn, "group_destination")
            .unwrap()
            .is_some()
    );
    assert!(
        GroupPresentationRepository::get(&conn, "group_untouched")
            .unwrap()
            .is_some()
    );
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[gpui::test]
fn team_capacity_rejected_transfers_keep_source_group_ownership(cx: &mut TestAppContext) {
    let limit = vibex_desktop_model::SESSION_GROUP_MEMBER_LIMIT;
    let (workbench, cx, _) = fixture(cx, limit + 2);
    workbench.update(cx, |workbench, cx| {
        group(workbench, limit);
        let session_workspaces = workbench.sidebar_session_workspaces();
        for (group_id, index) in [("accepted", limit), ("rejected", limit + 1)] {
            assert!(workbench.ui_state.sidebar.organization.create_group(
                group_id,
                group_id,
                session(0).project_id.as_str(),
                session(0).workspace_id.as_str(),
                &[session(index).id.to_string()],
                &session_workspaces,
                None,
            ));
            workbench
                .ui_state
                .sidebar
                .organization
                .group_mut(group_id)
                .unwrap()
                .set_automatic_layout(SessionGroupLayoutIntent::default(), 80.0);
        }
        let source_groups = workbench.ui_state.sidebar.organization.clone();
        let moving = [
            session(limit).id.to_string(),
            session(limit + 1).id.to_string(),
        ];
        workbench.add_sessions_to_group_from_menu("team", &moving, cx);
        assert_eq!(workbench.ui_state.sidebar.organization, source_groups);

        assert!(
            workbench
                .ui_state
                .sidebar
                .organization
                .remove_sessions_from_group("team", &[session(limit - 1).id.to_string()])
        );
        workbench.add_sessions_to_group_from_menu("team", &moving, cx);
        assert!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group("accepted")
                .is_none()
        );
        assert_eq!(
            workbench
                .ui_state
                .sidebar
                .organization
                .group_of_session(&moving[0]),
            Some("team"),
        );
        assert_eq!(
            workbench.ui_state.sidebar.organization.group("rejected"),
            source_groups.group("rejected"),
            "a capacity rejection must not transfer ownership of the source group",
        );
    });
}

/// The Archived page has to answer the question an empty list raises: is the
/// inactivity window running at all, and how far is the oldest session from
/// it? The pass is silent by design, so the page reads the window's own
/// report — including the batch a narrowed window retires in one go.
#[gpui::test]
fn auto_archive_status_explains_the_window_and_the_last_check(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 3);
    workbench.update(cx, |workbench, cx| {
        let now_ms = 40 * AUTO_ARCHIVE_MILLIS_PER_DAY;
        let mut oldest = session(0);
        oldest.created_at_ms = 0;
        oldest.last_message_at_ms = 0;
        let mut recent = session(1);
        recent.created_at_ms = 38 * AUTO_ARCHIVE_MILLIS_PER_DAY;
        recent.last_message_at_ms = recent.created_at_ms;
        let mut pinned = session(2);
        pinned.last_message_at_ms = 0;
        let mut sessions = vec![oldest, recent, pinned.clone()];
        for index in 3..15 {
            let mut aged = session(index);
            aged.created_at_ms = 0;
            aged.last_message_at_ms = 0;
            sessions.push(aged);
        }
        workbench.sessions = sessions;
        workbench.sidebar_state.pinned_ids = BTreeSet::from([pinned.id.to_string()]);

        // Nothing to report while the window is off.
        assert_eq!(workbench.auto_archive_status(now_ms), None);

        // A pinned session is a deliberate keep, so it is not what the window
        // reports as waiting: the line names the oldest session the window can
        // still act on, which is what tells the reader the window has simply
        // not been reached yet.
        workbench.ui_state.session.auto_archive_after_days = Some(30);
        let status = workbench
            .auto_archive_status(now_ms)
            .expect("an on window reports");
        assert_eq!(
            status,
            AutoArchiveStatus {
                window_days: 30,
                eligible_sessions: 14,
                oldest_idle_days: Some(40),
                last_pass: None,
            }
        );
        let label = auto_archive_status_label(&status);
        assert!(label.contains("40") && label.contains("30"), "{label}");

        // Narrowing the window retires the backlog in one pass, and the page
        // names that batch instead of leaving the reader to notice the rows.
        workbench.ui_state.session.auto_archive_after_days = Some(7);
        assert!(workbench.apply_auto_archive_sessions(now_ms, cx));
        let status = workbench
            .auto_archive_status(now_ms)
            .expect("an on window reports");
        assert_eq!(
            status
                .last_pass
                .map(|pass| (pass.window_days, pass.archived)),
            Some((7, 13))
        );
        let label = auto_archive_status_label(&status);
        assert!(label.contains("13") && label.contains('7'), "{label}");

        // The batch belongs to the window that produced it: a reader who
        // widens the window again is back to "nothing archived yet" rather
        // than reading a count that no longer describes this policy.
        workbench.ui_state.session.auto_archive_after_days = Some(14);
        let status = workbench
            .auto_archive_status(now_ms)
            .expect("an on window reports");
        assert_eq!(status.last_pass, None);
    });
}

#[gpui::test]
async fn team_root_listing_and_worker_updates_keep_automatic_archive_off_unloaded_workers(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 2);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let conn = open_database(&runtime.config().database_path).unwrap();
    SessionOwnershipRepository::upsert(&conn, &session(1).id, &session(0).id, None).unwrap();
    let mut worker = SessionRepository::get(&conn, &session(1).id)
        .unwrap()
        .unwrap();
    let backend = Arc::new(NativeBackend::new(runtime.clone())).facade();
    let roots = cx
        .update(|_, cx| {
            gpui_tokio::Tokio::spawn(cx, async move {
                backend.agent().list_sessions(false).await.unwrap()
            })
        })
        .await
        .unwrap();
    assert_eq!(
        roots.iter().map(|session| &session.id).collect::<Vec<_>>(),
        [&session(0).id]
    );
    workbench.update(cx, |workbench, cx| {
        workbench.sessions = roots;
        workbench.delegated_sessions.clear();
        assert!(workbench.delegated_tree.is_empty());
        assert!(workbench.delegated_pages.is_empty());
        let now_ms = 40 * AUTO_ARCHIVE_MILLIS_PER_DAY;
        assert!(workbench.auto_archive_candidates(now_ms).is_empty());
        workbench.ui_state.session.auto_archive_after_days = Some(30);
        assert_eq!(workbench.auto_archive_candidates(now_ms), [session(0).id]);

        // The first broadcast arrives before any ownership page. Later
        // broadcasts update that registry entry without promoting it to a root.
        for revision in [2, 3] {
            worker.updated_at_ms = revision;
            workbench.apply_desktop_event(DesktopEvent::SessionUpdated(worker.clone()), cx);
            assert_eq!(workbench.sessions.len(), 1);
            assert_eq!(
                workbench.registered_session(worker.id.as_str()),
                Some(&worker)
            );
            assert_eq!(workbench.auto_archive_candidates(now_ms), [session(0).id]);
            assert!(workbench.delegated_tree.is_empty());
        }
        assert!(workbench.delegated_roots_dirty);
    });
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[gpui::test]
fn team_background_page_refresh_never_borrows_the_loading_row(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            // Ownership pages only load on an authority that can read the tree.
            workbench
                .backend
                .as_ref()
                .expect("fixture backend")
                .replace_capabilities(BackendCapabilitySnapshot::desktop_native_v1());
            workbench.delegated_pages.insert(
                None,
                DelegatedBranchPage {
                    session_ids: vec![session(1).id.to_string()],
                    next_cursor: Some("cursor_1".into()),
                    ..Default::default()
                },
            );

            // The periodic pass fetches silently: the sidebar's "Loading
            // sessions" row belongs to a reader who asked for the page, not to
            // a poll that runs every few seconds.
            workbench.load_delegated_page(None, false, DelegatedPageLoad::Background, cx);
            let page = &workbench.delegated_pages[&None];
            assert!(!page.loading, "a background pass must not claim the row");
            assert!(page.in_flight, "the pass still dedupes against itself");

            // An explicit load keeps the row it always had.
            workbench.delegated_pages.get_mut(&None).unwrap().in_flight = false;
            workbench.load_delegated_page(None, false, DelegatedPageLoad::Interactive, cx);
            assert!(workbench.delegated_pages[&None].loading);

            // A failed background pass over rows that are already on screen
            // stays quiet: alternating retry and data every few seconds is the
            // flicker this split exists to remove. An empty page and an
            // explicit load may still claim the retry row.
            workbench.delegated_pages.get_mut(&None).unwrap().in_flight = false;
            workbench.record_delegated_page_failure(
                &None,
                DelegatedPageLoad::Background,
                "offline".into(),
            );
            assert!(workbench.delegated_pages[&None].error.is_none());
            workbench.record_delegated_page_failure(
                &None,
                DelegatedPageLoad::Interactive,
                "offline".into(),
            );
            assert_eq!(
                workbench.delegated_pages[&None].error.as_deref(),
                Some("offline")
            );
            let empty = workbench.delegated_pages.get_mut(&None).unwrap();
            empty.error = None;
            empty.session_ids.clear();
            workbench.record_delegated_page_failure(
                &None,
                DelegatedPageLoad::Background,
                "offline".into(),
            );
            assert_eq!(
                workbench.delegated_pages[&None].error.as_deref(),
                Some("offline"),
                "a branch with nothing to show still reports its failure"
            );
        });
    });
}

/// The pager is not a sidebar footer.
///
/// The root page pages ownership metadata for sessions the sidebar already
/// lists, so it draws no row of its own: a full-width "Load more sessions"
/// button under the project list read as a paged session list and did nothing
/// visible when clicked. A branch still pages under the row that owns it, and
/// a branch the branch limit refused is answered in the same place.
#[gpui::test]
fn team_delegated_pager_stays_under_its_branch(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 4);
    workbench.update(cx, |workbench, _| {
        workbench
            .delegated_tree
            .insert(session(1).id.to_string(), node(1, Some(0), 0));
        workbench.delegated_pages.insert(
            Some(session(0).id.to_string()),
            DelegatedBranchPage {
                session_ids: vec![session(1).id.to_string()],
                next_cursor: Some("cursor_branch".into()),
                ..Default::default()
            },
        );
    });
    draw(cx);
    assert!(
        cx.debug_bounds("delegated-page-session_team_0").is_some(),
        "a branch still pages under its own row"
    );

    // The branch limit refuses a branch by name, and the reader who asked for
    // it is told under that row instead of in a sidebar footer.
    workbench.update(cx, |workbench, cx| {
        workbench
            .delegated_pages
            .remove(&Some(session(0).id.to_string()));
        workbench.delegated_deferred_parent = Some(session(0).id.to_string());
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds("delegated-page-session_team_0").is_some(),
        "the refusal belongs under the row that asked for the branch"
    );
}

/// One root page per background pass, with no button to press.
///
/// The pass used to re-read page one forever, so a tree with more roots than a
/// page kept the cursor set and the sidebar kept offering to load rows that
/// were already on screen. Following the cursor is what retires that row and
/// what puts the ownership metadata behind the later roots in place.
#[gpui::test]
async fn team_root_page_advances_in_the_background_without_a_footer_pager(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, DELEGATED_TREE_PAGE_SIZE + 1);
    let (runtime, _home) = attach_team_runtime(&workbench, cx).await;
    let backend = Arc::new(NativeBackend::new(runtime.clone())).facade();
    workbench.update(cx, |workbench, cx| {
        workbench.backend = Some(backend);
        // The fixture's rows are all ownership roots in the runtime's database,
        // and the root list is what a root's row is drawn from. Adopting it
        // first keeps this test about paging the ownership page rather than
        // about a list that still has to be re-read for rows it never met.
        let mut roots = workbench.sessions.clone();
        roots.extend(workbench.delegated_sessions.values().cloned());
        roots.sort_by(|left, right| left.id.cmp(&right.id));
        workbench.sessions = roots;
        workbench.delegated_sessions.clear();
        workbench.refresh_delegated_sessions(cx);
    });
    let root_page_settled = |workbench: &VibexWorkbench| {
        workbench
            .delegated_pages
            .get(&None)
            .is_some_and(|page| !page.in_flight && !page.session_ids.is_empty())
    };
    cx.condition(&workbench, |workbench, _| root_page_settled(workbench))
        .await;
    workbench.read_with(cx, |workbench, _| {
        let page = &workbench.delegated_pages[&None];
        assert_eq!(page.session_ids.len(), DELEGATED_TREE_PAGE_SIZE);
        assert!(page.next_cursor.is_some(), "one root is still unread");
    });
    // The state the footer used to speak from: one page in hand, one to go.
    draw(cx);
    assert!(
        cx.debug_bounds("delegated-page-roots").is_none(),
        "an unspent root cursor draws no sidebar footer"
    );
    workbench.update(cx, |workbench, cx| workbench.refresh_delegated_sessions(cx));
    cx.condition(&workbench, |workbench, _| {
        root_page_settled(workbench) && workbench.delegated_pages[&None].next_cursor.is_none()
    })
    .await;
    workbench.read_with(cx, |workbench, _| {
        assert_eq!(
            workbench.delegated_pages[&None].session_ids.len(),
            DELEGATED_TREE_PAGE_SIZE + 1,
            "the next pass reads the page the cursor points at"
        );
    });
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[gpui::test]
fn team_hidden_tabs_materialize_only_the_four_live_conversations(cx: &mut TestAppContext) {
    let (workbench, cx, receiver) = fixture(cx, 64);
    workbench.update(cx, |workbench, cx| {
        group(workbench, 64);
        workbench
            .ui_state
            .sidebar
            .organization
            .group_mut("team")
            .unwrap()
            .apply_layout_preset(SessionGroupLayoutPreset::Columns, None, Some(4));
        workbench.ensure_session_group_views("team", cx);
        workbench.ensure_session_group_views("team", cx);
    });
    let mut loaded = BTreeSet::new();
    for _ in 0..4 {
        let Observed::Timeline(id) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
            panic!("expected a timeline fetch");
        };
        loaded.insert(id);
    }
    assert_eq!(loaded, (0..4).map(|index| session(index).id).collect());
    assert!(receiver.try_recv().is_err());
    cx.run_until_parked();
    workbench.update(cx, |workbench, cx| {
        let group = workbench
            .ui_state
            .sidebar
            .organization
            .group_mut("team")
            .unwrap();
        let pane = group
            .layout
            .pane_containing_session(session(63).id.as_str())
            .unwrap();
        group.layout.focus_session(&pane, session(63).id.as_str());
        group.focus_pane(&pane);
        workbench.record_composer_geometry(Some(session(3).id.as_str()), |geometry| {
            geometry.input_bounds = Some(Bounds::new(
                point(px(10.0), px(20.0)),
                size(px(30.0), px(40.0)),
            ));
        });
        workbench.selected_session_id = Some(session(63).id);
        workbench.borrow_session_view(&session(63).id);
        workbench.ensure_session_group_views("team", cx);
        let mut resident: BTreeSet<_> = workbench.session_views.keys().cloned().collect();
        resident.extend(workbench.view_session_id.as_ref().map(ToString::to_string));
        assert_eq!(resident.len(), 4);
        assert!(resident.contains(session(63).id.as_str()));
        assert!(!resident.contains(session(3).id.as_str()));
        assert!(
            !workbench
                .pending_composer_geometries
                .contains_key(session(3).id.as_str())
        );
    });
    let Observed::Timeline(id) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected lazy tab fetch");
    };
    assert_eq!(id, session(63).id);
}

#[gpui::test]
fn team_view_removal_and_eviction_discard_only_their_composer_measurements(
    cx: &mut TestAppContext,
) {
    let count = AGENT_SESSION_VIEW_CACHE_LIMIT + 2;
    let (workbench, cx, _) = fixture(cx, count);
    workbench.update(cx, |workbench, _| {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(30.0), px(40.0)));
        for index in 0..3 {
            workbench.ensure_session_view(&session(index).id);
            workbench.record_composer_geometry(Some(session(index).id.as_str()), |geometry| {
                geometry.input_bounds = Some(bounds);
            });
        }
        workbench.optimistically_remove_sessions(
            &BTreeSet::from([session(2).id.to_string()]),
            SidebarSessionRemoval::Delete,
        );
        assert!(
            !workbench
                .pending_composer_geometries
                .contains_key(session(2).id.as_str())
        );
        assert!(
            workbench
                .pending_composer_geometries
                .contains_key(session(1).id.as_str())
        );
        for index in 2..count {
            workbench.ensure_session_view(&session(index).id);
        }
        assert!(!workbench.session_views.contains_key(session(1).id.as_str()));
        assert!(
            !workbench
                .pending_composer_geometries
                .contains_key(session(1).id.as_str())
        );
        assert!(
            workbench
                .pending_composer_geometries
                .contains_key(session(0).id.as_str())
        );
        workbench.adopt_composer_geometry();
        assert_eq!(workbench.composer_geometry.input_bounds, Some(bounds));
    });
}

#[gpui::test]
fn team_composer_and_permissions_keep_the_borrowed_panes_session_and_runtime(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let mention = VibexUseMention::session(&session(2).id, Some("@Reviewer"));
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            assert!(
                workbench
                    .registered_session(session(2).id.as_str())
                    .is_none()
            );
            workbench.borrow_session_view(&session(1).id);
            workbench.runtime_selection = Some(selection_state("worker-model"));
            let input = workbench.ensure_composer_input(window, cx);
            input.update(cx, |input, cx| {
                input.set_value(
                    composer_content_with_mentions("Ask @Reviewer", std::slice::from_ref(&mention)),
                    window,
                    cx,
                )
            });
            let message = workbench.take_composer_message_remote(window, cx).unwrap();
            assert_eq!(message.session_id, session(1).id);
            assert_eq!(message.desired_runtime.model_id(), Some("worker-model"));
            workbench.dispatch_composer_message_remote(
                message,
                workbench.backend.clone().unwrap(),
                UserMessageDelivery::Prompt,
                window,
                cx,
            );
            workbench.resolve_permission(
                session(1).id,
                RequestId::new().to_string(),
                PermissionResponseKind::Approve,
                None,
                cx,
            );
        })
    });
    let mut sent = None;
    let mut permission = None;
    while sent.is_none() || permission.is_none() {
        match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            Observed::Message(message) => sent = Some(message),
            Observed::Permission(request) => permission = Some(request),
            Observed::Timeline(_) => {}
            Observed::Delegation(_) => panic!("unexpected delegation"),
        }
    }
    assert_eq!(sent.unwrap().mentions, [mention]);
    assert_eq!(permission.unwrap().session_id, session(1).id);
    assert_eq!(
        workbench.read_with(cx, |workbench, _| workbench.selected_session_id.clone()),
        Some(session(0).id)
    );
}

#[gpui::test]
fn team_message_copy_paste_and_inline_edit_preserve_only_selected_references(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, _) = fixture(cx, 2);
    let mention = VibexUseMention::session(&session(1).id, Some("@Reviewer"));
    let text = "Ask @Reviewer, then @PlainName";
    let clipboard =
        user_message_clipboard_item_with_mentions(text, &[], std::slice::from_ref(&mention));
    let ClipboardEntry::String(value) = &clipboard.entries()[0] else {
        panic!("text clipboard");
    };
    let metadata = value.metadata_json::<VibexMessageClipboard>().unwrap();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            assert!(workbench.add_vibex_message_clipboard_to(metadata, false, window, cx));
            let input = workbench.ensure_composer_input(window, cx);
            assert_eq!(
                composer_mentions(input.read(cx).tokens()),
                std::slice::from_ref(&mention)
            );
            workbench.begin_inline_user_message_edit(
                "turn".into(),
                "row".into(),
                session(0).id,
                1,
                1,
                text.into(),
                Vec::new(),
                vec![mention.clone()],
                window,
                cx,
            );
            assert_eq!(
                composer_mentions(workbench.user_message_edit_input.read(cx).tokens()),
                std::slice::from_ref(&mention)
            );
            let range = workbench.user_message_edit_input.read(cx).tokens()[0].range();
            workbench.user_message_edit_input.update(cx, |input, cx| {
                input.set_selected_range(range, cx);
                input.replace("@PlainName", window, cx);
            });
            assert!(
                composer_mentions(workbench.user_message_edit_input.read(cx).tokens()).is_empty()
            );
        })
    });
}

#[gpui::test]
async fn team_immediate_delegation_keeps_context_and_retries_the_same_gesture(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    let mention = VibexUseMention::session(&session(1).id, Some("@Reviewer"));
    let option =
        VibexUseRef::new(VibexUseResourceKind::RuntimeOption, "runtime-option-test").as_uri();
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let input = workbench.ensure_composer_input(window, cx);
            input.update(cx, |input, cx| {
                input.set_value(
                    composer_content_with_mentions(
                        "Review @Reviewer",
                        std::slice::from_ref(&mention),
                    ),
                    window,
                    cx,
                )
            });
            workbench.add_composer_path_to(
                std::path::Path::new("/team-test/context.md"),
                None,
                false,
                window,
                cx,
            );
            workbench.start_immediate_delegation("codex".into(), option.clone(), window, cx);
            workbench.start_immediate_delegation("codex".into(), option.clone(), window, cx);
        })
    });
    let Observed::Delegation(first) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected delegation");
    };
    assert_eq!(first.payload.parent_session_id, session(0).id);
    assert_eq!(first.payload.mentions, [mention]);
    assert_eq!(first.payload.attachments.len(), 1);
    assert_eq!(first.payload.context_refs[0].session_id, session(1).id);
    assert!(
        receiver.try_recv().is_err(),
        "a pending gesture cannot be submitted twice"
    );
    cx.condition(&workbench, |workbench, _| {
        !workbench.composer_delegations[session(0).id.as_str()].pending
    })
    .await;
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.start_immediate_delegation("codex".into(), option.clone(), window, cx);
        })
    });
    let Observed::Delegation(retry) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected retry");
    };
    assert_eq!(first.idempotency_key, retry.idempotency_key);
    cx.condition(&workbench, |workbench, _| {
        !workbench.composer_delegations[session(0).id.as_str()].pending
    })
    .await;
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            let input = workbench.ensure_composer_input(window, cx);
            input.update(cx, |input, cx| {
                input.set_value("A different task", window, cx)
            });
            workbench.start_immediate_delegation("codex".into(), option, window, cx);
        })
    });
    let Observed::Delegation(changed) = receiver.recv_timeout(Duration::from_secs(5)).unwrap()
    else {
        panic!("expected changed task");
    };
    assert_ne!(first.idempotency_key, changed.idempotency_key);
}

/// An archive that arrives from another client takes the row off the sidebar.
///
/// The reader's own archive removes the row optimistically and reloads a root
/// list that filters `archived_at_ms IS NULL` in SQL, so the session is gone by
/// construction. A session the TUI archived reaches this workbench through no
/// list at all: it arrives as a live `SessionUpdated` snapshot, which used to be
/// accepted unconditionally and drawn with an "Archived" badge. The snapshot now
/// answers the same visibility question the authoritative list answers, and the
/// row leaves the way a local archive takes it off.
#[gpui::test]
fn an_archive_from_another_client_takes_the_row_off_the_sidebar(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 2);
    workbench.update(cx, |workbench, cx| {
        assert!(
            workbench
                .sessions
                .iter()
                .any(|root| root.id == session(0).id),
            "the fixture should start with the root row on the sidebar"
        );

        let mut archived = session(0);
        archived.state = AgentSessionState::Archived;
        archived.archived_at_ms = Some(42);
        archived.updated_at_ms = 42;
        workbench.apply_desktop_event(DesktopEvent::SessionUpdated(archived), cx);
        assert!(
            !workbench
                .sessions
                .iter()
                .any(|root| root.id == session(0).id),
            "an archived snapshot must take the row off the root list"
        );
        assert!(
            workbench
                .registered_session(session(0).id.as_str())
                .is_none(),
            "the archived row must not be parked as a delegated one either"
        );
        assert!(
            !workbench
                .sidebar_workspace_groups("")
                .iter()
                .flat_map(|group| group.compact_sessions.iter())
                .any(|listed| listed.id == session(0).id),
            "the sidebar must not draw a row the reader archived elsewhere"
        );
        // Archiving is not deleting: no deletion tombstone is involved, because
        // the runtime still holds the session for the archived page to restore.
        assert!(
            !workbench
                .deleted_session_ids
                .contains(session(0).id.as_str())
        );

        // A session the workbench has never met is not parked as a delegated
        // row when the snapshot says it is archived: the unknown branch would
        // otherwise register the row and wake the ownership pass for a session
        // no root list is ever going to carry.
        let unseen = session(7);
        let mut archived_unseen = unseen.clone();
        archived_unseen.state = AgentSessionState::Archived;
        archived_unseen.archived_at_ms = Some(43);
        archived_unseen.updated_at_ms = 43;
        workbench.apply_desktop_event(DesktopEvent::SessionUpdated(archived_unseen), cx);
        assert!(
            workbench.registered_session(unseen.id.as_str()).is_none(),
            "an archived snapshot is not a creation to park and re-read for"
        );

        // The archive is reversible: the snapshot with the archive cleared is
        // not dropped by the visibility rule, so the row the restore puts back
        // reaches the sidebar again.
        workbench.upsert_session_snapshot(session(0));
        assert!(
            workbench
                .sessions
                .iter()
                .any(|root| root.id == session(0).id),
            "a restored snapshot has to be free to put the row back"
        );
        assert!(
            workbench
                .sidebar_workspace_groups("")
                .iter()
                .flat_map(|group| group.compact_sessions.iter())
                .any(|listed| listed.id == session(0).id),
            "the restored row belongs on the sidebar again"
        );
    });
}

/// An archive hides a session row only while its mutation is in flight.
///
/// Archiving is reversible and the restore is authoritative, so the optimistic
/// removal has to end with the mutation it belongs to: an id left behind keeps
/// filtering the restored session out of every authoritative list, and the row
/// never reaches the sidebar.
#[gpui::test]
fn restoring_an_archived_session_puts_its_row_back_on_the_sidebar(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 2);
    workbench.update(cx, |workbench, _cx| {
        let archived = BTreeSet::from([session(1).id.to_string()]);
        workbench.optimistically_remove_sessions(&archived, SidebarSessionRemoval::Archive);
        assert!(workbench.session_hidden_from_sidebar(session(1).id.as_str()));
        assert!(
            workbench
                .optimistically_archived_session_ids
                .contains(session(1).id.as_str())
        );
        // A deletion keeps its ids hidden until a load reconciles it; an
        // archive must not, because the runtime still has the session.
        assert!(
            !workbench
                .optimistically_removed_session_ids
                .contains(session(1).id.as_str())
        );
        // A snapshot that arrives while the archive is in flight cannot put the
        // row back.
        workbench.upsert_session_snapshot(session(1));
        assert!(
            !workbench
                .sessions
                .iter()
                .any(|root| root.id == session(1).id)
        );

        // The runtime confirms the archive, and the reader restores it.
        workbench.settle_optimistic_session_archive(&archived);
        assert!(!workbench.session_hidden_from_sidebar(session(1).id.as_str()));
        workbench.upsert_session_snapshot(session(1));
        assert!(
            workbench
                .sessions
                .iter()
                .any(|root| root.id == session(1).id)
        );
        let groups = workbench.sidebar_workspace_groups("");
        assert!(
            groups
                .iter()
                .flat_map(|group| group.compact_sessions.iter())
                .any(|listed| listed.id == session(1).id),
            "the restored session should be back on the sidebar"
        );
    });
}

/// A deletion the runtime confirmed never puts its row back.
///
/// Reads race the deletion: the turn that settled just before it, the poll
/// response and the registry page already in flight all describe the session as
/// it was when they were issued. The client applies them after the runtime has
/// answered, and every one of them used to rebuild the row — which is why a
/// just-deleted session reappeared on the sidebar and had to be deleted twice.
/// The reconciliation reload clears the optimistic marks, so the terminal
/// tombstone is what has to keep the row out afterwards.
#[gpui::test]
fn a_committed_deletion_keeps_its_row_off_the_sidebar(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 2);
    workbench.update(cx, |workbench, _cx| {
        let deleted = BTreeSet::from([session(1).id.to_string()]);
        workbench
            .pending_session_deletion_ids
            .extend(deleted.iter().cloned());
        workbench.optimistically_remove_sessions(&deleted, SidebarSessionRemoval::Delete);
        assert!(workbench.session_hidden_from_sidebar(session(1).id.as_str()));

        // The runtime answers, and the reload that follows adopts its list while
        // clearing the optimistic marks.
        workbench.settle_optimistic_session_deletion(&deleted, true);
        workbench.optimistically_removed_session_ids.clear();
        assert!(
            workbench.pending_session_deletion_ids.is_empty(),
            "the settled deletion must not stay pending"
        );
        assert!(workbench.session_hidden_from_sidebar(session(1).id.as_str()));

        // A snapshot and a list read before the deletion still carry the row.
        workbench.upsert_session_snapshot(session(1));
        assert_eq!(
            workbench.filter_hidden_sessions(vec![session(1)]).len(),
            0,
            "an authoritative list must not adopt a session the runtime deleted"
        );
        assert!(
            !workbench
                .sessions
                .iter()
                .any(|root| root.id == session(1).id)
        );
        let groups = workbench.sidebar_workspace_groups("");
        assert!(
            !groups
                .iter()
                .flat_map(|group| group.compact_sessions.iter())
                .any(|listed| listed.id == session(1).id),
            "a deleted session must not come back to the sidebar"
        );

        // A deletion the runtime rejected is only an optimistic removal: the
        // row is restored, and the reader can see that it is still there.
        let refused = BTreeSet::from([session(0).id.to_string()]);
        workbench
            .pending_session_deletion_ids
            .extend(refused.iter().cloned());
        workbench.optimistically_remove_sessions(&refused, SidebarSessionRemoval::Delete);
        workbench.settle_optimistic_session_deletion(&refused, false);
        assert!(!workbench.session_hidden_from_sidebar(session(0).id.as_str()));
        workbench.upsert_session_snapshot(session(0));
        assert!(
            workbench
                .sessions
                .iter()
                .any(|root| root.id == session(0).id),
            "a failed deletion has to put its row back"
        );
    });
}

/// An ownership root the workbench has never met is not a delegated child.
///
/// The root page describes every session without an ownership edge — exactly
/// the set the sidebar's root list draws from — and it can describe a session
/// another client just created while the announcement that names it is still
/// waiting out the event batch. Filing that row among the delegated ones took
/// it off the sidebar *and* registered the session the announcement was about,
/// so the announcement looked like news about a row the workbench already knew:
/// nothing reloaded the list the row was missing from, and the session stayed
/// invisible until some unrelated action happened to reload the overview.
#[gpui::test]
fn team_root_page_never_files_a_navigation_root_as_a_delegated_child(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    workbench.update(cx, |workbench, cx| {
        // A session the page describes without a parent is an ownership root,
        // and the tree page is not the list the sidebar is drawn from: the root
        // list is marked stale so the row arrives from the projection that owns
        // it, rather than being parked as somebody's child.
        let created = session(5);
        workbench.delegated_roots_dirty = false;
        workbench.apply_delegated_page(
            None,
            false,
            SessionTreePage {
                nodes: vec![node(5, None, 0)],
                registry: vec![created.clone()],
                ..Default::default()
            },
            cx,
        );
        assert!(
            !workbench
                .delegated_sessions
                .contains_key(created.id.as_str()),
            "a root owns itself, so the row is nobody's child"
        );
        assert!(
            workbench.delegated_roots_dirty,
            "the list that draws the row is marked stale"
        );
        assert!(
            !workbench.sessions.iter().any(|root| root.id == created.id),
            "the page describes ownership; the root list adds the row"
        );

        // An announcement the workbench could not place parked the snapshot
        // exactly there. The same page is the answer, and it has to take the
        // row back out of the delegated registry.
        let announced = session(6);
        workbench.apply_desktop_event(DesktopEvent::SessionUpdated(announced.clone()), cx);
        assert!(
            workbench
                .delegated_sessions
                .contains_key(announced.id.as_str()),
            "an announced session the workbench cannot place is parked"
        );
        workbench.delegated_roots_dirty = false;
        workbench.apply_delegated_page(
            None,
            false,
            SessionTreePage {
                nodes: vec![node(6, None, 0)],
                registry: vec![announced.clone()],
                ..Default::default()
            },
            cx,
        );
        assert!(
            !workbench
                .delegated_sessions
                .contains_key(announced.id.as_str()),
            "the ownership edge settles what the announcement could not"
        );
        assert!(workbench.delegated_roots_dirty);

        // A child of the page keeps its registry entry: that map is what the
        // branch draws its rows from, and it is not a root list row.
        let parent = session(5).id.to_string();
        let child = session(7);
        workbench.delegated_roots_dirty = false;
        workbench.apply_delegated_page(
            Some(parent),
            false,
            SessionTreePage {
                nodes: vec![node(7, Some(5), 0)],
                registry: vec![child.clone()],
                ..Default::default()
            },
            cx,
        );
        assert!(
            workbench.delegated_sessions.contains_key(child.id.as_str()),
            "a delegated child is registered under its parent"
        );
        assert!(
            !workbench.delegated_roots_dirty,
            "a child is not a row the root list has to be re-read for"
        );

        // A row the workbench is deliberately hiding is not one either: a
        // deletion in flight filters it back out of every read, so re-reading
        // would only be told the same thing until the mutation settles.
        let deleting = session(8);
        workbench
            .pending_session_deletion_ids
            .insert(deleting.id.to_string());
        workbench.delegated_roots_dirty = false;
        workbench.apply_delegated_page(
            None,
            false,
            SessionTreePage {
                nodes: vec![node(8, None, 0)],
                registry: vec![deleting],
                ..Default::default()
            },
            cx,
        );
        assert!(
            !workbench.delegated_roots_dirty,
            "a row a mutation took off the sidebar is not a stale list"
        );
    });
}

/// A fanout gap re-reads the list it may have dropped a row from.
///
/// The creation announcement rides the fanout stream, and no later event names
/// the row it announced: a receiver that lagged past it lost the only notice
/// the sidebar would have drawn, and the notification the timeline still
/// raised left the reader looking at a session that was not in the list. A
/// timeline gap is answered on the timeline it was seen on and is not a reason
/// to re-read the session list.
#[gpui::test]
fn team_fanout_gap_reloads_the_session_list(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    workbench.update(cx, |workbench, cx| {
        let before = workbench.agent_overview_generation;
        workbench.apply_desktop_event(
            DesktopEvent::Lagged {
                stream: DesktopEventStream::Fanout,
                skipped: 2,
                refetch: AuthoritativeRefetch::for_stream(DesktopEventStream::Fanout),
            },
            cx,
        );
        assert_eq!(
            workbench.agent_overview_generation,
            before.wrapping_add(1),
            "a creation the fanout dropped is only found in the root list"
        );

        let before = workbench.agent_overview_generation;
        workbench.apply_desktop_event(
            DesktopEvent::Lagged {
                stream: DesktopEventStream::Timeline,
                skipped: 2,
                refetch: AuthoritativeRefetch::for_stream(DesktopEventStream::Timeline),
            },
            cx,
        );
        assert_eq!(
            workbench.agent_overview_generation, before,
            "a timeline gap is answered by the timeline"
        );

        // A remote authority's sidebar domain reports the same gap, and the
        // invalidation the gap swallowed is the creation's only notice: the
        // mapped event has to re-read the list it staled.
        let before = workbench.agent_overview_generation;
        let gap = map_backend_event(BackendEvent::Lagged {
            stream: BackendEventStream::Fanout,
            skipped: 0,
            refetch: vibex_backend::BackendRefetch {
                session_id: None,
                timeline: false,
                runtime: false,
                runtime_selection: false,
                projection: Some(BackendProjection::Sidebar),
            },
            observed_live: true,
        })
        .expect("a lagged fan-out is a desktop event");
        workbench.apply_desktop_event(gap, cx);
        assert_eq!(
            workbench.agent_overview_generation,
            before.wrapping_add(1),
            "a sidebar gap is a creation the list has to be re-read for"
        );
    });
}

/// A remote authority's sidebar invalidation is a creation's only notice.
///
/// The authority fans the committed snapshot out to the clients that share its
/// event stream and publishes this projection invalidation for the clients that
/// do not, so a client driving a paired runtime draws a session created
/// elsewhere only because it re-read the list on this event. The invalidation
/// has no desktop twin, so it is recognized before the mapping that would drop
/// it as an event with nothing to apply.
#[test]
fn a_remote_sidebar_invalidation_is_recognized() {
    assert!(backend_event_invalidates_sidebar(
        &BackendEvent::ProjectionInvalidated(BackendProjection::Sidebar)
    ));
    assert!(!backend_event_invalidates_sidebar(
        &BackendEvent::ProjectionInvalidated(BackendProjection::Management)
    ));
    assert!(!backend_event_invalidates_sidebar(
        &BackendEvent::ProjectionInvalidated(BackendProjection::Usage)
    ));
}

/// One timeline row for a verdict test: the loader only reads `kind`, `source`,
/// and the payload, so the rest is filled with the defaults an append would use.
fn timeline_row(
    session_id: &VibexSessionId,
    sequence: i64,
    source: TimelineSource,
    payload: TimelinePayload,
) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::new(),
        session_id: session_id.clone(),
        sequence,
        timestamp_ms: sequence,
        source,
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

/// A timeline page that ends mid-turn may not pin an "abnormal stop" verdict.
///
/// The load paths read a turn-window page, so the final answer that proves a
/// normal completion is routinely cut off with the rows below it. Caching that
/// reading against the session's settled revision latches a verdict no later
/// event can revise, which is exactly what keeps an auto-send in the pre-send
/// queue from ever leaving after the turn ended normally.
#[gpui::test]
fn a_loaded_page_never_pins_an_unconfirmed_stop_to_a_settled_revision(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            let session_id = session(0).id.clone();
            let revision = session(0).updated_at_ms;
            // The page ends mid-turn: the answer is not in this window.
            let items = vec![timeline_row(
                &session_id,
                1,
                TimelineSource::User,
                TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                    text: "do the work".into(),
                    attachments: Vec::new(),
                    ..Default::default()
                }),
            )];
            assert_eq!(authoritative_loaded_turn_status(&items), None);
            workbench.cache_auto_continue_turn_status(&session_id, revision, None, cx);
            workbench.composer_queue = vec![head_queue_message(1, &session_id, None)];
            // The unjudged revision is what the shell's real queue does with
            // `None`: it waits, and the head must still leave once the
            // authority's probe answers normally.
            workbench.cache_auto_continue_turn_status(&session_id, revision, Some(true), cx);
            assert!(
                workbench
                    .cached_auto_continue_turn_status(&session_id, revision)
                    .is_some_and(|status| status.ended_normally == Some(true)),
                "a normal completion is the only verdict the queue may act on"
            );
        });
    });
}

/// A normal completion the page fully contains is still cached as `true`.
#[test]
fn a_loaded_page_caches_the_normal_completion_it_can_see() {
    let session_id = VibexSessionId::new();
    let items = vec![
        timeline_row(
            &session_id,
            1,
            TimelineSource::User,
            TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: "do the work".into(),
                attachments: Vec::new(),
                ..Default::default()
            }),
        ),
        timeline_row(
            &session_id,
            2,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
        ),
    ];
    assert_eq!(authoritative_loaded_turn_status(&items), Some(true));
}

/// A page that ends mid-turn leaves the answer and the reader alone.
///
/// The selected session is loaded as soon as the desktop starts, and that read
/// is what an unfinished restored turn reaches first. The page it returns
/// routinely ends mid-turn, so it carries no verdict. Caching that reading used
/// to overwrite the verdict a probe had already supplied for the same revision
/// and cancel the probe still running for it — after recording the attempt, so
/// the sync believed an answer was on its way while nothing was coming. The
/// countdown was therefore missing for exactly the session the reader had
/// selected, and only appeared after a reselect loaded the page once more.
#[gpui::test]
fn a_loaded_page_with_no_verdict_keeps_the_verdict_and_the_probe(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            let session_id = session(0).id.clone();
            let revision = session(0).updated_at_ms;
            // The authority already judged the revision, and a probe for it is
            // still running: a load that cannot judge it may not displace
            // either.
            workbench.cache_auto_continue_turn_status(&session_id, revision, Some(false), cx);
            workbench.auto_continue_probe_tasks.insert(
                session_id.to_string(),
                AutoContinueProbeTask {
                    session_updated_at_ms: revision,
                    _task: cx.spawn(async move |_, _| std::future::pending::<()>().await),
                },
            );
            assert!(
                !workbench.cache_auto_continue_turn_status(&session_id, revision, None, cx),
                "an unjudged reading changes nothing"
            );
            assert_eq!(
                workbench
                    .cached_auto_continue_turn_status(&session_id, revision)
                    .map(|status| status.ended_normally),
                Some(Some(false)),
                "a page without a verdict may not erase the one the probe gave"
            );
            assert!(
                workbench
                    .auto_continue_probe_tasks
                    .contains_key(session_id.as_str()),
                "the probe is the only reader left that can judge the revision"
            );

            // A verdict still settles the revision and retires that probe.
            assert!(workbench.cache_auto_continue_turn_status(
                &session_id,
                revision,
                Some(true),
                cx
            ));
            assert_eq!(
                workbench
                    .cached_auto_continue_turn_status(&session_id, revision)
                    .map(|status| status.ended_normally),
                Some(Some(true))
            );
            assert!(
                !workbench
                    .auto_continue_probe_tasks
                    .contains_key(session_id.as_str()),
                "a settled revision needs no further probe"
            );
        });
    });
}

/// An unjudged load answer does not strand the countdown of an enabled session.
///
/// This is the reported shape end to end: the session is enabled, its restored
/// turn is unfinished, and the load answers for the current revision with no
/// verdict. The countdown must not be cancelled by that answer, and the probe
/// whose result would start it must not be cancelled with it.
#[gpui::test]
fn an_unjudged_load_answer_leaves_an_enabled_session_counting(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            let session_id = session(0).id.clone();
            let revision = session(0).updated_at_ms;
            workbench
                .auto_continue_session_ids
                .insert(session_id.to_string());
            // A probe was asked for the revision and is waiting on the
            // authority; meanwhile the startup load answers without a verdict.
            workbench.auto_continue_probe_tasks.insert(
                session_id.to_string(),
                AutoContinueProbeTask {
                    session_updated_at_ms: revision,
                    _task: cx.spawn(async move |_, _| std::future::pending::<()>().await),
                },
            );
            workbench.record_auto_continue_probe_attempt(&session_id, revision);
            workbench.cache_auto_continue_turn_status(&session_id, revision, None, cx);
            assert!(
                workbench
                    .auto_continue_probe_tasks
                    .contains_key(session_id.as_str()),
                "the answer that would start the countdown is still on its way"
            );
            // The probe answering normally settles the same revision, which is
            // what starts the countdown.
            workbench.cache_auto_continue_turn_status(&session_id, revision, Some(false), cx);
            assert!(
                workbench
                    .auto_continue_countdowns
                    .contains_key(session_id.as_str()),
                "the restored unfinished turn counts down once it is judged"
            );
        });
    });
}

/// An authoritative whole-list read wakes a queue waiting on a settled row.
///
/// A client that draws the sidebar projection never sees the per-session
/// snapshot the queue was waiting on, so the settle reaches it only through the
/// list. The replaced row has a newer revision than the one the queue holds a
/// probe answer for, and the queue has to ask for the current revision instead
/// of waiting forever on the superseded one.
#[gpui::test]
fn an_authoritative_overview_read_wakes_a_waiting_auto_send(cx: &mut TestAppContext) {
    let (workbench, cx, _) = fixture(cx, 1);
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, _| {
            let session_id = session(0).id.clone();
            // A verdict pinned to a revision the list has already moved past.
            workbench.auto_continue_turn_statuses.insert(
                session_id.to_string(),
                AutoContinueTurnStatus {
                    session_updated_at_ms: session(0).updated_at_ms,
                    ended_normally: Some(false),
                },
            );
            workbench.composer_queue = vec![head_queue_message(1, &session_id, None)];
            let mut settled = session(0);
            settled.state = AgentSessionState::Idle;
            settled.updated_at_ms = session(0).updated_at_ms + 100;
            workbench.sessions = vec![settled];
            workbench.wake_composer_queue_after_overview();
            assert!(
                workbench
                    .cached_auto_continue_turn_status(&session_id, session(0).updated_at_ms + 100)
                    .is_none(),
                "the superseded verdict may not speak for the revision the list shows"
            );
            assert!(
                workbench
                    .composer_queue_ready_after_continuation_session_ids
                    .contains(session_id.as_str()),
                "the waiting auto-send is re-evaluated against the current revision"
            );
        });
    });
}

/// A turn that settles through its own snapshot hands the queued message over.
///
/// This drives the real event path the reader's session takes: a dispatch arms
/// the turn, the authority admits it, and then it settles normally. The queued
/// auto-send behind it must leave on that settle without any further gesture —
/// the failure the reader reported was exactly this message staying put.
#[gpui::test]
fn a_locally_dispatched_turn_settles_the_queued_auto_send(cx: &mut TestAppContext) {
    let (workbench, cx, receiver) = fixture(cx, 1);
    let session_id = session(0).id.clone();
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            let mut running = session(0);
            running.state = AgentSessionState::Running;
            running.updated_at_ms = 50;
            workbench.apply_desktop_event(DesktopEvent::SessionUpdated(running.clone()), cx);
            workbench.set_session_turn_pending(&session_id, true);
            // A message the reader lined up while the turn was running.
            workbench.composer_queue = vec![head_queue_message(1, &session_id, None)];
        });
    });

    // The authority settles the turn. The settle arms the queue, and the flush
    // the render pass runs is what hands the head over — no second gesture.
    let mut settled = session(0);
    settled.state = AgentSessionState::Idle;
    settled.updated_at_ms = 70;
    cx.update(|_, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.apply_desktop_event(DesktopEvent::SessionUpdated(settled.clone()), cx);
            assert!(
                workbench
                    .composer_queue_ready_after_continuation_session_ids
                    .contains(session_id.as_str()),
                "the settle arms the waiting auto-send for re-evaluation"
            );
        });
    });
    // The queued head waits for the revision's verdict, so it leaves on the
    // flush the render pass runs once the probe has answered — no second
    // gesture. A headless test never paints, so the flush is driven here.
    let mut dispatched = false;
    for _ in 0..400 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            workbench.update(cx, |workbench, cx| {
                workbench.flush_composer_queue_dispatches_after_continuation(window, cx);
            });
        });
        if workbench.read_with(cx, |workbench, _| workbench.composer_queue.is_empty()) {
            dispatched = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        dispatched,
        "the settle hands the queued auto-send on without a second gesture"
    );
    let sent = pump_until(cx, |_| {
        receiver
            .try_recv()
            .is_ok_and(|observed| matches!(observed, Observed::Message(_)))
    });
    assert!(sent, "the submission path received the queued message");
}

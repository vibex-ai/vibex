use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use std::sync::{Mutex, mpsc as sync_mpsc};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendEventSubscription, BackendFuture, DisconnectedBackend,
};
use vibex_core::*;

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
    }
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
            workbench.optimistically_remove_sessions(&BTreeSet::from([session(1).id.to_string()]));
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

#[gpui::test]
async fn team_queued_session_recovery_uses_metadata_and_drops_only_confirmed_deletions(
    cx: &mut TestAppContext,
) {
    let (workbench, cx, receiver) = fixture(cx, 2);
    cx.update(|window, cx| {
        workbench.update(cx, |workbench, cx| {
            workbench.delegated_sessions.clear();
            workbench.composer_queue.push(queued_worker_message());
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
        assert_eq!(workbench.composer_queue, [queued_worker_message()]);
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
    let disclosure = cx
        .debug_bounds("sidebar-delegated-toggle-session_team_0")
        .expect("root disclosure is rendered");
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
        workbench.selected_session_id = Some(session(63).id);
        workbench.borrow_session_view(&session(63).id);
        workbench.ensure_session_group_views("team", cx);
        let mut resident: BTreeSet<_> = workbench.session_views.keys().cloned().collect();
        resident.extend(workbench.view_session_id.as_ref().map(ToString::to_string));
        assert_eq!(resident.len(), 4);
        assert!(resident.contains(session(63).id.as_str()));
        assert!(!resident.contains(session(3).id.as_str()));
    });
    let Observed::Timeline(id) = receiver.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected lazy tab fetch");
    };
    assert_eq!(id, session(63).id);
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

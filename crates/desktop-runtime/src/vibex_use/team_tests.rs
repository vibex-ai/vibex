use super::*;
use crate::sidebar_organization::{SessionGroupPresentationRequest, SidebarOrganizationBridge};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{AgentSessionSafety, WorkspaceMode};

struct Fixture {
    service: Arc<VibexUseService>,
    directory: tempfile::TempDir,
}

#[test]
fn selected_delegation_mentions_preserve_explicit_ranges_and_exact_runtime() {
    let mut request = HumanDelegationRequest::new(VibexSessionId::new(), "Review the context");
    let selected = VibexSessionId::new();
    let another = VibexSessionId::new();
    let runtime = VibexUseRef::runtime_option("selected_option");
    request.context_refs.push(TeamContextReference {
        session_id: selected.clone(),
        from_sequence: Some(3),
        through_sequence: Some(7),
    });
    request.mentions = vec![
        vibex_core::VibexUseMention::session(&selected, None),
        vibex_core::VibexUseMention::session(&another, None),
        vibex_core::VibexUseMention::agent(&AgentId::parse("codex").unwrap(), &runtime, None),
    ];
    selected_delegation_context(&mut request).unwrap();
    assert_eq!(request.runtime_option_ref.as_ref(), Some(&runtime));
    assert_eq!(request.context_refs.len(), 2);
    assert_eq!(request.context_refs[0].from_sequence, Some(3));
    assert_eq!(request.context_refs[0].through_sequence, Some(7));
    assert_eq!(request.context_refs[1].session_id, another);
    assert_eq!(request.context_refs[1].from_sequence, None);
    selected_delegation_context(&mut request).unwrap();
    assert_eq!(request.context_refs.len(), 2);
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("team.db");
        let manager = Arc::new(AgentManager::new(&path).unwrap());
        let catalog = Arc::new(RuntimeOptionCatalogService::new(
            manager.clone(),
            ProviderConfigService::new(&path),
        ));
        let service = VibexUseService::new(
            &path,
            "team-test",
            manager,
            catalog,
            SessionGroupPresentationBridge::new(),
            SidebarOrganizationBridge::new(),
            1,
        );
        Self { service, directory }
    }

    fn session(&self, title: &str) -> AgentSession {
        let conn = self.service.open().unwrap();
        let (project, workspace) = WorkspaceRepository::ensure(
            &conn,
            self.directory.path(),
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: title.to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        session
    }

    fn own(&self, parent: &AgentSession, child: &AgentSession) {
        SessionOwnershipRepository::upsert(
            &self.service.open().unwrap(),
            &child.id,
            &parent.id,
            None,
        )
        .unwrap();
    }

    fn user_group(&self, owner: &AgentSession) -> GroupPresentationRecord {
        let id = SessionGroupId::new();
        let members = vec![owner.id.clone()];
        let layout = SessionGroupLayoutIntent::default();
        GroupPresentationRepository::reserve_or_get(
            &self.service.open().unwrap(),
            None,
            owner.id.as_str(),
            id.as_str(),
            &SessionGroupScope::Workspace {
                workspace_ref: VibexUseRef::workspace(owner.workspace_id.as_str()),
            },
            "Review",
            &members,
            &layout,
            "recoverable-human-group",
        )
        .unwrap();
        self.service
            .sync_user_group(id.as_str(), "My review", &members, &layout, 17)
            .unwrap();
        GroupPresentationRepository::get(&self.service.open().unwrap(), id.as_str())
            .unwrap()
            .unwrap()
    }

    fn reopened_service(&self) -> Arc<VibexUseService> {
        VibexUseService::new(
            &self.service.db_path,
            self.service.authority.clone(),
            self.service.manager.clone(),
            self.service.runtime_catalog.clone(),
            SessionGroupPresentationBridge::new(),
            SidebarOrganizationBridge::new(),
            self.service.activation_revision(),
        )
    }

    async fn interrupted_presentation(
        &self,
        request: TeamPresentationRequest,
    ) -> VibexUseOperation {
        let mut shell = self.service.presentation.attach();
        let service = self.service.clone();
        let call = tokio::spawn(async move {
            service
                .human_present_team(request, "interrupted-show".into(), "human:local".into())
                .await
        });
        let pending = tokio::time::timeout(std::time::Duration::from_secs(5), shell.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            pending,
            SessionGroupPresentationRequest::Apply { .. }
        ));
        // Kill the caller after acceptance and before the display reply. The
        // new service below must resume from SQLite, without an in-memory gate.
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        drop(pending);
        self.service.presentation.detach();
        VibexUseOperationRepository::list_pending(&self.service.open().unwrap())
            .unwrap()
            .into_iter()
            .find(|operation| operation.tool == "human_present_team")
            .unwrap()
    }
}

#[tokio::test]
async fn human_access_retry_does_not_undo_a_newer_revoke() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let target = fixture.session("Context");
    let request = SessionAccessRequest {
        grantee_session_id: parent.id.clone(),
        target_session_id: target.id.clone(),
        access: SessionAccess::Control,
    };
    let first = fixture
        .service
        .human_set_session_access(request.clone(), "control".into(), "human:local".into())
        .await
        .unwrap();
    assert!(first.changed);
    fixture
        .service
        .human_set_session_access(
            SessionAccessRequest {
                access: SessionAccess::Revoke,
                ..request.clone()
            },
            "revoke".into(),
            "human:local".into(),
        )
        .await
        .unwrap();
    let replay = fixture
        .service
        .human_set_session_access(request.clone(), "control".into(), "human:local".into())
        .await
        .unwrap();
    assert_eq!(replay, first);
    assert!(
        SessionGrantRepository::get(&fixture.service.open().unwrap(), &parent.id, &target.id)
            .unwrap()
            .is_none()
    );
    let conflict = fixture
        .service
        .human_set_session_access(
            SessionAccessRequest {
                access: SessionAccess::Read,
                ..request
            },
            "control".into(),
            "human:local".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, use_codes::IDEMPOTENCY_PAYLOAD_CONFLICT);
}

#[tokio::test]
async fn human_tree_pages_keep_ownership_and_direct_navigation_ancestors() {
    let fixture = Fixture::new();
    let roots: Vec<_> = (0..4)
        .map(|index| fixture.session(&format!("Root {index}")))
        .collect();
    let children: Vec<_> = (0..3)
        .map(|index| fixture.session(&format!("Worker {index}")))
        .collect();
    for child in &children {
        fixture.own(&roots[0], child);
    }
    let grandchild = fixture.session("Nested worker");
    fixture.own(&children[0], &grandchild);
    let mut request = SessionTreeRequest {
        limit: Some(2),
        ..Default::default()
    };
    let mut loaded = std::collections::BTreeSet::new();
    loop {
        let page = fixture
            .service
            .human_session_tree(request.clone())
            .await
            .unwrap();
        assert!(page.ancestors.is_empty());
        assert!(page.nodes.len() <= 2);
        for node in page.nodes {
            assert!(node.parent_session_ref.is_none());
            assert!(loaded.insert(node.session_ref.session_id().unwrap()));
        }
        if !page.has_more {
            break;
        }
        request.cursor = page.next_cursor;
    }
    assert_eq!(
        loaded,
        roots.iter().map(|session| session.id.clone()).collect()
    );
    request = SessionTreeRequest {
        parent_session_id: Some(roots[0].id.clone()),
        limit: Some(2),
        ..Default::default()
    };
    loaded.clear();
    loop {
        let page = fixture
            .service
            .human_session_tree(request.clone())
            .await
            .unwrap();
        assert_eq!(page.ancestors.len(), 1);
        assert_eq!(
            page.ancestors[0].session_ref.session_id().as_ref(),
            Some(&roots[0].id)
        );
        for node in page.nodes {
            assert_eq!(
                node.parent_session_ref
                    .as_ref()
                    .and_then(VibexUseRef::session_id)
                    .as_ref(),
                Some(&roots[0].id)
            );
            assert!(loaded.insert(node.session_ref.session_id().unwrap()));
        }
        if !page.has_more {
            break;
        }
        request.cursor = page.next_cursor;
    }
    assert_eq!(
        loaded,
        children.iter().map(|session| session.id.clone()).collect()
    );
    let direct = fixture
        .service
        .human_session_tree(SessionTreeRequest {
            parent_session_id: Some(grandchild.id.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(direct.nodes.is_empty());
    assert_eq!(
        direct
            .ancestors
            .iter()
            .filter_map(|node| node.session_ref.session_id())
            .collect::<Vec<_>>(),
        vec![
            roots[0].id.clone(),
            children[0].id.clone(),
            grandchild.id.clone()
        ]
    );
    assert_eq!(direct.registry.len(), 3);
}

#[test]
fn referenceable_sessions_match_owned_and_older_sessions_before_limiting() {
    let fixture = Fixture::new();
    let parent = fixture.session("Recent conversation");
    let child = fixture.session("Earlier 100% report");
    fixture.own(&parent, &child);
    let conn = fixture.service.open().unwrap();
    conn.execute(
        "UPDATE agent_sessions SET updated_at_ms = 1, current_agent_id = 'review-helper'
         WHERE session_id = ?1",
        params![child.id.as_str()],
    )
    .unwrap();
    for query in ["  EARLIER  ", "100%", "HELPER", child.id.as_str()] {
        let matches = fixture.service.referenceable_sessions(query, 1).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, child.id);
    }
    assert_eq!(
        fixture.service.referenceable_sessions("", 1).unwrap()[0].id,
        parent.id
    );
    assert!(
        fixture
            .service
            .referenceable_sessions("", 0)
            .unwrap()
            .is_empty()
    );
    conn.execute(
        "UPDATE agent_sessions SET archived_at_ms = 1 WHERE session_id = ?1",
        params![child.id.as_str()],
    )
    .unwrap();
    assert!(
        fixture
            .service
            .referenceable_sessions("earlier", 1)
            .unwrap()
            .is_empty()
    );
    conn.execute(
        "UPDATE agent_sessions SET deleted_at_ms = 1 WHERE session_id = ?1",
        params![parent.id.as_str()],
    )
    .unwrap();
    assert!(
        fixture
            .service
            .referenceable_sessions("", 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn human_interrupt_rejects_a_stale_task_revision_and_changed_controller() {
    let fixture = Fixture::new();
    let parent = fixture.session("Lead");
    let child = fixture.session("Worker");
    fixture.own(&parent, &child);
    let mut conn = fixture.service.open().unwrap();
    let mut task = AgentDelegation::single_turn_legacy(
        parent.id.clone(),
        "interrupt-task",
        "Review",
        "Review this change",
        Some(child.agent_id.clone()),
        AgentDelegationStatus::Starting,
        unix_timestamp_ms(),
    );
    task.root_session_id = Some(parent.id.clone());
    task.child_session_id = Some(child.id.clone());
    task.completion_policy = DelegationCompletionPolicy::OwnerReview;
    let task = match AgentDelegationRepository::reserve_or_get(&mut conn, &task, 8).unwrap() {
        vibex_db::AgentDelegationReservation::Claimed(task) => task,
        other => panic!("expected a task, got {other:?}"),
    };
    SessionControllerRepository::claim(&conn, &child.id, &task.id, &parent.id, None).unwrap();
    let mut request = TeamTaskControlRequest {
        session_id: parent.id,
        task_id: task.id.clone(),
        expected_revision: Some(0),
        action: TeamTaskAction::Interrupt,
    };
    assert_eq!(
        fixture
            .service
            .human_control_team_task(
                request.clone(),
                "stale-interrupt".into(),
                "human:local".into(),
            )
            .await
            .unwrap_err()
            .code,
        use_codes::REVISION_CONFLICT
    );
    request.expected_revision = Some(task.revision);
    SessionControllerRepository::mark_human_controlled(&conn, &child.id).unwrap();
    assert_eq!(
        fixture
            .service
            .human_control_team_task(request, "changed-controller".into(), "human:local".into(),)
            .await
            .unwrap_err()
            .code,
        use_codes::CONTROLLER_CHANGED
    );
    let unchanged = AgentDelegationRepository::get(&conn, &task.id)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.revision, task.revision);
    assert_eq!(unchanged.phase(), task.phase());
}

#[tokio::test]
async fn taskless_interrupt_checks_the_first_queued_round_and_replays_acceptance() {
    let fixture = Fixture::new();
    let session = fixture.session("Standalone session");
    let mut conn = fixture.service.open().unwrap();
    let actor = fixture.service.human_actor(&conn, &session.id).unwrap();
    let mut executions = Vec::new();
    for key in ["first-round", "second-round"] {
        let arguments = serde_json::json!({
            "sessionRef": VibexUseRef::session(&session.id).as_uri(),
            "idempotencyKey": key,
            "text": "Review the next round",
        });
        let (operation, claimed) = fixture
            .service
            .reserve_operation(
                &conn,
                &actor,
                VibexUseTool::SendMessage,
                key,
                &arguments_fingerprint(&arguments),
                &arguments,
            )
            .unwrap();
        assert!(claimed);
        let submission = vibex_db::MessageSubmissionRepository::enqueue(
            &mut conn,
            MessageSubmissionId::new(),
            &SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: key.into(),
                desired_runtime: SessionRuntimeSelection::provider(
                    session.agent_id.clone(),
                    vibex_core::ProviderProfileId::parse("provider_fixture").unwrap(),
                    "fixture-model",
                ),
                text: "Review the next round".into(),
                attachments: Vec::new(),
                mentions: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
                delivery: UserMessageDelivery::Prompt,
                prompt_context: None,
                provenance: MessageProvenance::DelegatedInput {
                    actor_session_ref: VibexUseRef::session(&session.id),
                    task_ref: None,
                    operation_ref: operation.operation_ref,
                },
            },
        )
        .unwrap();
        let execution =
            VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
                .unwrap()
                .unwrap();
        VibexUseOperationRepository::update_state(
            &conn,
            &operation.id,
            VibexUseOperationState::Succeeded,
            None,
            None,
            false,
        )
        .unwrap();
        assert!(execution.task_ref.is_none());
        executions.push(execution);
    }
    let stale = serde_json::json!({
        "sessionRef": VibexUseRef::session(&session.id).as_uri(),
        "expectedExecutionRef": executions[1].execution_ref.as_uri(),
        "idempotencyKey": "wrong-round",
    });
    assert_eq!(
        fixture
            .service
            .interrupt(&actor, &stale)
            .await
            .unwrap_err()
            .code,
        use_codes::CONTROLLER_CHANGED
    );
    let current = serde_json::json!({
        "sessionRef": VibexUseRef::session(&session.id).as_uri(),
        "expectedExecutionRef": executions[0].execution_ref.as_uri(),
        "idempotencyKey": "current-round",
    });
    // Another accepted interrupt loses its caller after the durable target
    // checkpoint. Recovery must retain that target after a successor exists.
    let mut recovery_request = current.clone();
    recovery_request["idempotencyKey"] = serde_json::json!("recover-current-round");
    let (pending, claimed) = fixture
        .service
        .reserve_operation(
            &conn,
            &actor,
            VibexUseTool::Interrupt,
            "recover-current-round",
            &arguments_fingerprint(&recovery_request),
            &recovery_request,
        )
        .unwrap();
    assert!(claimed);
    assert!(vibex_db::VibexUseInterruptRepository::request(&conn, &executions[0].id).unwrap());
    VibexUseOperationRepository::set_checkpoint_once(
        &conn,
        &pending.id,
        "interrupt_execution_id",
        executions[0].id.as_str(),
    )
    .unwrap();
    fixture
        .service
        .record_operation_resource(
            &conn,
            &pending,
            "execution",
            executions[0].execution_ref.clone(),
        )
        .unwrap();
    let accepted = fixture.service.interrupt(&actor, &current).await.unwrap();
    assert_eq!(accepted["status"], "accepted");
    for (index, execution) in executions.iter().enumerate() {
        let submission =
            vibex_db::MessageSubmissionRepository::get(&conn, &execution.submission_id)
                .unwrap()
                .unwrap();
        assert_eq!(
            submission.status,
            if index == 0 {
                vibex_core::MessageSubmissionStatus::Cancelled
            } else {
                vibex_core::MessageSubmissionStatus::AwaitingRuntime
            }
        );
        assert!(submission.dispatched_at_ms.is_none());
        assert_eq!(
            vibex_db::VibexUseInterruptRepository::is_requested(&conn, &execution.id).unwrap(),
            index == 0
        );
    }
    // The accepted retry still succeeds after its expected round was cancelled.
    fixture.service.interrupt(&actor, &current).await.unwrap();
    assert_eq!(
        fixture
            .reopened_service()
            .recover_operations()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        VibexUseOperationRepository::get(&conn, &pending.id)
            .unwrap()
            .unwrap()
            .state,
        VibexUseOperationState::Succeeded
    );
    assert_eq!(
        vibex_db::VibexUseInterruptRepository::current_execution(&conn, &session.id)
            .unwrap()
            .unwrap()
            .id,
        executions[1].id
    );
    assert!(!vibex_db::VibexUseInterruptRepository::request(&conn, &executions[0].id).unwrap());
    // A submission can finish before its observer updates the execution row.
    // Such a round must not acquire a late interrupt marker either.
    use vibex_core::MessageSubmissionStatus;
    for statuses in [
        MessageSubmissionStatus::AwaitingRuntime,
        MessageSubmissionStatus::ReadyToDispatch,
        MessageSubmissionStatus::AboutToPrompt,
        MessageSubmissionStatus::Dispatched,
        MessageSubmissionStatus::Completed,
    ]
    .windows(2)
    {
        vibex_db::MessageSubmissionRepository::advance_status(
            &conn,
            &executions[1].submission_id,
            statuses[0],
            statuses[1],
        )
        .unwrap();
    }
    assert!(!vibex_db::VibexUseInterruptRepository::request(&conn, &executions[1].id).unwrap());
    assert!(
        !vibex_db::VibexUseInterruptRepository::is_requested(&conn, &executions[1].id).unwrap()
    );
}

#[tokio::test]
async fn human_ack_is_team_scoped_atomic_and_independent_from_agent_delivery() {
    let fixture = Fixture::new();
    let root = fixture.session("Root");
    let foreign = fixture.session("Other team");
    let conn = fixture.service.open().unwrap();
    for (id, session) in [("ours", &root), ("foreign", &foreign)] {
        VibexUseEventRepository::append(
            &conn,
            id,
            Some(&session.id),
            DelegationTaskEventKind::TaskResultAvailable,
            None,
            Some(&session.id),
            1,
            &serde_json::json!({}),
        )
        .unwrap();
    }
    let before = fixture
        .service
        .human_team_snapshot(TeamSnapshotRequest::new(root.id.clone()))
        .await
        .unwrap();
    assert_eq!(before.inbox.events.len(), 1);
    assert!(!before.inbox.events[0].acknowledged);
    let rejected = fixture
        .service
        .human_acknowledge_team_events(
            TeamAcknowledgeRequest {
                session_id: root.id.clone(),
                event_ids: vec!["ours".into(), "foreign".into()],
            },
            "human:local".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code, "team_event_scope_denied");
    assert_eq!(
        VibexUseEventRepository::unacknowledged_for_root(
            &conn,
            HUMAN_INBOX,
            &root.id,
            0,
            10,
            false
        )
        .unwrap()
        .len(),
        1
    );
    let request = TeamAcknowledgeRequest {
        session_id: root.id.clone(),
        event_ids: vec!["ours".into()],
    };
    assert_eq!(
        fixture
            .service
            .human_acknowledge_team_events(request.clone(), "human:local".into())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .service
            .human_acknowledge_team_events(request, "human:local".into())
            .await
            .unwrap(),
        0
    );
    let after = fixture
        .service
        .human_team_snapshot(TeamSnapshotRequest::new(root.id.clone()))
        .await
        .unwrap();
    assert!(after.inbox.events.is_empty());
    assert_eq!(
        VibexUseEventRepository::unacknowledged_for_root(
            &conn,
            "agent:root",
            &root.id,
            0,
            10,
            false
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn delegation_reference_replay_cannot_restore_a_revoked_read_grant() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let target = fixture.session("Context");
    let mut request = HumanDelegationRequest::new(parent.id.clone(), "Review selected context");
    request.context_refs.push(TeamContextReference {
        session_id: target.id.clone(),
        from_sequence: Some(1),
        through_sequence: Some(2),
    });
    fixture
        .service
        .selected_team_references(&request, "delegate", "human:local")
        .unwrap();
    let conn = fixture.service.open().unwrap();
    assert_eq!(
        SessionGrantRepository::get(&conn, &parent.id, &target.id)
            .unwrap()
            .unwrap()
            .scope,
        "referenced"
    );
    SessionGrantRepository::revoke(&conn, &parent.id, &target.id).unwrap();
    fixture
        .service
        .selected_team_references(&request, "delegate", "human:local")
        .unwrap();
    assert!(
        SessionGrantRepository::get(&conn, &parent.id, &target.id)
            .unwrap()
            .is_none()
    );
    request.task = "Different work".into();
    assert_eq!(
        fixture
            .service
            .selected_team_references(&request, "delegate", "human:local")
            .unwrap_err()
            .code,
        use_codes::IDEMPOTENCY_PAYLOAD_CONFLICT
    );
}

#[tokio::test]
async fn human_presentation_resolves_the_group_owner_and_respects_manual_layout_revision() {
    let fixture = Fixture::new();
    let parent = fixture.session("Lead");
    let child = fixture.session("Worker");
    let foreign = fixture.session("Other team");
    fixture.own(&parent, &child);
    let group_id = SessionGroupId::new();
    let members = vec![parent.id.clone(), child.id.clone()];
    let layout = SessionGroupLayoutIntent::default();
    GroupPresentationRepository::reserve_or_get(
        &fixture.service.open().unwrap(),
        None,
        parent.id.as_str(),
        group_id.as_str(),
        &SessionGroupScope::Workspace {
            workspace_ref: VibexUseRef::workspace(parent.workspace_id.as_str()),
        },
        "Review",
        &members,
        &layout,
        "human-group",
    )
    .unwrap();
    fixture
        .service
        .sync_user_group(group_id.as_str(), "My review", &members, &layout, 17)
        .unwrap();
    let record =
        GroupPresentationRepository::get(&fixture.service.open().unwrap(), group_id.as_str())
            .unwrap()
            .unwrap();
    assert!(!record.created_by_caller);
    let request = TeamPresentationRequest {
        session_id: child.id.clone(),
        group_ref: VibexUseRef::group(group_id.as_str()),
        expected_revision: Some(record.revision),
        activation_policy: PresentationActivationPolicy::WhenUserReturns,
        focus_session_id: Some(child.id.clone()),
    };
    let outcome = fixture
        .service
        .human_present_team(request.clone(), "show".into(), "human:local".into())
        .await
        .unwrap();
    assert_eq!(outcome.state, PresentationState::Prepared);
    assert_eq!(
        fixture
            .service
            .human_present_team(request.clone(), "show".into(), "human:local".into())
            .await
            .unwrap(),
        outcome
    );
    let still_user_owned =
        GroupPresentationRepository::get(&fixture.service.open().unwrap(), group_id.as_str())
            .unwrap()
            .unwrap();
    assert!(!still_user_owned.created_by_caller);
    assert_eq!(still_user_owned.revision, record.revision);
    let mut stale = request.clone();
    stale.expected_revision = Some(record.revision.saturating_sub(1));
    assert!(
        fixture
            .service
            .human_present_team(stale, "stale".into(), "human:local".into())
            .await
            .is_err()
    );
    let mut other_team = request;
    other_team.session_id = foreign.id;
    assert_eq!(
        fixture
            .service
            .human_present_team(other_team, "foreign".into(), "human:local".into())
            .await
            .unwrap_err()
            .code,
        "team_group_scope_denied"
    );
}

#[tokio::test]
async fn human_presentation_recovers_after_acceptance_and_replays_the_cached_result() {
    let fixture = Fixture::new();
    let owner = fixture.session("Lead");
    let group = fixture.user_group(&owner);
    let request = TeamPresentationRequest {
        session_id: owner.id.clone(),
        group_ref: VibexUseRef::group(&group.group_id),
        expected_revision: None,
        activation_policy: PresentationActivationPolicy::IfCurrentTeam,
        focus_session_id: Some(owner.id),
    };
    let operation = fixture.interrupted_presentation(request.clone()).await;
    assert_eq!(operation.state, VibexUseOperationState::Accepted);
    assert!(operation.checkpoint.contains_key("request"));

    let recovered = fixture.reopened_service();
    let mut shell = recovered.presentation.attach();
    let service = recovered.clone();
    let recovery = tokio::spawn(async move { service.recover_operations().await });
    let pending = tokio::time::timeout(std::time::Duration::from_secs(5), shell.recv())
        .await
        .unwrap()
        .unwrap();
    let SessionGroupPresentationRequest::Apply { command, reply } = pending else {
        panic!("expected a recovered presentation");
    };
    assert!(command.presentation_only);
    assert_eq!(
        command.activation_policy,
        PresentationActivationPolicy::WhenUserReturns
    );
    assert_eq!(command.expected_revision, group.client_revision);
    reply
        .send(Ok(vibex_core::GroupPresentationReply {
            state: PresentationState::Presented,
            revision: 17,
            reason: None,
            message: None,
            applied_layout: None,
        }))
        .unwrap();
    assert_eq!(recovery.await.unwrap().unwrap(), 1);
    let replay = recovered
        .human_present_team(request, "interrupted-show".into(), "human:local".into())
        .await
        .unwrap();
    assert_eq!(replay.state, PresentationState::Presented);
    assert!(shell.try_recv().is_err());
    let conn = recovered.open().unwrap();
    assert_eq!(
        VibexUseOperationRepository::get(&conn, &operation.id)
            .unwrap()
            .unwrap()
            .state,
        VibexUseOperationState::Succeeded
    );
    let stored = GroupPresentationRepository::get(&conn, &group.group_id)
        .unwrap()
        .unwrap();
    assert!(!stored.created_by_caller);
    assert_eq!(stored.revision, group.revision);
}

#[tokio::test]
async fn human_presentation_recovery_cannot_present_a_newer_manual_layout() {
    let fixture = Fixture::new();
    let owner = fixture.session("Lead");
    let group = fixture.user_group(&owner);
    let request = TeamPresentationRequest {
        session_id: owner.id,
        group_ref: VibexUseRef::group(&group.group_id),
        expected_revision: None,
        activation_policy: PresentationActivationPolicy::IfCurrentTeam,
        focus_session_id: None,
    };
    let operation = fixture.interrupted_presentation(request.clone()).await;
    fixture
        .service
        .sync_user_group(
            &group.group_id,
            "New arrangement",
            &group.member_session_ids,
            &group.layout,
            23,
        )
        .unwrap();
    let recovered = fixture.reopened_service();
    let mut shell = recovered.presentation.attach();
    assert_eq!(recovered.recover_operations().await.unwrap(), 0);
    assert!(shell.try_recv().is_err());
    assert_eq!(
        recovered
            .human_present_team(request, "interrupted-show".into(), "human:local".into())
            .await
            .unwrap_err()
            .code,
        use_codes::PRESENTATION_LAYOUT_CONFLICT
    );
    let conn = recovered.open().unwrap();
    let operation = VibexUseOperationRepository::get(&conn, &operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(operation.state, VibexUseOperationState::Failed);
    assert_eq!(
        operation.error_code.as_deref(),
        Some(use_codes::PRESENTATION_LAYOUT_CONFLICT)
    );
    let stored = GroupPresentationRepository::get(&conn, &group.group_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.name, "New arrangement");
    assert_eq!(stored.client_revision, Some(23));
    assert!(!stored.created_by_caller);
}

fn team_control_message(
    target: &AgentSession,
    key: &str,
    provenance: MessageProvenance,
) -> SendAgentMessageRequest {
    SendAgentMessageRequest {
        session_id: target.id.clone(),
        message_idempotency_key: key.to_string(),
        desired_runtime: SessionRuntimeSelection::provider(
            target.agent_id.clone(),
            vibex_core::ProviderProfileId::parse("provider_fixture").unwrap(),
            "fixture-model",
        ),
        text: "Check the current changes".into(),
        attachments: Vec::new(),
        mentions: Vec::new(),
        reasoning_effort: None,
        correlation_id: None,
        delivery: UserMessageDelivery::Prompt,
        prompt_context: None,
        provenance,
    }
}

fn taskless_control_message(
    fixture: &Fixture,
    actor_session: &AgentSession,
    target: &AgentSession,
    key: &str,
) -> SendAgentMessageRequest {
    let conn = fixture.service.open().unwrap();
    let actor = fixture
        .service
        .human_actor(&conn, &actor_session.id)
        .unwrap();
    let arguments = serde_json::json!({
        "sessionRef": VibexUseRef::session(&target.id),
        "idempotencyKey": key,
        "text": "Check the current changes",
    });
    let (operation, claimed) = fixture
        .service
        .reserve_operation(
            &conn,
            &actor,
            VibexUseTool::SendMessage,
            key,
            &arguments_fingerprint(&arguments),
            &arguments,
        )
        .unwrap();
    assert!(claimed);
    team_control_message(
        target,
        key,
        MessageProvenance::DelegatedInput {
            actor_session_ref: VibexUseRef::session(&actor_session.id),
            task_ref: None,
            operation_ref: operation.operation_ref,
        },
    )
}

#[tokio::test]
async fn human_takeover_of_taskless_work_requires_a_new_control_intent_to_resume() {
    use vibex_core::MessageSubmissionStatus;
    use vibex_db::MessageSubmissionRepository;

    for external in [false, true] {
        for running in [false, true] {
            let fixture = Fixture::new();
            let parent = fixture.session("Automating parent");
            let navigation_parent = fixture.session("Navigation parent");
            let target = fixture.session("Taskless worker");
            fixture.own(
                if external {
                    &navigation_parent
                } else {
                    &parent
                },
                &target,
            );
            let access = SessionAccessRequest {
                grantee_session_id: parent.id.clone(),
                target_session_id: target.id.clone(),
                access: SessionAccess::Control,
            };
            let initial_grant = fixture
                .service
                .human_set_session_access(
                    access.clone(),
                    "initial-control".into(),
                    "human:local".into(),
                )
                .await
                .unwrap();
            if !external {
                // Exercise creation ownership without an explicit grant. The
                // earlier Control operation remains available for replay.
                fixture
                    .service
                    .human_set_session_access(
                        SessionAccessRequest {
                            access: SessionAccess::Revoke,
                            ..access.clone()
                        },
                        "keep-creation-ownership".into(),
                        "human:local".into(),
                    )
                    .await
                    .unwrap();
            }
            let mut conn = fixture.service.open().unwrap();
            assert_eq!(
                vibex_db::vibex_use_session_scope(&conn, &parent.id, &target.id).unwrap(),
                if external {
                    VibexUseScope::Controlled
                } else {
                    VibexUseScope::Owned
                }
            );
            let request = taskless_control_message(&fixture, &parent, &target, "before-takeover");
            let first = MessageSubmissionRepository::enqueue(
                &mut conn,
                MessageSubmissionId::new(),
                &request,
            )
            .unwrap();
            MessageSubmissionRepository::advance_status(
                &conn,
                &first.submission_id,
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            )
            .unwrap();
            let queued = if running {
                MessageSubmissionRepository::mark_about_to_prompt(&conn, &first.submission_id)
                    .unwrap();
                let request =
                    taskless_control_message(&fixture, &parent, &target, "next-old-round");
                let queued = MessageSubmissionRepository::enqueue(
                    &mut conn,
                    MessageSubmissionId::new(),
                    &request,
                )
                .unwrap();
                MessageSubmissionRepository::advance_status(
                    &conn,
                    &queued.submission_id,
                    MessageSubmissionStatus::AwaitingRuntime,
                    MessageSubmissionStatus::ReadyToDispatch,
                )
                .unwrap();
                queued
            } else {
                first.clone()
            };
            let automated = SessionControllerRepository::get(&conn, &target.id)
                .unwrap()
                .unwrap();
            assert!(!automated.human_controlled);
            assert!(automated.owner_task_id.is_none());

            let human_request =
                team_control_message(&target, "human-takeover", MessageProvenance::HumanInput);
            MessageSubmissionRepository::enqueue_with_mentions(
                &mut conn,
                MessageSubmissionId::new(),
                &human_request,
                "human:local",
            )
            .unwrap();
            let human = SessionControllerRepository::get(&conn, &target.id)
                .unwrap()
                .unwrap();
            assert!(human.human_controlled);
            assert!(human.revision > automated.revision);
            assert_eq!(
                MessageSubmissionRepository::mark_about_to_prompt(&conn, &queued.submission_id)
                    .unwrap_err()
                    .code,
                "vibex_use_controller_changed"
            );
            let blocked_request =
                taskless_control_message(&fixture, &parent, &target, "while-human-controls");
            assert_eq!(
                MessageSubmissionRepository::enqueue(
                    &mut conn,
                    MessageSubmissionId::new(),
                    &blocked_request,
                )
                .unwrap_err()
                .code,
                "vibex_use_controller_changed"
            );

            let recovered = fixture.reopened_service();
            let replay = recovered
                .human_set_session_access(
                    access.clone(),
                    "initial-control".into(),
                    "human:local".into(),
                )
                .await
                .unwrap();
            assert_eq!(replay, initial_grant);
            assert_eq!(
                SessionControllerRepository::get(&conn, &target.id).unwrap(),
                Some(human.clone())
            );
            let previous_grant =
                SessionGrantRepository::get(&conn, &parent.id, &target.id).unwrap();
            assert_eq!(previous_grant.is_some(), external);
            let handback = recovered
                .human_set_session_access(access, "return-control".into(), "human:local".into())
                .await
                .unwrap();
            assert!(handback.changed);
            let returned = SessionControllerRepository::get(&conn, &target.id)
                .unwrap()
                .unwrap();
            assert!(!returned.human_controlled);
            assert!(returned.revision > human.revision);
            let current_grant = SessionGrantRepository::get(&conn, &parent.id, &target.id)
                .unwrap()
                .unwrap();
            assert_eq!(current_grant.scope, "controlled");
            if let Some(previous_grant) = previous_grant {
                assert_eq!(current_grant.revision, previous_grant.revision);
            }
            // Handback authorizes new work; it cannot revive inputs admitted
            // under the controller revision that the human superseded.
            assert_eq!(
                MessageSubmissionRepository::mark_about_to_prompt(&conn, &queued.submission_id)
                    .unwrap_err()
                    .code,
                "vibex_use_controller_changed"
            );
            assert_eq!(
                MessageSubmissionRepository::get(&conn, &queued.submission_id)
                    .unwrap()
                    .unwrap()
                    .status,
                MessageSubmissionStatus::ReadyToDispatch
            );
            // Replaying the original human submission is also just a replay,
            // not a fresh takeover after the explicit return of control.
            MessageSubmissionRepository::enqueue_with_mentions(
                &mut conn,
                MessageSubmissionId::new(),
                &human_request,
                "human:local",
            )
            .unwrap();
            assert_eq!(
                SessionControllerRepository::get(&conn, &target.id).unwrap(),
                Some(returned)
            );

            if running {
                for statuses in [
                    MessageSubmissionStatus::AboutToPrompt,
                    MessageSubmissionStatus::Dispatched,
                    MessageSubmissionStatus::Completed,
                ]
                .windows(2)
                {
                    MessageSubmissionRepository::advance_status(
                        &conn,
                        &first.submission_id,
                        statuses[0],
                        statuses[1],
                    )
                    .unwrap();
                }
            }
            let fresh_request =
                taskless_control_message(&fixture, &parent, &target, "after-handback");
            let fresh = MessageSubmissionRepository::enqueue(
                &mut conn,
                MessageSubmissionId::new(),
                &fresh_request,
            )
            .unwrap();
            MessageSubmissionRepository::advance_status(
                &conn,
                &fresh.submission_id,
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            )
            .unwrap();
            MessageSubmissionRepository::mark_about_to_prompt(&conn, &fresh.submission_id).unwrap();
            assert_eq!(
                VibexUseExecutionRepository::get_by_submission(&conn, &fresh.submission_id)
                    .unwrap()
                    .unwrap()
                    .outcome,
                ExecutionOutcome::Running
            );
            assert_eq!(
                SessionOwnershipRepository::parent_of(&conn, &target.id).unwrap(),
                Some(if external {
                    navigation_parent.id
                } else {
                    parent.id
                })
            );
        }
    }
}

#[tokio::test]
async fn human_handback_preserves_an_active_task_owner_and_rolls_back_foreign_grants() {
    use vibex_db::MessageSubmissionRepository;

    let fixture = Fixture::new();
    let owner = fixture.session("Task owner");
    let foreign = fixture.session("Another parent");
    let target = fixture.session("Task worker");
    fixture.own(&owner, &target);
    let mut conn = fixture.service.open().unwrap();
    let mut task = AgentDelegation::single_turn_legacy(
        owner.id.clone(),
        "owned-active-task",
        "Review",
        "Review this change",
        Some(target.agent_id.clone()),
        AgentDelegationStatus::Starting,
        unix_timestamp_ms(),
    );
    task.root_session_id = Some(owner.id.clone());
    task.child_session_id = Some(target.id.clone());
    let task = match AgentDelegationRepository::reserve_or_get(&mut conn, &task, 8).unwrap() {
        vibex_db::AgentDelegationReservation::Claimed(task) => task,
        other => panic!("expected a new task, got {other:?}"),
    };
    SessionControllerRepository::claim(&conn, &target.id, &task.id, &owner.id, None).unwrap();
    MessageSubmissionRepository::enqueue_with_mentions(
        &mut conn,
        MessageSubmissionId::new(),
        &team_control_message(
            &target,
            "human-controls-task",
            MessageProvenance::HumanInput,
        ),
        "human:local",
    )
    .unwrap();
    let before = SessionControllerRepository::get(&conn, &target.id)
        .unwrap()
        .unwrap();
    assert!(before.human_controlled);
    let error = fixture
        .service
        .human_set_session_access(
            SessionAccessRequest {
                grantee_session_id: foreign.id.clone(),
                target_session_id: target.id.clone(),
                access: SessionAccess::Control,
            },
            "foreign-handback".into(),
            "human:local".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "vibex_use_controller_changed");
    assert_eq!(
        SessionControllerRepository::get(&conn, &target.id).unwrap(),
        Some(before.clone())
    );
    assert!(
        SessionGrantRepository::get(&conn, &foreign.id, &target.id)
            .unwrap()
            .is_none()
    );
    let restored = fixture
        .service
        .human_set_session_access(
            SessionAccessRequest {
                grantee_session_id: owner.id.clone(),
                target_session_id: target.id.clone(),
                access: SessionAccess::Control,
            },
            "return-to-task-owner".into(),
            "human:local".into(),
        )
        .await
        .unwrap();
    assert!(restored.changed);
    let after = SessionControllerRepository::get(&conn, &target.id)
        .unwrap()
        .unwrap();
    assert!(!after.human_controlled);
    assert_eq!(after.owner_task_id, Some(task.id));
    assert_eq!(after.owner_parent_session_id, Some(owner.id));
    assert!(after.revision > before.revision);
}

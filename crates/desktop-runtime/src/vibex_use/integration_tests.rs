use super::*;
use crate::sidebar_organization::{SessionGroupPresentationRequest, SidebarOrganizationBridge};
use serde_json::{Value, json};
use std::time::Duration;
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AgentMessagePayload, AgentSessionSafety, AppliedGroupLayout, ExecutionResultRange,
    GroupPresentationReply, TimelineRedactionState, TimelineSource, WorkspaceMode,
};
use vibex_db::MessageSubmissionRepository;

const TEST_AUTHORITY: &str = "integration-test";
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

struct Fixture {
    service: Arc<VibexUseService>,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let service = Self::service_at(&directory.path().join("runtime.db"));
        Self { service, directory }
    }

    fn service_at(path: &std::path::Path) -> Arc<VibexUseService> {
        let manager = Arc::new(AgentManager::new(path).unwrap());
        let catalog = Arc::new(RuntimeOptionCatalogService::new(
            manager.clone(),
            ProviderConfigService::new(path),
        ));
        VibexUseService::new(
            path,
            TEST_AUTHORITY,
            manager,
            catalog,
            SessionGroupPresentationBridge::new(),
            SidebarOrganizationBridge::new(),
            1,
        )
    }

    fn session(&self, title: &str) -> AgentSession {
        self.session_in(title, "workspace")
    }

    fn session_in(&self, title: &str, workspace_name: &str) -> AgentSession {
        let root = self.directory.path().join(workspace_name);
        std::fs::create_dir_all(&root).unwrap();
        let conn = self.service.open().unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &root, WorkspaceMode::CurrentCheckout).unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: title.to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
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

    fn actor(&self, session: &AgentSession) -> VibexUseActor {
        VibexUseActor::new(TEST_AUTHORITY, session.id.clone(), 1)
    }

    fn operation(
        &self,
        actor: &VibexUseActor,
        tool: VibexUseTool,
        key: &str,
        arguments: &Value,
    ) -> VibexUseOperation {
        let (operation, claimed) = self
            .service
            .reserve_operation(
                &self.service.open().unwrap(),
                actor,
                tool,
                key,
                &arguments_fingerprint(arguments),
                arguments,
            )
            .unwrap();
        assert!(claimed);
        operation
    }

    fn task(
        &self,
        parent: &AgentSession,
        child: &AgentSession,
        key: &str,
        follows: Option<AgentDelegationId>,
    ) -> AgentDelegation {
        let mut conn = self.service.open().unwrap();
        let mut task = AgentDelegation::single_turn_legacy(
            parent.id.clone(),
            key,
            "Review changes",
            "Check the behavior and report the result",
            Some(parent.agent_id.clone()),
            AgentDelegationStatus::Starting,
            unix_timestamp_ms(),
        );
        task.child_session_id = Some(child.id.clone());
        task.root_session_id = Some(parent.id.clone());
        task.completion_policy = DelegationCompletionPolicy::OwnerReview;
        task.follows_task_id = follows;
        let task = match AgentDelegationRepository::reserve_or_get(&mut conn, &task, 8).unwrap() {
            vibex_db::AgentDelegationReservation::Claimed(task) => task,
            other => panic!("expected a new task, got {other:?}"),
        };
        if SessionOwnershipRepository::parent_of(&conn, &child.id)
            .unwrap()
            .is_none()
        {
            SessionOwnershipRepository::upsert(&conn, &child.id, &parent.id, Some(&task.id))
                .unwrap();
        }
        task
    }

    fn enqueue(
        &self,
        actor: &VibexUseActor,
        session: &AgentSession,
        task: Option<&AgentDelegation>,
        key: &str,
    ) -> DelegationExecution {
        let operation = self.operation(
            actor,
            VibexUseTool::SendMessage,
            key,
            &json!({
                "idempotencyKey": key,
                "sessionRef": VibexUseRef::session(&session.id),
                "text": "Review the next round",
            }),
        );
        let mut conn = self.service.open().unwrap();
        let submission = MessageSubmissionRepository::enqueue(
            &mut conn,
            MessageSubmissionId::new(),
            &SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: key.to_string(),
                desired_runtime: SessionRuntimeSelection::provider(
                    session.agent_id.clone(),
                    vibex_core::ProviderProfileId::parse("provider_fixture").unwrap(),
                    "fixture-model",
                ),
                text: "Review the next round".to_string(),
                attachments: Vec::new(),
                mentions: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
                delivery: UserMessageDelivery::Prompt,
                prompt_context: None,
                provenance: MessageProvenance::DelegatedInput {
                    actor_session_ref: VibexUseRef::session(&actor.session_id),
                    task_ref: task.map(|task| VibexUseRef::task(&task.id)),
                    operation_ref: operation.operation_ref,
                },
            },
        )
        .unwrap();
        VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
            .unwrap()
            .expect("admission must commit an execution with its submission")
    }

    fn complete(&self, execution: &DelegationExecution, text: &str) -> DelegationExecution {
        let mut conn = self.service.open().unwrap();
        let session_id = execution.session_ref.session_id().unwrap();
        let sequence = TimelineRepository::latest_sequence(&conn, &session_id).unwrap() + 1;
        VibexUseExecutionRepository::mark_started(&conn, &execution.id, sequence).unwrap();
        let item = TimelineRepository::append(
            &mut conn,
            &session_id,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: text.to_string(),
                is_final: true,
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        let completed = DelegationExecution {
            outcome: ExecutionOutcome::Completed,
            stop_reason: Some("end_turn".to_string()),
            summary: Some(text.to_string()),
            end_sequence: Some(item.sequence),
            result_ranges: vec![ExecutionResultRange {
                start_sequence: item.sequence,
                end_sequence: item.sequence,
            }],
            finished_at_ms: Some(unix_timestamp_ms()),
            ..execution.clone()
        };
        let settled = vibex_db::settle_vibex_use_execution(&mut conn, &completed)
            .unwrap()
            .unwrap();
        self.service.notify_progress();
        settled
    }

    async fn call(&self, actor: &VibexUseActor, tool: VibexUseTool, arguments: Value) -> Value {
        self.service
            .call(actor.clone(), tool, arguments)
            .await
            .unwrap()
    }

    async fn read_result(
        &self,
        actor: &VibexUseActor,
        execution: &DelegationExecution,
    ) -> SessionReadPage {
        serde_json::from_value(
            self.call(
                actor,
                VibexUseTool::ReadSession,
                json!({
                    "sessionRef": execution.session_ref,
                    "executionRef": execution.execution_ref,
                    "latest": true,
                }),
            )
            .await,
        )
        .unwrap()
    }

    async fn group(
        &self,
        actor: &VibexUseActor,
        members: &[&AgentSession],
    ) -> GroupPresentationRecord {
        let value = self
            .call(
                actor,
                VibexUseTool::CreateGroup,
                json!({
                    "idempotencyKey": "create-group",
                    "name": "Review team",
                    "memberSessionRefs": members.iter()
                        .map(|session| VibexUseRef::session(&session.id)).collect::<Vec<_>>(),
                }),
            )
            .await;
        let reference = VibexUseRef::parse(value["groupRef"].as_str().unwrap()).unwrap();
        GroupPresentationRepository::get(&self.service.open().unwrap(), &reference.id)
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn reused_worker_keeps_each_round_result_and_acceptance_releases_its_controller() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let worker = fixture.session("Worker");
    let actor = fixture.actor(&parent);
    let task = fixture.task(&parent, &worker, "review-task", None);
    let first = fixture.enqueue(&actor, &worker, Some(&task), "first-round");
    let first = fixture.complete(&first, "First round result");
    let second = fixture.enqueue(&actor, &worker, Some(&task), "second-round");

    // A delayed observer for the old round cannot finish the current round or
    // move its fixed result into the new reply.
    let mut conn = fixture.service.open().unwrap();
    let delayed = DelegationExecution {
        summary: Some("Late replacement".to_string()),
        result_ranges: vec![ExecutionResultRange {
            start_sequence: 100,
            end_sequence: 200,
        }],
        ..first.clone()
    };
    vibex_db::settle_vibex_use_execution(&mut conn, &delayed).unwrap();
    let active = AgentDelegationRepository::get(&conn, &task.id)
        .unwrap()
        .unwrap();
    assert_eq!(active.phase(), DelegationTaskPhase::Active);
    assert_eq!(active.current_execution_id.as_ref(), Some(&second.id));
    let old = fixture.read_result(&actor, &first).await;
    assert_eq!(old.items.len(), 1);
    assert_eq!(old.items[0].text, "First round result");

    let second = fixture.complete(&second, "Second round result");
    let recent = fixture.read_result(&actor, &second).await;
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].text, "Second round result");
    let old = fixture.read_result(&actor, &first).await;
    assert_eq!(old.items[0].text, "First round result");
    assert_ne!(old.items[0].sequence, recent.items[0].sequence);

    let projected: TaskListPage = serde_json::from_value(
        fixture
            .call(
                &actor,
                VibexUseTool::GetTasks,
                json!({
                    "taskRefs": [VibexUseRef::task(&task.id)],
                    "includeFinished": true,
                }),
            )
            .await,
    )
    .unwrap();
    assert_eq!(projected.tasks.len(), 1);
    let reviewed = &projected.tasks[0];
    assert_eq!(reviewed.phase, DelegationTaskPhase::AwaitingReview);
    assert_eq!(reviewed.result_refs.len(), 2);
    assert_eq!(reviewed.result_refs[0].execution_ref, first.execution_ref);
    assert_eq!(reviewed.result_refs[1].execution_ref, second.execution_ref);

    let accepted = fixture
        .call(
            &actor,
            VibexUseTool::FinishTask,
            json!({
                "taskRef": VibexUseRef::task(&task.id),
                "expectedRevision": reviewed.revision,
                "outcome": "accepted",
            }),
        )
        .await;
    assert_eq!(accepted["phase"], "completed");
    assert!(
        SessionControllerRepository::get(&conn, &worker.id)
            .unwrap()
            .is_some_and(|controller| controller.owner_task_id.is_none())
    );

    let next_task = fixture.task(&parent, &worker, "follow-up-task", Some(task.id.clone()));
    let next = fixture.enqueue(&actor, &worker, Some(&next_task), "follow-up-round");
    assert_ne!(next.task_ref, second.task_ref);
    assert_eq!(next.session_ref, second.session_ref);
    assert_eq!(
        SessionOwnershipRepository::parent_of(&conn, &worker.id).unwrap(),
        Some(parent.id)
    );
    let old_task = AgentDelegationRepository::get(&conn, &task.id)
        .unwrap()
        .unwrap();
    assert_eq!(old_task.phase(), DelegationTaskPhase::Completed);
    assert_eq!(old_task.current_execution_id.as_ref(), Some(&second.id));
    let next_view = fixture.service.task_view(&conn, &next_task).unwrap();
    assert_eq!(
        next_view.follows_task_ref,
        Some(VibexUseRef::task(&task.id))
    );
    assert!(next_view.result_refs.is_empty());
}

#[tokio::test]
async fn cancellation_operation_waits_for_taskless_work_captured_after_the_task_was_cancelled() {
    let fixture = Fixture::new();
    let parent = fixture.session("Origin parent");
    let worker = fixture.session("Task worker");
    let foreign_root = fixture.session("Navigation parent");
    let external = fixture.session("Controlled worker");
    fixture.own(&foreign_root, &external);
    let actor = fixture.actor(&parent);
    let task = fixture.task(&parent, &worker, "cancel-in-stages", None);
    let own_round = fixture.enqueue(&actor, &worker, Some(&task), "own-round");
    let mut conn = fixture.service.open().unwrap();
    SessionGrantRepository::grant(&conn, &worker.id, &external.id, "controlled", "human:local")
        .unwrap();
    let taskless_round = fixture.enqueue(
        &fixture.actor(&worker),
        &external,
        None,
        "controlled-taskless-round",
    );
    for execution in [&own_round, &taskless_round] {
        MessageSubmissionRepository::advance_status(
            &conn,
            &execution.submission_id,
            vibex_core::MessageSubmissionStatus::AwaitingRuntime,
            vibex_core::MessageSubmissionStatus::ReadyToDispatch,
        )
        .unwrap();
        MessageSubmissionRepository::mark_about_to_prompt(&conn, &execution.submission_id).unwrap();
    }
    let complete_submission = |execution: &DelegationExecution| {
        let conn = fixture.service.open().unwrap();
        for (from, to) in [
            (
                vibex_core::MessageSubmissionStatus::AboutToPrompt,
                vibex_core::MessageSubmissionStatus::Dispatched,
            ),
            (
                vibex_core::MessageSubmissionStatus::Dispatched,
                vibex_core::MessageSubmissionStatus::Completed,
            ),
        ] {
            MessageSubmissionRepository::advance_status(&conn, &execution.submission_id, from, to)
                .unwrap();
        }
    };

    vibex_db::request_delegation_tree_cancellation(&mut conn, &task.id, false).unwrap();
    complete_submission(&own_round);
    assert_eq!(
        fixture.complete(&own_round, "Own round stopped").outcome,
        ExecutionOutcome::Cancelled
    );
    assert_eq!(
        AgentDelegationRepository::get(&conn, &task.id)
            .unwrap()
            .unwrap()
            .phase(),
        DelegationTaskPhase::Cancelled
    );
    assert!(
        !vibex_db::VibexUseCancellationRepository::is_requested(&conn, &taskless_round.id).unwrap()
    );

    // Persist the service operation and its captured resources without a live
    // provider: get_operation must classify these durable facts after restart.
    let arguments = json!({
        "idempotencyKey": "cancel-remaining-cascade",
        "taskRef": VibexUseRef::task(&task.id),
        "cascade": true,
    });
    let operation = fixture.operation(
        &actor,
        VibexUseTool::CancelTask,
        "cancel-remaining-cascade",
        &arguments,
    );
    vibex_db::request_delegation_tree_cancellation(&mut conn, &task.id, true).unwrap();
    fixture
        .service
        .record_operation_resource(&conn, &operation, "task", VibexUseRef::task(&task.id))
        .unwrap();
    for execution in [&own_round, &taskless_round] {
        assert!(
            vibex_db::VibexUseCancellationRepository::is_requested(&conn, &execution.id).unwrap()
        );
        fixture
            .service
            .record_operation_resource(
                &conn,
                &operation,
                "execution",
                execution.execution_ref.clone(),
            )
            .unwrap();
    }
    fixture
        .service
        .settle_operation(&conn, &operation, VibexUseOperationState::InProgress, None)
        .unwrap();

    let recovered = Fixture::service_at(&fixture.directory.path().join("runtime.db"));
    let operation_request = json!({"operationRef": operation.operation_ref});
    let pending = recovered
        .call(
            actor.clone(),
            VibexUseTool::GetOperation,
            operation_request.clone(),
        )
        .await
        .unwrap();
    assert_eq!(pending["tasks"][0]["phase"], "cancelled");
    assert_eq!(pending["state"], "in_progress");
    assert_eq!(pending["cancelled"], false);
    assert_eq!(
        VibexUseExecutionRepository::get(&conn, &taskless_round.id)
            .unwrap()
            .unwrap()
            .outcome,
        ExecutionOutcome::Running
    );

    complete_submission(&taskless_round);
    assert_eq!(
        fixture
            .complete(&taskless_round, "Captured round stopped")
            .outcome,
        ExecutionOutcome::Cancelled
    );
    let settled = recovered
        .call(actor.clone(), VibexUseTool::GetOperation, operation_request)
        .await
        .unwrap();
    assert_eq!(settled["state"], "succeeded");
    assert_eq!(settled["cancelled"], true);

    let noop_arguments = json!({
        "idempotencyKey": "cancel-already-stopped",
        "taskRef": VibexUseRef::task(&task.id),
        "cascade": true,
    });
    let noop = fixture.operation(
        &actor,
        VibexUseTool::CancelTask,
        "cancel-already-stopped",
        &noop_arguments,
    );
    let response = fixture
        .call(&actor, VibexUseTool::CancelTask, noop_arguments)
        .await;
    assert_eq!(response["cancelled"], false);
    let replay = fixture
        .call(
            &actor,
            VibexUseTool::GetOperation,
            json!({"operationRef": noop.operation_ref}),
        )
        .await;
    assert_eq!(replay["state"], "succeeded");
    assert_eq!(replay["cancelled"], false);
    assert_eq!(
        SessionOwnershipRepository::parent_of(&conn, &external.id).unwrap(),
        Some(foreign_root.id)
    );
}

#[tokio::test]
async fn session_only_wait_uses_its_originating_team_and_ignores_another_target() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let target = fixture.session("Granted worker");
    let unrelated = fixture.session("Another worker");
    let actor = fixture.actor(&parent);
    fixture.own(&parent, &unrelated);
    SessionGrantRepository::grant(
        &fixture.service.open().unwrap(),
        &parent.id,
        &target.id,
        "controlled",
        "human:local",
    )
    .unwrap();
    let execution = fixture.enqueue(&actor, &target, None, "session-only");
    let another = fixture.enqueue(&actor, &unrelated, None, "other-session");
    fixture.complete(&another, "An unrelated result");

    let request = json!({"sessionRefs": [VibexUseRef::session(&target.id)], "timeoutMs": 0});
    let before: WaitResponse = serde_json::from_value(
        fixture
            .call(&actor, VibexUseTool::Wait, request.clone())
            .await,
    )
    .unwrap();
    assert_eq!(before.outcome, WaitOutcome::TimedOut);
    assert!(before.tasks.is_empty());
    assert_eq!(before.executions.len(), 1);
    assert_eq!(before.executions[0].id, execution.id);
    assert!(
        before
            .events
            .iter()
            .all(|event| event.session_ref.as_ref() == Some(&execution.session_ref))
    );

    let settled = fixture.complete(&execution, "The requested result");
    let after: WaitResponse =
        serde_json::from_value(fixture.call(&actor, VibexUseTool::Wait, request).await).unwrap();
    assert_eq!(after.outcome, WaitOutcome::Settled);
    assert!(!after.more_expected);
    assert_eq!(after.executions[0].summary, settled.summary);
    assert!(
        after
            .events
            .iter()
            .any(|event| event.kind == DelegationTaskEventKind::ExecutionFinished)
    );
    assert!(
        after
            .events
            .iter()
            .all(|event| event.session_ref.as_ref() == Some(&execution.session_ref))
    );
    assert_eq!(
        settled.root_session_ref,
        Some(VibexUseRef::session(&parent.id))
    );
    assert!(
        SessionOwnershipRepository::parent_of(&fixture.service.open().unwrap(), &target.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn resolved_blocked_events_remain_readable_without_waking_attention_again() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let worker = fixture.session("Worker");
    let actor = fixture.actor(&parent);
    let task = fixture.task(&parent, &worker, "blocked-task", None);
    let execution = fixture.enqueue(&actor, &worker, Some(&task), "blocked-round");
    let blocked = DelegationBlockedOn::Permission {
        request_id: "permission-one".to_string(),
    };
    let conn = fixture.service.open().unwrap();
    VibexUseExecutionRepository::set_blocked_on(&conn, &execution.id, Some(&blocked)).unwrap();
    VibexUseEventRepository::append(
        &conn,
        "blocked-event",
        Some(&parent.id),
        DelegationTaskEventKind::TaskBlocked,
        Some(&task.id),
        Some(&worker.id),
        1,
        &json!({"executionRef": execution.execution_ref, "blockedOn": blocked}),
    )
    .unwrap();
    let request = json!({"sessionRefs": [execution.session_ref], "timeoutMs": 0});
    let attention: WaitResponse = serde_json::from_value(
        fixture
            .call(&actor, VibexUseTool::Wait, request.clone())
            .await,
    )
    .unwrap();
    assert_eq!(attention.outcome, WaitOutcome::Attention);

    VibexUseExecutionRepository::set_blocked_on(&conn, &execution.id, None).unwrap();
    let resolved: WaitResponse =
        serde_json::from_value(fixture.call(&actor, VibexUseTool::Wait, request).await).unwrap();
    assert_eq!(resolved.outcome, WaitOutcome::TimedOut);
    assert!(
        resolved
            .events
            .iter()
            .any(|event| event.event_id == "blocked-event")
    );
    assert!(resolved.more_expected);
}

#[tokio::test]
async fn event_pages_hide_siblings_and_only_delivered_events_can_be_acknowledged() {
    let fixture = Fixture::new();
    let root = fixture.session("Root");
    let reader = fixture.session("Reader");
    let sibling = fixture.session("Sibling");
    fixture.own(&root, &reader);
    fixture.own(&root, &sibling);
    let actor = fixture.actor(&reader);
    let conn = fixture.service.open().unwrap();
    for (id, target) in [
        ("hidden-first", &sibling),
        ("visible-first", &reader),
        ("hidden-second", &sibling),
        ("visible-second", &reader),
    ] {
        VibexUseEventRepository::append(
            &conn,
            id,
            Some(&root.id),
            DelegationTaskEventKind::ExecutionFinished,
            None,
            Some(&target.id),
            1,
            &json!({"summary": id}),
        )
        .unwrap();
    }
    let all = fixture
        .call(&actor, VibexUseTool::GetEvents, json!({}))
        .await;
    assert_eq!(event_ids(&all), vec!["visible-first", "visible-second"]);
    assert!(
        all["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["delivered"] == false && event["acknowledged"] == false)
    );
    let cursor = all["nextEventCursor"].as_i64().unwrap();
    let before_delivery = fixture
        .call(
            &actor,
            VibexUseTool::AckEvents,
            json!({
                "eventIds": ["visible-first", "visible-second", "hidden-first", "hidden-second"],
                "throughEventCursor": cursor,
            }),
        )
        .await;
    assert_eq!(before_delivery["acknowledged"], 0);

    let first = fixture
        .call(&actor, VibexUseTool::GetEvents, json!({"maxItems": 1}))
        .await;
    assert_eq!(event_ids(&first), vec!["visible-first"]);
    assert_eq!(first["events"][0]["delivered"], false);
    let observability = fixture.service.manager.observability();
    let delivery_count = |result| {
        observability
            .snapshot()
            .series
            .into_iter()
            .filter(|metric| {
                metric.name == RuntimeMetricName::DelegationDelivery && metric.result == result
            })
            .map(|metric| metric.count)
            .sum::<u64>()
    };
    assert_eq!(delivery_count(RuntimeMetricResult::Success), 0);
    assert_eq!(delivery_count(RuntimeMetricResult::Reused), 0);
    fixture
        .service
        .response_delivered(&actor, VibexUseTool::GetEvents, &first)
        .unwrap();
    assert_eq!(delivery_count(RuntimeMetricResult::Success), 1);
    assert_eq!(delivery_count(RuntimeMetricResult::Reused), 0);
    let repeated = fixture
        .call(&actor, VibexUseTool::GetEvents, json!({"maxItems": 1}))
        .await;
    assert_eq!(event_ids(&repeated), vec!["visible-first"]);
    assert_eq!(repeated["events"][0]["delivered"], true);
    assert_eq!(repeated["events"][0]["acknowledged"], false);
    fixture
        .service
        .response_delivered(&actor, VibexUseTool::GetEvents, &repeated)
        .unwrap();
    assert_eq!(delivery_count(RuntimeMetricResult::Success), 1);
    assert_eq!(delivery_count(RuntimeMetricResult::Reused), 1);
    let ack = fixture
        .call(
            &actor,
            VibexUseTool::AckEvents,
            json!({"throughEventCursor": cursor}),
        )
        .await;
    assert_eq!(ack["acknowledged"], 1);
    let second = fixture
        .call(&actor, VibexUseTool::GetEvents, json!({"maxItems": 1}))
        .await;
    assert_eq!(event_ids(&second), vec!["visible-second"]);
    assert_eq!(second["events"][0]["delivered"], false);
    let hidden_ack = fixture
        .call(
            &actor,
            VibexUseTool::AckEvents,
            json!({
                "eventIds": ["hidden-first", "hidden-second", "visible-second"],
            }),
        )
        .await;
    assert_eq!(hidden_ack["acknowledged"], 0);
    let history = fixture
        .call(
            &actor,
            VibexUseTool::GetEvents,
            json!({
                "includeAcknowledged": true,
            }),
        )
        .await;
    assert_eq!(event_ids(&history), vec!["visible-first", "visible-second"]);
    assert_eq!(history["events"][0]["delivered"], true);
    assert_eq!(history["events"][0]["acknowledged"], true);
    assert_eq!(history["events"][1]["delivered"], false);
}

#[tokio::test]
async fn wait_reaches_a_result_beyond_a_full_page_of_non_settling_events() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let worker = fixture.session("Worker");
    let actor = fixture.actor(&parent);
    fixture.own(&parent, &worker);
    let execution = fixture.enqueue(&actor, &worker, None, "paged-wait");
    let initial = fixture
        .call(&actor, VibexUseTool::GetEvents, json!({}))
        .await;
    let cursor = initial["nextEventCursor"].as_i64().unwrap();
    assert!(cursor > 0);
    let conn = fixture.service.open().unwrap();
    for index in 0..60 {
        VibexUseEventRepository::append(
            &conn,
            &format!("layout-progress-{index}"),
            Some(&parent.id),
            DelegationTaskEventKind::GroupMembershipChanged,
            None,
            Some(&worker.id),
            index,
            &json!({}),
        )
        .unwrap();
    }
    fixture.complete(&execution, "A result after many progress events");
    let response: WaitResponse = serde_json::from_value(
        fixture
            .call(
                &actor,
                VibexUseTool::Wait,
                json!({
                    "sessionRefs": [execution.session_ref],
                    "afterEventCursor": cursor,
                    "timeoutMs": 100,
                }),
            )
            .await,
    )
    .unwrap();
    assert_eq!(response.outcome, WaitOutcome::Settled);
    assert_eq!(response.executions[0].id, execution.id);
    assert_eq!(response.executions[0].outcome, ExecutionOutcome::Completed);
    assert!(response.event_cursor > cursor);
    assert!(response.events.len() <= 50);
    let next: WaitResponse = serde_json::from_value(
        fixture
            .call(
                &actor,
                VibexUseTool::Wait,
                json!({
                    "sessionRefs": [execution.session_ref],
                    "afterEventCursor": response.event_cursor,
                    "timeoutMs": 0,
                }),
            )
            .await,
    )
    .unwrap();
    assert_eq!(next.outcome, WaitOutcome::Settled);
    let ids = response
        .events
        .iter()
        .chain(&next.events)
        .map(|event| event.event_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 61, "progress pages must remain lossless");
    assert!(
        next.events
            .iter()
            .any(|event| event.kind == DelegationTaskEventKind::ExecutionFinished)
    );
    assert!(next.event_cursor > cursor + 50);
}

#[tokio::test]
async fn wait_reports_failure_after_a_nonzero_event_cursor() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let worker = fixture.session("Worker");
    let actor = fixture.actor(&parent);
    fixture.own(&parent, &worker);
    let execution = fixture.enqueue(&actor, &worker, None, "failed-wait");
    let initial = fixture
        .call(&actor, VibexUseTool::GetEvents, json!({}))
        .await;
    let cursor = initial["nextEventCursor"].as_i64().unwrap();
    assert!(cursor > 0);
    let mut conn = fixture.service.open().unwrap();
    let failed = DelegationExecution {
        outcome: ExecutionOutcome::Failed,
        error_code: Some("fixture_execution_failed".to_string()),
        finished_at_ms: Some(unix_timestamp_ms()),
        ..execution.clone()
    };
    vibex_db::settle_vibex_use_execution(&mut conn, &failed).unwrap();
    let response: WaitResponse = serde_json::from_value(
        fixture
            .call(
                &actor,
                VibexUseTool::Wait,
                json!({
                    "sessionRefs": [execution.session_ref],
                    "afterEventCursor": cursor,
                    "timeoutMs": 0,
                }),
            )
            .await,
    )
    .unwrap();
    assert_eq!(response.outcome, WaitOutcome::Failed);
    assert_eq!(response.executions[0].outcome, ExecutionOutcome::Failed);
    assert!(!response.more_expected);
    assert!(response.event_cursor > cursor);
}

fn event_ids(value: &Value) -> Vec<&str> {
    value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["eventId"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn cross_workspace_group_members_are_rejected_before_any_group_is_saved() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let foreign = fixture.session_in("Other workspace", "other-workspace");
    fixture.own(&parent, &foreign);
    let actor = fixture.actor(&parent);
    let error = fixture.service.call(actor.clone(), VibexUseTool::CreateGroup, json!({
        "idempotencyKey": "cross-workspace", "name": "Invalid group",
        "memberSessionRefs": [VibexUseRef::session(&parent.id), VibexUseRef::session(&foreign.id)],
    })).await.unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.key == "reason"
                && diagnostic.value == "cross_workspace_unsupported")
    );
    assert!(
        GroupPresentationRepository::list_for_actor(
            &fixture.service.open().unwrap(),
            parent.id.as_str(),
            50,
        )
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn automatic_group_membership_refreshes_the_accepted_worker_resources() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let worker = fixture.session("Actual worker");
    fixture.own(&parent, &worker);
    let actor = fixture.actor(&parent);
    let arguments = json!({
        "idempotencyKey": "automatic-group", "task": {"prompt": "Review"},
        "presentation": {"groupName": "Review team", "present": false},
    });
    let operation = fixture.operation(
        &actor,
        VibexUseTool::Delegate,
        "automatic-group",
        &arguments,
    );
    let conn = fixture.service.open().unwrap();
    for session in [&worker, &parent] {
        fixture
            .service
            .record_operation_resource(
                &conn,
                &operation,
                "session",
                VibexUseRef::session(&session.id),
            )
            .unwrap();
    }
    assert!(operation.resources.is_empty());
    let outcome = fixture
        .service
        .apply_presentation(&actor, &operation, &arguments)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.state, PresentationState::Prepared);
    let record = GroupPresentationRepository::get(&conn, &outcome.group_ref.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.member_session_ids,
        vec![parent.id.clone(), worker.id.clone()]
    );
    assert_eq!(
        record.layout.lead_session_ref,
        Some(VibexUseRef::session(&parent.id))
    );
    assert_eq!(record.layout.preferred_live_panes, Some(2));
    let stored = VibexUseOperationRepository::get(&conn, &operation.id)
        .unwrap()
        .unwrap();
    assert!(
        stored
            .resources
            .iter()
            .any(|resource| resource.reference == outcome.group_ref)
    );
}

async fn next_group_command(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<SessionGroupPresentationRequest>,
) -> (
    GroupPresentationCommand,
    tokio::sync::oneshot::Sender<VibexResult<GroupPresentationReply>>,
) {
    let request = tokio::time::timeout(TEST_TIMEOUT, receiver.recv())
        .await
        .expect("the runtime should offer its saved group")
        .unwrap();
    match request {
        SessionGroupPresentationRequest::Apply { command, reply } => (*command, reply),
        _ => panic!("expected a group apply request"),
    }
}

fn applied_reply(command: &GroupPresentationCommand, revision: u64) -> GroupPresentationReply {
    GroupPresentationReply {
        state: PresentationState::Applied,
        revision,
        reason: None,
        message: None,
        applied_layout: Some(AppliedGroupLayout {
            preset: command.layout.preset,
            visible_session_refs: command.member_session_refs.clone(),
            tabbed_session_refs: Vec::new(),
            live_panes: command.member_session_refs.len(),
        }),
    }
}

#[tokio::test]
async fn manual_group_edits_release_ownership_and_fence_a_late_shell_reply() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let worker = fixture.session("Worker");
    fixture.own(&parent, &worker);
    let actor = fixture.actor(&parent);
    let original = fixture.group(&actor, &[&parent, &worker]).await;
    let mut shell = fixture.service.presentation.attach();
    let service = fixture.service.clone();
    let updating_actor = actor.clone();
    let group_ref = VibexUseRef::group(&original.group_id);
    let update_ref = group_ref.clone();
    let update =
        tokio::spawn(async move {
            service.call(updating_actor, VibexUseTool::UpdateGroup, json!({
            "idempotencyKey": "agent-update", "groupRef": update_ref,
            "name": "Agent rename", "expectedRevision": original.presentation_revision(),
        })).await
        });
    let (command, reply) = next_group_command(&mut shell).await;
    let manual_layout = SessionGroupLayoutIntent {
        preset: SessionGroupLayoutPreset::Tabs,
        lead_session_ref: Some(VibexUseRef::session(&parent.id)),
        preferred_live_panes: Some(1),
    };
    fixture
        .service
        .sync_user_group(
            &group_ref.id,
            "My arrangement",
            std::slice::from_ref(&parent.id),
            &manual_layout,
            77,
        )
        .unwrap();
    let manual = GroupPresentationRepository::get(&fixture.service.open().unwrap(), &group_ref.id)
        .unwrap()
        .unwrap();
    assert!(!manual.created_by_caller);
    assert_eq!(manual.client_revision, Some(77));
    reply.send(Ok(applied_reply(&command, 22))).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, update)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let after = GroupPresentationRepository::get(&fixture.service.open().unwrap(), &group_ref.id)
        .unwrap()
        .unwrap();
    assert_eq!(after, manual);
    assert_eq!(after.name, "My arrangement");
    assert_eq!(after.layout, manual_layout);
    assert!(
        fixture
            .service
            .call(
                actor,
                VibexUseTool::UpdateGroup,
                json!({
                    "idempotencyKey": "after-release", "groupRef": group_ref,
                    "name": "Another Agent rename", "expectedRevision": 77,
                })
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn applied_groups_are_restored_after_the_runtime_and_shell_reconnect() {
    let fixture = Fixture::new();
    let parent = fixture.session("Parent");
    let actor = fixture.actor(&parent);
    let group = fixture.group(&actor, &[&parent]).await;
    let mut first_shell = fixture.service.presentation.attach();
    let service = fixture.service.clone();
    let first_recovery = tokio::spawn(async move { service.recover_presentations().await });
    let (first, reply) = next_group_command(&mut first_shell).await;
    assert_eq!(first.group_id.as_str(), group.group_id);
    assert!(first.presentation_only);
    reply.send(Ok(applied_reply(&first, 41))).unwrap();
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, first_recovery)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        1
    );
    fixture.service.presentation.detach();
    drop(first_shell);

    let restarted = Fixture::service_at(&fixture.directory.path().join("runtime.db"));
    let stored = GroupPresentationRepository::get(&restarted.open().unwrap(), &group.group_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, GroupPresentationState::Applied);
    let mut next_shell = restarted.presentation.attach();
    let service = restarted.clone();
    let recovery = tokio::spawn(async move { service.recover_presentations().await });
    let (command, reply) = next_group_command(&mut next_shell).await;
    assert_eq!(command.group_id, first.group_id);
    assert_eq!(command.member_session_refs, first.member_session_refs);
    assert_eq!(command.expected_revision, Some(41));
    assert!(command.presentation_only);
    assert!(!command.present);
    assert_eq!(
        command.activation_policy,
        PresentationActivationPolicy::WhenUserReturns
    );
    reply.send(Ok(applied_reply(&command, 42))).unwrap();
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, recovery)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        1
    );
    let recovered = GroupPresentationRepository::get(&restarted.open().unwrap(), &group.group_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.client_revision, Some(42));
    assert_eq!(
        GroupPresentationRepository::list_for_actor(
            &restarted.open().unwrap(),
            parent.id.as_str(),
            50
        )
        .unwrap()
        .len(),
        1
    );
}

use super::*;
use crate::{
    AgentDelegationRepository, AgentDelegationReservation, AgentUsageRepository,
    MessageSubmissionRepository, SessionRepository, WorkspaceRepository, apply_migrations,
    open_database,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;
use vibex_core::{
    AgentDelegationStatus, AgentId, AgentSession, AgentSessionSafety, AgentSessionState,
    AgentUsageCounterOrigin, AgentUsageCounterScope, AgentUsageExecution, AgentUsageObservation,
    AgentUsageObservationSource, AgentUsageStreamAttribution, AgentUsageTokenValues,
    MessageSubmissionStatus, ProviderProfileId, RuntimeAuthSource, RuntimeBindingId,
    SendAgentMessageRequest, SessionRuntimeSelection, UsageExecutionId, UserMessageDelivery,
    VibexUseBudgetPolicy, VibexUseBudgetPreset, VibexUseBudgetSettings, WorkspaceMode,
};

struct Fixture {
    directory: PathBuf,
    database: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("vibex-budget-tests-{}", VibexSessionId::new(),));
        std::fs::create_dir_all(directory.join("workspace")).unwrap();
        let database = directory.join("runtime.db");
        let mut conn = open_database(&database).unwrap();
        apply_migrations(&mut conn).unwrap();
        Self {
            directory,
            database,
        }
    }

    fn open(&self) -> Connection {
        let conn = open_database(&self.database).unwrap();
        conn.busy_timeout(Duration::from_secs(2)).unwrap();
        conn
    }

    fn session(&self, title: &str, agent: &str) -> AgentSession {
        let conn = self.open();
        let (project, workspace) = WorkspaceRepository::ensure(
            &conn,
            self.directory.join("workspace"),
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
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse(agent).unwrap(),
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
        SessionOwnershipRepository::upsert(&self.open(), &child.id, &parent.id, None).unwrap();
    }

    fn policy(&self, root_limit: u32, agent_limit: u32) -> VibexUseBudgetPolicy {
        let policy = VibexUseBudgetPolicy {
            root_execution_limit: root_limit,
            per_agent_execution_limit: agent_limit,
            ..VibexUseBudgetPolicy::default()
        };
        VibexUseBudgetRepository::set_policy(&self.open(), &policy).unwrap();
        policy
    }

    fn task(&self, parent: &AgentSession, child: &AgentSession, key: &str) -> AgentDelegation {
        let mut task = AgentDelegation::single_turn_legacy(
            parent.id.clone(),
            key,
            "Budgeted task",
            "Run a bounded check",
            Some(child.agent_id.clone()),
            AgentDelegationStatus::Starting,
            unix_timestamp_ms(),
        );
        task.child_session_id = Some(child.id.clone());
        task.root_session_id = Some(vibex_use_root_session(&self.open(), &parent.id).unwrap());
        task
    }

    fn reserve(&self, task: &AgentDelegation) -> VibexResult<AgentDelegationReservation> {
        AgentDelegationRepository::reserve_or_get(&mut self.open(), task, u32::MAX)
    }

    fn request(
        &self,
        actor: &AgentSession,
        target: &AgentSession,
        task: Option<&AgentDelegation>,
        key: &str,
    ) -> SendAgentMessageRequest {
        let id = VibexOperationId::new();
        let now = unix_timestamp_ms();
        let operation = VibexUseOperation {
            operation_ref: VibexUseRef::operation(&id),
            id,
            authority: "budget-test".to_string(),
            actor_key: actor.id.to_string(),
            tool: "vibex_send_message".to_string(),
            caller_key: key.to_string(),
            payload_fingerprint: key.to_string(),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: BTreeMap::new(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        assert!(matches!(
            VibexUseOperationRepository::reserve(&self.open(), &operation, key).unwrap(),
            VibexUseOperationReservation::Claimed(_),
        ));
        SendAgentMessageRequest {
            session_id: target.id.clone(),
            message_idempotency_key: key.to_string(),
            desired_runtime: SessionRuntimeSelection::provider(
                target.agent_id.clone(),
                ProviderProfileId::parse("provider_fixture").unwrap(),
                "fixture-model",
            ),
            text: "Run the next round".to_string(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
            delivery: UserMessageDelivery::Prompt,
            prompt_context: None,
            provenance: MessageProvenance::DelegatedInput {
                actor_session_ref: VibexUseRef::session(&actor.id),
                task_ref: task.map(|task| VibexUseRef::task(&task.id)),
                operation_ref: operation.operation_ref,
            },
        }
    }

    fn execution(
        &self,
        root: &AgentSession,
        worker: &AgentSession,
        key: &str,
    ) -> DelegationExecution {
        self.own(root, worker);
        let request = self.request(root, worker, None, key);
        let mut conn = self.open();
        let submission =
            MessageSubmissionRepository::enqueue(&mut conn, MessageSubmissionId::new(), &request)
                .unwrap();
        VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
            .unwrap()
            .unwrap()
    }

    fn usage(
        &self,
        worker: &AgentSession,
        execution: &DelegationExecution,
        tokens: AgentUsageTokenValues,
    ) {
        let mut conn = self.open();
        let now = unix_timestamp_ms();
        let usage = AgentUsageExecution {
            usage_execution_id: UsageExecutionId::new(),
            message_submission_id: Some(execution.submission_id.clone()),
            project_id: worker.project_id.clone(),
            workspace_id: worker.workspace_id.clone(),
            stream: AgentUsageStreamAttribution {
                session_id: worker.id.clone(),
                binding_id: RuntimeBindingId::new(),
                activation_generation: 1,
                agent_id: worker.agent_id.clone(),
                auth_source: RuntimeAuthSource::provider_profile(
                    ProviderProfileId::parse("provider_fixture").unwrap(),
                ),
                auth_source_revision: 1,
                model_id: Some("fixture-model".to_string()),
            },
            dispatched_at_ms: now,
        };
        AgentUsageRepository::record_execution(&conn, &usage).unwrap();
        if tokens.any_reported() {
            AgentUsageRepository::apply_observation(
                &mut conn,
                &AgentUsageObservation {
                    stream: usage.stream.clone(),
                    execution: Some(usage),
                    counter_origin: AgentUsageCounterOrigin::Unknown,
                    counter_scope: AgentUsageCounterScope::Turn,
                    observation_sequence: 1,
                    cumulative: tokens,
                    context_window_used_tokens: None,
                    context_window_size_tokens: None,
                    source: AgentUsageObservationSource::PromptResponse,
                    observed_at_ms: now + 1,
                },
            )
            .unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Each contender owns an independent SQLite connection. The barrier starts
/// admission together, without relying on a service-local mutex or sleeps.
fn race<R: Send + 'static, T: Send + 'static>(
    fixture: &Fixture,
    requests: Vec<R>,
    operation: fn(&mut Connection, R) -> VibexResult<T>,
) -> Vec<VibexResult<T>> {
    let count = requests.len();
    let barrier = Arc::new(Barrier::new(count + 1));
    let (sender, receiver) = mpsc::channel();
    let mut workers = Vec::new();
    for request in requests {
        let mut conn = fixture.open();
        let barrier = barrier.clone();
        let sender = sender.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            sender.send(operation(&mut conn, request)).unwrap();
        }));
    }
    drop(sender);
    barrier.wait();
    let results = (0..count)
        .map(|_| {
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("bounded admission should finish for every contender")
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    results
}

fn reserve(
    conn: &mut Connection,
    task: AgentDelegation,
) -> VibexResult<AgentDelegationReservation> {
    AgentDelegationRepository::reserve_or_get(conn, &task, u32::MAX)
}

#[test]
fn concurrent_task_reservations_cannot_overbook_one_root_slot() {
    let fixture = Fixture::new();
    fixture.policy(1, 64);
    let root = fixture.session("Root", "codex");
    let first = fixture.session("First worker", "codex");
    let second = fixture.session("Second worker", "claude");
    let requests = vec![
        fixture.task(&root, &first, "first"),
        fixture.task(&root, &second, "second"),
    ];
    let results = race(&fixture, requests.clone(), reserve);
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error.code.as_str())
            .collect::<Vec<_>>(),
        vec!["vibex_use_root_budget_exceeded"]
    );
    let conn = fixture.open();
    assert_eq!(
        count_active_executions_for_root(&conn, &root.id).unwrap(),
        1
    );
    for request in requests {
        let saved = AgentDelegationRepository::get(&conn, &request.id).unwrap();
        assert_eq!(
            VibexUseBudgetRepository::task_deadline(&conn, &request.id)
                .unwrap()
                .is_some(),
            saved.is_some()
        );
        let controller =
            SessionControllerRepository::get(&conn, request.child_session_id.as_ref().unwrap())
                .unwrap();
        assert_eq!(
            controller.and_then(|controller| controller.owner_task_id),
            saved.map(|task| task.id)
        );
    }
}

#[test]
fn concurrent_roots_share_the_configured_per_agent_limit() {
    let fixture = Fixture::new();
    fixture.policy(64, 1);
    let first_root = fixture.session("First root", "codex");
    let second_root = fixture.session("Second root", "codex");
    let first = fixture.session("First worker", "codex");
    let second = fixture.session("Second worker", "codex");
    let results = race(
        &fixture,
        vec![
            fixture.task(&first_root, &first, "first"),
            fixture.task(&second_root, &second, "second"),
        ],
        reserve,
    );
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error.code.as_str())
            .collect::<Vec<_>>(),
        vec!["vibex_use_agent_budget_exceeded"]
    );
    assert_eq!(
        VibexUseBudgetRepository::active_for_agent(&fixture.open(), &first.agent_id).unwrap(),
        1
    );
    let other_agent = fixture.session("Another Agent worker", "claude");
    assert!(
        fixture
            .reserve(&fixture.task(&first_root, &other_agent, "other-agent"))
            .is_ok()
    );
    assert_eq!(
        VibexUseBudgetRepository::active_for_agent(&fixture.open(), &other_agent.agent_id).unwrap(),
        1
    );
}

#[test]
fn concurrent_taskless_enqueues_reserve_their_execution_slot_atomically() {
    let fixture = Fixture::new();
    fixture.policy(1, 64);
    let root = fixture.session("Root", "codex");
    let first = fixture.session("First worker", "codex");
    let second = fixture.session("Second worker", "codex");
    fixture.own(&root, &first);
    fixture.own(&root, &second);
    let requests = vec![
        fixture.request(&root, &first, None, "first-input"),
        fixture.request(&root, &second, None, "second-input"),
    ];
    let results = race(&fixture, requests, |conn, request| {
        MessageSubmissionRepository::enqueue(conn, MessageSubmissionId::new(), &request)
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error.code.as_str())
            .collect::<Vec<_>>(),
        vec!["vibex_use_root_budget_exceeded"]
    );
    assert_eq!(
        count_active_executions_for_root(&fixture.open(), &root.id).unwrap(),
        1
    );
    assert_eq!(
        VibexUseExecutionRepository::list_open(&fixture.open())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn taskless_control_uses_the_origin_budget_without_reparenting_navigation() {
    let fixture = Fixture::new();
    fixture.policy(1, 64);
    let origin = fixture.session("Origin team", "codex");
    let navigation_root = fixture.session("Navigation root", "codex");
    let branch = fixture.session("Navigation branch", "codex");
    let worker = fixture.session("Controlled worker", "codex");
    fixture.own(&navigation_root, &branch);
    fixture.own(&branch, &worker);
    let mut conn = fixture.open();
    SessionGrantRepository::grant(&conn, &origin.id, &worker.id, "controlled", "human:local")
        .unwrap();
    let request = fixture.request(&origin, &worker, None, "controlled-input");
    let submission =
        MessageSubmissionRepository::enqueue(&mut conn, MessageSubmissionId::new(), &request)
            .unwrap();
    assert_eq!(
        vibex_use_root_session(&conn, &worker.id).unwrap(),
        navigation_root.id
    );
    MessageSubmissionRepository::advance_status(
        &conn,
        &submission.submission_id,
        MessageSubmissionStatus::AwaitingRuntime,
        MessageSubmissionStatus::ReadyToDispatch,
    )
    .unwrap();
    MessageSubmissionRepository::mark_about_to_prompt(&conn, &submission.submission_id).unwrap();
    let execution =
        VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
            .unwrap()
            .unwrap();
    assert_eq!(execution.outcome, ExecutionOutcome::Running);
    assert_eq!(
        execution.root_session_ref,
        Some(VibexUseRef::session(&origin.id))
    );
    assert_eq!(
        vibex_use_root_session(&conn, &worker.id).unwrap(),
        origin.id
    );
    assert_eq!(vibex_use_delegation_depth(&conn, &worker.id).unwrap(), 1);
    assert_eq!(
        SessionOwnershipRepository::ancestor_depth(&conn, &worker.id).unwrap(),
        2
    );
    assert_eq!(
        count_active_executions_for_root(&conn, &origin.id).unwrap(),
        1
    );
    assert_eq!(
        count_active_executions_for_root(&conn, &navigation_root.id).unwrap(),
        0
    );

    let child = fixture.session("Created for the origin team", "codex");
    fixture.own(&worker, &child);
    assert_eq!(vibex_use_root_session(&conn, &child.id).unwrap(), origin.id);
    let child_request = fixture.request(&worker, &child, None, "over-origin-budget");
    assert_eq!(
        MessageSubmissionRepository::enqueue(
            &mut conn,
            MessageSubmissionId::new(),
            &child_request,
        )
        .unwrap_err()
        .code,
        "vibex_use_root_budget_exceeded"
    );
    let completed = DelegationExecution {
        outcome: ExecutionOutcome::Completed,
        finished_at_ms: Some(unix_timestamp_ms()),
        ..execution
    };
    settle_vibex_use_execution(&mut conn, &completed).unwrap();
    assert_eq!(
        vibex_use_root_session(&conn, &worker.id).unwrap(),
        navigation_root.id
    );
    assert_eq!(vibex_use_root_session(&conn, &child.id).unwrap(), origin.id);
    assert_eq!(
        SessionOwnershipRepository::parent_of(&conn, &worker.id).unwrap(),
        Some(branch.id)
    );
    assert_eq!(
        count_active_executions_for_root(&conn, &origin.id).unwrap(),
        0
    );

    let followup = fixture.request(&origin, &child, None, "continue-created-work");
    let submission =
        MessageSubmissionRepository::enqueue(&mut conn, MessageSubmissionId::new(), &followup)
            .unwrap();
    let execution =
        VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
            .unwrap()
            .unwrap();
    assert_eq!(
        execution.root_session_ref,
        Some(VibexUseRef::session(&origin.id))
    );
    assert_eq!(
        count_active_executions_for_root(&conn, &origin.id).unwrap(),
        1
    );
    assert_eq!(
        count_active_executions_for_root(&conn, &navigation_root.id).unwrap(),
        0
    );
}

#[test]
fn a_reserved_task_slot_is_consumed_once_when_its_first_input_is_enqueued() {
    let fixture = Fixture::new();
    fixture.policy(1, 1);
    let root = fixture.session("Root", "codex");
    let worker = fixture.session("Worker", "codex");
    fixture.own(&root, &worker);
    let task = fixture.task(&root, &worker, "task");
    fixture.reserve(&task).unwrap();
    let request = fixture.request(&root, &worker, Some(&task), "first-input");
    let conn = fixture.open();
    assert_eq!(
        count_active_executions_for_root(&conn, &root.id).unwrap(),
        1
    );
    MessageSubmissionRepository::enqueue(&mut fixture.open(), MessageSubmissionId::new(), &request)
        .unwrap();
    assert_eq!(
        count_active_executions_for_root(&conn, &root.id).unwrap(),
        1
    );
    assert_eq!(
        VibexUseBudgetRepository::active_for_agent(&conn, &worker.agent_id).unwrap(),
        1
    );
    let next = fixture.request(&root, &worker, Some(&task), "second-input");
    let error = MessageSubmissionRepository::enqueue(
        &mut fixture.open(),
        MessageSubmissionId::new(),
        &next,
    )
    .unwrap_err();
    assert_eq!(error.code, "vibex_use_root_budget_exceeded");
    assert_eq!(
        VibexUseExecutionRepository::list_for_task(&conn, &task.id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn expired_task_deadlines_reject_enqueue_dispatch_and_descendant_admission() {
    let fixture = Fixture::new();
    fixture.policy(64, 64);
    let root = fixture.session("Root", "codex");
    let worker = fixture.session("Worker", "codex");
    fixture.own(&root, &worker);
    let task = fixture.task(&root, &worker, "timed-task");
    fixture.reserve(&task).unwrap();
    let original_deadline = VibexUseBudgetRepository::task_deadline(&fixture.open(), &task.id)
        .unwrap()
        .unwrap();
    assert!(
        VibexUseBudgetRepository::check_task_deadline(
            &fixture.open(),
            &task.id,
            original_deadline - 1
        )
        .is_ok()
    );
    assert_eq!(
        VibexUseBudgetRepository::check_task_deadline(&fixture.open(), &task.id, original_deadline)
            .unwrap_err()
            .code,
        "vibex_use_task_deadline_exceeded"
    );
    let request = fixture.request(&root, &worker, Some(&task), "before-expiration");
    let submission = MessageSubmissionRepository::enqueue(
        &mut fixture.open(),
        MessageSubmissionId::new(),
        &request,
    )
    .unwrap();
    let conn = fixture.open();
    MessageSubmissionRepository::advance_status(
        &conn,
        &submission.submission_id,
        MessageSubmissionStatus::AwaitingRuntime,
        MessageSubmissionStatus::ReadyToDispatch,
    )
    .unwrap();

    // Move only the fixture's durable deadline; no wall-clock sleep is needed.
    conn.execute(
        "UPDATE vibex_use_task_limits SET deadline_at_ms = ?2 WHERE task_id = ?1",
        params![task.id.as_str(), unix_timestamp_ms() - 1],
    )
    .unwrap();
    let next = fixture.request(&root, &worker, Some(&task), "after-expiration");
    let error = MessageSubmissionRepository::enqueue(
        &mut fixture.open(),
        MessageSubmissionId::new(),
        &next,
    )
    .unwrap_err();
    assert_eq!(error.code, "vibex_use_task_deadline_exceeded");
    assert_eq!(
        MessageSubmissionRepository::mark_about_to_prompt(&conn, &submission.submission_id)
            .unwrap_err()
            .code,
        "vibex_use_task_deadline_exceeded"
    );
    assert_eq!(
        MessageSubmissionRepository::get(&conn, &submission.submission_id)
            .unwrap()
            .unwrap()
            .status,
        MessageSubmissionStatus::ReadyToDispatch
    );
    let execution =
        VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
            .unwrap()
            .unwrap();
    assert_eq!(execution.outcome, ExecutionOutcome::Queued);
    assert!(execution.start_sequence.is_none());
    let descendant = fixture.session("Nested worker", "codex");
    assert_eq!(
        fixture
            .reserve(&fixture.task(&worker, &descendant, "nested-task"))
            .unwrap_err()
            .code,
        "vibex_use_task_deadline_exceeded"
    );
    assert_eq!(
        VibexUseBudgetRepository::expired_tasks(&conn, unix_timestamp_ms())
            .unwrap()
            .iter()
            .map(|task| &task.id)
            .collect::<Vec<_>>(),
        vec![&task.id]
    );
}

#[test]
fn expanded_policy_controls_depth_and_capacity_without_cancelling_existing_work() {
    let fixture = Fixture::new();
    let conservative = VibexUseBudgetPolicy::for_preset(VibexUseBudgetPreset::Conservative);
    VibexUseBudgetRepository::set_policy(&fixture.open(), &conservative).unwrap();
    let sessions: Vec<_> = (0..5)
        .map(|index| fixture.session(&format!("Level {index}"), "codex"))
        .collect();
    for edge in sessions.windows(2) {
        fixture.own(&edge[0], &edge[1]);
    }
    let nested = fixture.task(&sessions[2], &sessions[3], "nested");
    assert_eq!(
        fixture.reserve(&nested).unwrap_err().code,
        "delegation_depth_exceeded"
    );
    let expanded = VibexUseBudgetSettings {
        preset: VibexUseBudgetPreset::Expanded,
        per_agent_execution_limit: Some(16),
        ..VibexUseBudgetSettings::default()
    }
    .resolve()
    .unwrap();
    VibexUseBudgetRepository::set_policy(&fixture.open(), &expanded).unwrap();
    assert_eq!(
        VibexUseBudgetRepository::policy(&fixture.open()).unwrap(),
        expanded
    );
    fixture.reserve(&nested).unwrap();
    let overflow = fixture.session("Too deep", "codex");
    assert_eq!(
        fixture
            .reserve(&fixture.task(&sessions[4], &overflow, "too-deep"))
            .unwrap_err()
            .code,
        "delegation_depth_exceeded"
    );
    for index in 0..9 {
        let child = fixture.session(&format!("Parallel {index}"), "codex");
        fixture
            .reserve(&fixture.task(&sessions[0], &child, &format!("parallel-{index}")))
            .unwrap();
    }
    let conn = fixture.open();
    assert_eq!(
        count_active_executions_for_root(&conn, &sessions[0].id).unwrap(),
        10
    );
    let deadline = VibexUseBudgetRepository::task_deadline(&conn, &nested.id).unwrap();
    fixture.policy(1, 1);
    assert_eq!(
        count_active_executions_for_root(&conn, &sessions[0].id).unwrap(),
        10
    );
    assert_eq!(
        VibexUseBudgetRepository::task_deadline(&conn, &nested.id).unwrap(),
        deadline
    );
    let extra = fixture.session("Over capacity", "codex");
    assert_eq!(
        fixture
            .reserve(&fixture.task(&sessions[0], &extra, "over-capacity"))
            .unwrap_err()
            .code,
        "vibex_use_root_budget_exceeded"
    );
    let current = VibexUseBudgetRepository::policy(&conn).unwrap();
    let invalid = VibexUseBudgetPolicy {
        max_depth: 0,
        ..current.clone()
    };
    assert!(VibexUseBudgetRepository::set_policy(&conn, &invalid).is_err());
    assert_eq!(VibexUseBudgetRepository::policy(&conn).unwrap(), current);
}

#[test]
fn reported_token_budget_keeps_unknown_usage_and_counts_only_known_totals() {
    let fixture = Fixture::new();
    let mut policy = fixture.policy(64, 64);
    let root = fixture.session("Root", "codex");
    let conn = fixture.open();
    assert_eq!(
        VibexUseBudgetRepository::reported_tokens(&conn, &root.id).unwrap(),
        None
    );
    let unknown = fixture.session("Unknown usage", "codex");
    let execution = fixture.execution(&root, &unknown, "unknown");
    fixture.usage(&unknown, &execution, AgentUsageTokenValues::default());
    let partial = fixture.session("Input only", "codex");
    let execution = fixture.execution(&root, &partial, "partial");
    fixture.usage(
        &partial,
        &execution,
        AgentUsageTokenValues {
            input_tokens: Some(500),
            ..AgentUsageTokenValues::default()
        },
    );
    assert_eq!(
        VibexUseBudgetRepository::reported_tokens(&conn, &root.id).unwrap(),
        None
    );
    let total = fixture.session("Reported total", "codex");
    let execution = fixture.execution(&root, &total, "total");
    fixture.usage(
        &total,
        &execution,
        AgentUsageTokenValues {
            total_tokens: Some(40),
            input_tokens: Some(100),
            output_tokens: Some(200),
            thought_tokens: Some(300),
            cached_read_tokens: Some(400),
            cached_write_tokens: Some(500),
        },
    );
    assert_eq!(
        VibexUseBudgetRepository::reported_tokens(&conn, &root.id).unwrap(),
        Some(40)
    );
    let derived = fixture.session("Derived total", "codex");
    let execution = fixture.execution(&root, &derived, "derived");
    fixture.usage(
        &derived,
        &execution,
        AgentUsageTokenValues {
            input_tokens: Some(5),
            output_tokens: Some(7),
            cached_read_tokens: Some(100),
            ..AgentUsageTokenValues::default()
        },
    );
    let foreign_root = fixture.session("Another root", "codex");
    let foreign_worker = fixture.session("Another team's usage", "codex");
    let execution = fixture.execution(&foreign_root, &foreign_worker, "foreign-usage");
    fixture.usage(
        &foreign_worker,
        &execution,
        AgentUsageTokenValues {
            total_tokens: Some(1_000),
            ..AgentUsageTokenValues::default()
        },
    );
    assert_eq!(
        VibexUseBudgetRepository::reported_tokens(&conn, &root.id).unwrap(),
        Some(52)
    );
    policy.max_reported_tokens = Some(52);
    VibexUseBudgetRepository::set_policy(&conn, &policy).unwrap();
    assert_eq!(
        VibexUseBudgetRepository::check_admission(&conn, &root.id, Some(&root.agent_id), 0)
            .unwrap_err()
            .code,
        "vibex_use_token_budget_exceeded"
    );
    policy.max_reported_tokens = Some(53);
    VibexUseBudgetRepository::set_policy(&conn, &policy).unwrap();
    assert!(
        VibexUseBudgetRepository::check_admission(&conn, &root.id, Some(&root.agent_id), 0).is_ok()
    );
}

#[test]
fn token_sum_overflow_cannot_reopen_an_exhausted_budget() {
    let fixture = Fixture::new();
    let mut policy = fixture.policy(64, 64);
    let root = fixture.session("Root", "codex");
    for index in 0..3 {
        let worker = fixture.session(&format!("Worker {index}"), "codex");
        let execution = fixture.execution(&root, &worker, &format!("large-usage-{index}"));
        fixture.usage(
            &worker,
            &execution,
            AgentUsageTokenValues {
                total_tokens: Some(i64::MAX as u64),
                ..AgentUsageTokenValues::default()
            },
        );
    }
    let conn = fixture.open();
    assert_eq!(
        VibexUseBudgetRepository::reported_tokens(&conn, &root.id).unwrap(),
        Some(u64::MAX)
    );
    policy.max_reported_tokens = Some(i64::MAX as u64);
    VibexUseBudgetRepository::set_policy(&conn, &policy).unwrap();
    let extra = fixture.session("Extra worker", "codex");
    fixture.own(&root, &extra);
    let request = fixture.request(&root, &extra, None, "after-overflow");
    let error = MessageSubmissionRepository::enqueue(
        &mut fixture.open(),
        MessageSubmissionId::new(),
        &request,
    )
    .unwrap_err();
    assert_eq!(error.code, "vibex_use_token_budget_exceeded");
    assert_eq!(
        count_active_executions_for_root(&conn, &root.id).unwrap(),
        3
    );
}

fn running_cancellation_round(
    fixture: &Fixture,
    actor: &AgentSession,
    target: &AgentSession,
    task: Option<&AgentDelegation>,
    key: &str,
) -> DelegationExecution {
    let request = fixture.request(actor, target, task, key);
    let mut conn = fixture.open();
    let submission =
        MessageSubmissionRepository::enqueue(&mut conn, MessageSubmissionId::new(), &request)
            .unwrap();
    MessageSubmissionRepository::advance_status(
        &conn,
        &submission.submission_id,
        MessageSubmissionStatus::AwaitingRuntime,
        MessageSubmissionStatus::ReadyToDispatch,
    )
    .unwrap();
    MessageSubmissionRepository::mark_about_to_prompt(&conn, &submission.submission_id).unwrap();
    VibexUseExecutionRepository::get_by_submission(&conn, &submission.submission_id)
        .unwrap()
        .unwrap()
}

fn settle_cancellation_round(
    conn: &mut Connection,
    execution: &DelegationExecution,
    outcome: ExecutionOutcome,
) -> DelegationExecution {
    if outcome == ExecutionOutcome::Ambiguous {
        MessageSubmissionRepository::mark_ambiguous(conn, &execution.submission_id, None).unwrap();
    } else {
        for (from, to) in [
            (
                MessageSubmissionStatus::AboutToPrompt,
                MessageSubmissionStatus::Dispatched,
            ),
            (
                MessageSubmissionStatus::Dispatched,
                MessageSubmissionStatus::Completed,
            ),
        ] {
            MessageSubmissionRepository::advance_status(conn, &execution.submission_id, from, to)
                .unwrap();
        }
    }
    settle_vibex_use_execution(
        conn,
        &DelegationExecution {
            outcome,
            finished_at_ms: Some(unix_timestamp_ms()),
            ..execution.clone()
        },
    )
    .unwrap()
    .unwrap()
}

#[test]
fn cancellation_captures_causal_work_without_following_foreign_navigation_children() {
    for cascade in [false, true] {
        let fixture = Fixture::new();
        let mut policy = fixture.policy(64, 64);
        policy.max_depth = 4;
        VibexUseBudgetRepository::set_policy(&fixture.open(), &policy).unwrap();
        let root = fixture.session("Origin root", "codex");
        let worker = fixture.session("Origin worker", "codex");
        let foreign_root = fixture.session("Navigation root", "codex");
        let external = fixture.session("Controlled external worker", "codex");
        let prior_child = fixture.session("Preexisting task worker", "codex");
        let prior_owned = fixture.session("Preexisting taskless worker", "codex");
        fixture.own(&root, &worker);
        fixture.own(&foreign_root, &external);
        fixture.own(&external, &prior_child);
        fixture.own(&external, &prior_owned);

        // These inputs predate external control and must stay outside its stop.
        let prior_task = fixture.task(&external, &prior_child, "prior-task");
        fixture.reserve(&prior_task).unwrap();
        let prior_task_round = running_cancellation_round(
            &fixture,
            &external,
            &prior_child,
            Some(&prior_task),
            "prior-task-round",
        );
        let prior_owned_round = running_cancellation_round(
            &fixture,
            &external,
            &prior_owned,
            None,
            "prior-taskless-round",
        );
        let task = fixture.task(&root, &worker, "origin-task");
        fixture.reserve(&task).unwrap();
        let own_round =
            running_cancellation_round(&fixture, &root, &worker, Some(&task), "own-round");
        let mut conn = fixture.open();
        SessionGrantRepository::grant(&conn, &worker.id, &external.id, "controlled", "human:local")
            .unwrap();
        let external_round =
            running_cancellation_round(&fixture, &worker, &external, None, "external-round");
        assert!(external_round.task_ref.is_none());
        assert_eq!(
            vibex_use_origin_task(&conn, &external.id).unwrap(),
            Some(task.id.clone())
        );
        assert_eq!(
            external_round.root_session_ref,
            Some(VibexUseRef::session(&root.id))
        );

        let child = fixture.session("Causal child worker", "codex");
        fixture.own(&external, &child);
        let child_task = fixture.task(&external, &child, "causal-child-task");
        fixture.reserve(&child_task).unwrap();
        let child_round = running_cancellation_round(
            &fixture,
            &external,
            &child,
            Some(&child_task),
            "causal-child-round",
        );
        let queued_request = fixture.request(&worker, &external, None, "queued-external-round");
        let queued_submission = MessageSubmissionRepository::enqueue(
            &mut conn,
            MessageSubmissionId::new(),
            &queued_request,
        )
        .unwrap();
        let queued_round =
            VibexUseExecutionRepository::get_by_submission(&conn, &queued_submission.submission_id)
                .unwrap()
                .unwrap();
        let navigation = [
            &root,
            &worker,
            &foreign_root,
            &external,
            &prior_child,
            &prior_owned,
            &child,
        ];
        let ancestry = || {
            let conn = fixture.open();
            navigation
                .iter()
                .map(|session| {
                    (
                        SessionOwnershipRepository::parent_of(&conn, &session.id).unwrap(),
                        SessionOwnershipRepository::root_of(&conn, &session.id).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let original_ancestry = ancestry();
        assert_eq!(
            SessionOwnershipRepository::root_of(&conn, &external.id).unwrap(),
            foreign_root.id
        );
        assert_eq!(
            SessionOwnershipRepository::root_of(&conn, &child.id).unwrap(),
            foreign_root.id
        );
        assert_eq!(vibex_use_root_session(&conn, &child.id).unwrap(), root.id);

        let cancelled = request_delegation_tree_cancellation(&mut conn, &task.id, cascade).unwrap();
        let expected_tasks: std::collections::BTreeSet<_> = if cascade {
            [task.id.clone(), child_task.id.clone()]
                .into_iter()
                .collect()
        } else {
            [task.id.clone()].into_iter().collect()
        };
        assert_eq!(
            cancelled
                .iter()
                .map(|task| task.id.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            expected_tasks,
            "cascade={cascade}"
        );
        let expected_executions: std::collections::BTreeSet<_> = if cascade {
            [
                own_round.id.clone(),
                external_round.id.clone(),
                child_round.id.clone(),
                queued_round.id.clone(),
            ]
            .into_iter()
            .collect()
        } else {
            [own_round.id.clone()].into_iter().collect()
        };
        assert_eq!(
            VibexUseCancellationRepository::executions_for_task(&conn, &task.id)
                .unwrap()
                .into_iter()
                .map(|execution| execution.id)
                .collect::<std::collections::BTreeSet<_>>(),
            expected_executions,
            "cascade={cascade}"
        );
        assert!(VibexUseCancellationRepository::is_requested(&conn, &own_round.id).unwrap());
        for round in [&external_round, &child_round, &queued_round] {
            assert_eq!(
                VibexUseCancellationRepository::is_requested(&conn, &round.id).unwrap(),
                cascade
            );
        }
        for round in [&prior_task_round, &prior_owned_round] {
            assert!(!VibexUseCancellationRepository::is_requested(&conn, &round.id).unwrap());
            assert_eq!(
                VibexUseExecutionRepository::get(&conn, &round.id)
                    .unwrap()
                    .unwrap()
                    .outcome,
                ExecutionOutcome::Running
            );
        }
        assert_eq!(
            AgentDelegationRepository::get(&conn, &prior_task.id)
                .unwrap()
                .unwrap()
                .phase(),
            DelegationTaskPhase::Active
        );
        assert_eq!(
            AgentDelegationRepository::get(&conn, &child_task.id)
                .unwrap()
                .unwrap()
                .phase(),
            if cascade {
                DelegationTaskPhase::Cancelling
            } else {
                DelegationTaskPhase::Active
            }
        );
        assert_eq!(
            MessageSubmissionRepository::get(&conn, &queued_submission.submission_id)
                .unwrap()
                .unwrap()
                .status,
            if cascade {
                MessageSubmissionStatus::Cancelled
            } else {
                MessageSubmissionStatus::AwaitingRuntime
            }
        );
        if !cascade {
            MessageSubmissionRepository::advance_status(
                &conn,
                &queued_submission.submission_id,
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            )
            .unwrap();
            MessageSubmissionRepository::mark_about_to_prompt(
                &conn,
                &queued_submission.submission_id,
            )
            .unwrap();
            assert_eq!(
                VibexUseExecutionRepository::get(&conn, &queued_round.id)
                    .unwrap()
                    .unwrap()
                    .outcome,
                ExecutionOutcome::Running
            );
        }
        let late_child = fixture.session("Late causal child", "codex");
        assert_eq!(
            fixture
                .reserve(&fixture.task(&external, &late_child, "late-child-task"))
                .unwrap_err()
                .code,
            "vibex_use_task_cancelling"
        );
        assert_eq!(ancestry(), original_ancestry);
    }
}

#[test]
fn task_cancellation_waits_for_running_and_ambiguous_captured_taskless_rounds() {
    for final_outcome in [ExecutionOutcome::Completed, ExecutionOutcome::Ambiguous] {
        let fixture = Fixture::new();
        fixture.policy(64, 64);
        let root = fixture.session("Origin root", "codex");
        let worker = fixture.session("Origin worker", "codex");
        let foreign_root = fixture.session("Navigation root", "codex");
        let external = fixture.session("Controlled external worker", "codex");
        fixture.own(&root, &worker);
        fixture.own(&foreign_root, &external);
        let task = fixture.task(&root, &worker, "origin-task");
        fixture.reserve(&task).unwrap();
        let own_round =
            running_cancellation_round(&fixture, &root, &worker, Some(&task), "own-round");
        let mut conn = fixture.open();
        SessionGrantRepository::grant(&conn, &worker.id, &external.id, "controlled", "human:local")
            .unwrap();
        let external_round =
            running_cancellation_round(&fixture, &worker, &external, None, "external-round");
        request_delegation_tree_cancellation(&mut conn, &task.id, true).unwrap();
        assert_eq!(
            settle_cancellation_round(&mut conn, &own_round, ExecutionOutcome::Completed).outcome,
            ExecutionOutcome::Cancelled
        );
        VibexUseCancellationRepository::settle_task(&conn, &task.id).unwrap();
        assert!(!VibexUseCancellationRepository::is_confirmed(&conn, &task.id).unwrap());
        assert_eq!(
            AgentDelegationRepository::get(&conn, &task.id)
                .unwrap()
                .unwrap()
                .phase(),
            DelegationTaskPhase::Cancelling
        );
        assert_eq!(
            SessionControllerRepository::get(&conn, &worker.id)
                .unwrap()
                .unwrap()
                .owner_task_id,
            Some(task.id.clone())
        );
        let cancellation_event = DelegationTaskEventKind::TaskCancelled.stable_id(&task.id, None);
        assert!(
            VibexUseEventRepository::get(&conn, &cancellation_event)
                .unwrap()
                .is_none()
        );

        let settled = settle_cancellation_round(&mut conn, &external_round, final_outcome);
        let ambiguous = final_outcome == ExecutionOutcome::Ambiguous;
        assert_eq!(
            settled.outcome,
            if ambiguous {
                ExecutionOutcome::Ambiguous
            } else {
                ExecutionOutcome::Cancelled
            }
        );
        VibexUseCancellationRepository::settle_task(&conn, &task.id).unwrap();
        assert_eq!(
            VibexUseCancellationRepository::is_confirmed(&conn, &task.id).unwrap(),
            !ambiguous
        );
        assert_eq!(
            AgentDelegationRepository::get(&conn, &task.id)
                .unwrap()
                .unwrap()
                .phase(),
            if ambiguous {
                DelegationTaskPhase::Cancelling
            } else {
                DelegationTaskPhase::Cancelled
            }
        );
        assert_eq!(
            SessionControllerRepository::get(&conn, &worker.id)
                .unwrap()
                .and_then(|controller| controller.owner_task_id),
            ambiguous.then(|| task.id.clone())
        );
        assert_eq!(
            VibexUseEventRepository::get(&conn, &cancellation_event)
                .unwrap()
                .is_some(),
            !ambiguous
        );
        assert_eq!(
            VibexUseCancellationRepository::pending_tasks(&conn)
                .unwrap()
                .iter()
                .any(|pending| pending.id == task.id),
            ambiguous
        );
        assert_eq!(
            SessionOwnershipRepository::parent_of(&conn, &external.id).unwrap(),
            Some(foreign_root.id.clone())
        );
        assert_eq!(
            SessionOwnershipRepository::root_of(&conn, &external.id).unwrap(),
            foreign_root.id
        );
        assert_eq!(
            settled.root_session_ref,
            Some(VibexUseRef::session(&root.id))
        );
    }
}

#[test]
fn cancellation_maintenance_rotates_past_ambiguous_tasks_and_fences_new_deadlines() {
    let fixture = Fixture::new();
    fixture.policy(64, 64);
    let root = fixture.session("Maintenance root", "codex");
    let mut conn = fixture.open();
    let mut pending_ids = std::collections::BTreeSet::new();
    let mut ambiguous_rounds = Vec::new();
    for index in 0..65 {
        let worker = fixture.session(&format!("Ambiguous worker {index}"), "codex");
        fixture.own(&root, &worker);
        let task = fixture.task(&root, &worker, &format!("ambiguous-task-{index}"));
        fixture.reserve(&task).unwrap();
        let execution = running_cancellation_round(
            &fixture,
            &root,
            &worker,
            Some(&task),
            &format!("ambiguous-round-{index}"),
        );
        request_delegation_tree_cancellation(&mut conn, &task.id, true).unwrap();
        let settled = settle_cancellation_round(&mut conn, &execution, ExecutionOutcome::Ambiguous);
        assert_eq!(settled.outcome, ExecutionOutcome::Ambiguous);
        conn.execute(
            "UPDATE vibex_use_task_limits SET deadline_at_ms = ?2 WHERE task_id = ?1",
            params![task.id.as_str(), index + 1],
        )
        .unwrap();
        pending_ids.insert(task.id);
        ambiguous_rounds.push(execution);
    }

    // Older unresolved stops must not occupy the first deadline page forever.
    let mut fresh_ids = std::collections::BTreeSet::new();
    for index in 0..2 {
        let worker = fixture.session(&format!("Newly expired worker {index}"), "codex");
        fixture.own(&root, &worker);
        let task = fixture.task(&root, &worker, &format!("newly-expired-{index}"));
        fixture.reserve(&task).unwrap();
        conn.execute(
            "UPDATE vibex_use_task_limits SET deadline_at_ms = ?2 WHERE task_id = ?1",
            params![task.id.as_str(), index + 1_000],
        )
        .unwrap();
        fresh_ids.insert(task.id.clone());
        pending_ids.insert(task.id);
    }
    let expired = VibexUseBudgetRepository::expired_tasks(&conn, unix_timestamp_ms()).unwrap();
    assert_eq!(
        expired
            .iter()
            .map(|task| task.id.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        fresh_ids
    );
    for task in expired {
        request_delegation_tree_cancellation(&mut conn, &task.id, true).unwrap();
    }
    assert!(
        VibexUseBudgetRepository::expired_tasks(&conn, unix_timestamp_ms())
            .unwrap()
            .is_empty()
    );
    let before = pending_ids
        .iter()
        .map(|id| AgentDelegationRepository::get(&conn, id).unwrap().unwrap())
        .collect::<Vec<_>>();

    let first = VibexUseCancellationRepository::pending_tasks(&conn).unwrap();
    assert_eq!(first.len(), 64);
    let mut observed = first
        .into_iter()
        .map(|task| task.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(observed.is_disjoint(&fresh_ids));
    drop(conn);

    // Selection history is durable: a new connection must reach later tasks
    // even though none of the ambiguous executions has become confirmable.
    let conn = fixture.open();
    let second = VibexUseCancellationRepository::pending_tasks(&conn).unwrap();
    assert_eq!(second.len(), 64);
    observed.extend(second.into_iter().map(|task| task.id));
    assert_eq!(observed, pending_ids);
    for saved in before {
        let current = AgentDelegationRepository::get(&conn, &saved.id)
            .unwrap()
            .unwrap();
        assert_eq!(current.phase(), DelegationTaskPhase::Cancelling);
        assert_eq!(current.revision, saved.revision);
        assert_eq!(current.updated_at_ms, saved.updated_at_ms);
    }
    for execution in ambiguous_rounds {
        assert_eq!(
            VibexUseExecutionRepository::get(&conn, &execution.id)
                .unwrap()
                .unwrap()
                .outcome,
            ExecutionOutcome::Ambiguous
        );
        let task_id = execution.task_ref.as_ref().unwrap().task_id().unwrap();
        assert!(!VibexUseCancellationRepository::is_confirmed(&conn, &task_id).unwrap());
        assert_eq!(
            SessionControllerRepository::get(&conn, &execution.session_ref.session_id().unwrap())
                .unwrap()
                .unwrap()
                .owner_task_id,
            Some(task_id)
        );
    }
}

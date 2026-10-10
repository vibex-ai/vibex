use super::*;
use crate::test_support::TestRuntimeHarness;
use vibex_core::{
    AgentSessionRuntimeSelectionState, DelegationCompletionPolicy, DelegationOwnershipKind,
    RuntimeSelectionInteraction, SetDesiredAgentSessionRuntimeRequest, VibexUseOperation,
    VibexUseOperationResource, VibexUseOperationState,
};
use vibex_db::{
    SessionControllerRepository, SessionGrantRepository, VibexUseCancellationRepository,
    VibexUseInterruptRepository, VibexUseOperationRepository,
};

struct DelegationFixture {
    harness: TestRuntimeHarness,
    parent: AgentSession,
    selection: SessionRuntimeSelection,
    db_path: PathBuf,
    _directory: tempfile::TempDir,
}

impl DelegationFixture {
    async fn new() -> Self {
        Self::with_provider(Arc::new(CompletingTurnProvider)).await
    }

    async fn with_provider(provider: Arc<dyn AgentProvider>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("delegation.db");
        let harness = TestRuntimeHarness::new(&db_path, AgentId::parse("codex").unwrap(), provider);
        let selection = harness
            .resolve_initial_runtime_selection(None, ProviderKind::Codex, None, None)
            .unwrap();
        let parent = harness
            .create_session(CreateAgentSessionRequest {
                session_id: None,
                defer_runtime_materialization: false,
                runtime: selection.clone(),
                workspace_root: directory.path().to_string_lossy().into_owned(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                title: Some("Delegation parent".into()),
                safety: Some(AgentSessionSafety::workspace_write_ask_on_risk()),
            })
            .await
            .unwrap();
        Self {
            harness,
            parent,
            selection,
            db_path,
            _directory: directory,
        }
    }

    fn request(&self, key: &str) -> CreateAgentDelegationRequest {
        CreateAgentDelegationRequest {
            parent_session_id: self.parent.id.clone(),
            idempotency_key: key.into(),
            task: "Review the change and report its result".into(),
            attachments: Vec::new(),
            title: Some("Review".into()),
            agent_id: None,
            provider_profile_id: None,
            model: None,
            reasoning_effort: None,
            mode_id: None,
            completion_policy: DelegationCompletionPolicy::OwnerReview,
            ownership_kind: DelegationOwnershipKind::OwnedChild,
            context_refs: Vec::new(),
            acceptance_criteria: vec!["Explain the observed behavior".into()],
            follows_task_id: None,
            existing_session_id: None,
            runtime_selection: None,
            prompt_context: None,
            operation_id: None,
        }
    }

    fn reserve_task(&self, request: &CreateAgentDelegationRequest) -> AgentDelegation {
        let mut conn = self.harness.open_migrated().unwrap();
        let mut task = AgentDelegation::single_turn_legacy(
            self.parent.id.clone(),
            request.idempotency_key.clone(),
            "Review",
            request.task.clone(),
            Some(self.parent.agent_id.clone()),
            AgentDelegationStatus::Starting,
            unix_timestamp_ms(),
        );
        task.root_session_id = Some(self.parent.id.clone());
        task.completion_policy = request.completion_policy;
        task.requested_runtime = Some(delegation_runtime_summary(&self.selection));
        task.payload_fingerprint = Some(request.payload_fingerprint());
        match AgentDelegationRepository::reserve_or_get(&mut conn, &task, 8).unwrap() {
            AgentDelegationReservation::Claimed(task) => task,
            other => panic!("expected a new task, got {other:?}"),
        }
    }

    fn operation(&self, key: &str) -> Option<VibexUseOperation> {
        let manager = self.harness.manager();
        VibexUseOperationRepository::get_by_key(
            &manager.open_migrated().unwrap(),
            &format!("{}\u{1f}{}", manager.vibex_use_authority(), self.parent.id),
            "vibex_delegate_legacy",
            key,
        )
        .unwrap()
    }

    async fn wait_for_task(&self, id: &AgentDelegationId) -> AgentDelegation {
        wait_for(|| {
            let task = self
                .harness
                .get_agent_delegation(&self.parent.id, id)
                .unwrap();
            (task.phase() == DelegationTaskPhase::AwaitingReview || task.phase().is_terminal())
                .then_some(task)
        })
        .await
    }

    async fn switch_mode(
        &self,
        session_id: &VibexSessionId,
        mode: &str,
    ) -> AgentSessionRuntimeSelectionState {
        let manager = self.harness.manager();
        let runtime = manager.runtime_selection.get().unwrap().upgrade().unwrap();
        let state = runtime.get_selection_state(session_id).unwrap();
        let mut desired = state.desired;
        desired.mode_id = Some(mode.into());
        runtime
            .set_desired_runtime(SetDesiredAgentSessionRuntimeRequest {
                session_id: session_id.clone(),
                idempotency_key: format!("switch-{mode}"),
                expected_revision: state.session_revision,
                expected_selection_revision: state.selection_revision,
                desired: desired.clone(),
                interaction: RuntimeSelectionInteraction::Seamless,
            })
            .await
            .unwrap();
        wait_for(|| {
            let state = runtime.get_selection_state(session_id).unwrap();
            (state.status == SessionRuntimeSelectionStatus::Ready && state.effective == desired)
                .then_some(state)
        })
        .await
    }

    fn enqueue_round(&self, task: &AgentDelegation, key: &str) -> DelegationExecution {
        let manager = self.harness.manager();
        let child = task.child_session_id.as_ref().unwrap();
        let conn = manager.open_migrated().unwrap();
        let selection = AgentSessionRuntimeRepository::get_runtime_state(&conn, child)
            .unwrap()
            .unwrap()
            .effective_runtime_selection
            .unwrap();
        let operation = self.operation(&task.idempotency_key).unwrap();
        let submission = manager
            .message_submission_coordinator()
            .unwrap()
            .prepare_submission(SendAgentMessageRequest {
                session_id: child.clone(),
                message_idempotency_key: key.into(),
                desired_runtime: selection,
                text: format!("Follow-up {key}"),
                attachments: Vec::new(),
                mentions: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
                delivery: UserMessageDelivery::Prompt,
                prompt_context: None,
                provenance: MessageProvenance::DelegatedInput {
                    actor_session_ref: VibexUseRef::session(&self.parent.id),
                    task_ref: Some(VibexUseRef::task(&task.id)),
                    operation_ref: operation.operation_ref,
                },
            })
            .unwrap();
        let execution = VibexUseExecutionRepository::get_by_submission(&conn, &submission)
            .unwrap()
            .unwrap();
        manager.start_execution_observer(&execution.id).unwrap();
        execution
    }

    fn enqueue_external_round(
        &self,
        actor: &VibexSessionId,
        target: &AgentSession,
        key: &str,
    ) -> DelegationExecution {
        let manager = self.harness.manager();
        let conn = manager.open_migrated().unwrap();
        let id = VibexOperationId::new();
        let now = unix_timestamp_ms();
        let operation = VibexUseOperation {
            operation_ref: VibexUseRef::operation(&id),
            id,
            authority: manager.vibex_use_authority(),
            actor_key: actor.to_string(),
            tool: "vibex_send_message".into(),
            caller_key: key.into(),
            payload_fingerprint: key.into(),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: Default::default(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        VibexUseOperationRepository::reserve(&conn, &operation, key).unwrap();
        let submission = manager
            .message_submission_coordinator()
            .unwrap()
            .prepare_submission(SendAgentMessageRequest {
                session_id: target.id.clone(),
                message_idempotency_key: key.into(),
                desired_runtime: self.selection.clone(),
                text: "Review the external context".into(),
                attachments: Vec::new(),
                mentions: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
                delivery: UserMessageDelivery::Prompt,
                prompt_context: None,
                provenance: MessageProvenance::DelegatedInput {
                    actor_session_ref: VibexUseRef::session(actor),
                    task_ref: None,
                    operation_ref: operation.operation_ref,
                },
            })
            .unwrap();
        let execution = VibexUseExecutionRepository::get_by_submission(&conn, &submission)
            .unwrap()
            .unwrap();
        manager.start_execution_observer(&execution.id).unwrap();
        execution
    }

    async fn wait_for_execution(&self, id: &VibexExecutionId) -> DelegationExecution {
        wait_for(|| {
            let execution =
                VibexUseExecutionRepository::get(&self.harness.open_migrated().unwrap(), id)
                    .unwrap()
                    .unwrap();
            execution.is_settled().then_some(execution)
        })
        .await
    }
}

#[derive(Default)]
struct PreparationBlockedProvider {
    block_next: std::sync::atomic::AtomicBool,
    prepared: tokio::sync::Notify,
    release: tokio::sync::Notify,
    prompts: Mutex<Vec<String>>,
    interrupts: AtomicUsize,
}

#[async_trait]
impl AgentProvider for PreparationBlockedProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Acp
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = CompletingTurnProvider.capabilities();
        capabilities.interrupt = true;
        capabilities
    }

    async fn create_session(
        &self,
        _request: ProviderCreateRequest,
    ) -> VibexResult<ProviderSessionHandle> {
        unreachable!("the test harness owns runtime materialization")
    }

    async fn resume_session(&self, binding: ProviderBinding) -> VibexResult<ProviderSessionHandle> {
        Ok(ProviderSessionHandle {
            binding,
            capabilities: self.capabilities(),
        })
    }

    async fn prepare_turn_execution(
        &self,
        _handle: &ProviderSessionHandle,
        request: &ProviderTurnRequest,
    ) -> VibexResult<Option<ProviderTurnExecutionIdentity>> {
        if self.block_next.swap(false, Ordering::SeqCst) {
            self.prepared.notify_one();
            self.release.notified().await;
        }
        Ok(request.execution_identity.clone())
    }

    async fn send_turn(
        &self,
        handle: ProviderSessionHandle,
        request: ProviderTurnRequest,
    ) -> VibexResult<ProviderTurnResult> {
        self.prompts.lock().unwrap().push(request.text.clone());
        CompletingTurnProvider.send_turn(handle, request).await
    }

    async fn interrupt(&self, _handle: ProviderSessionHandle) -> VibexResult<()> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn wait_for<T>(mut read: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(value) = read() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("durable delegation state did not reach the expected checkpoint")
}

#[tokio::test]
async fn restart_fails_an_unlinked_task_but_preserves_a_checkpointed_creation() {
    let fixture = DelegationFixture::new().await;
    let orphan_request = fixture.request("orphan");
    let orphan = fixture.reserve_task(&orphan_request);
    let request = fixture.request("recoverable");
    let recoverable = fixture.reserve_task(&request);
    let manager = fixture.harness.manager();
    let id = VibexOperationId::new();
    let now = unix_timestamp_ms();
    let operation = VibexUseOperation {
        operation_ref: VibexUseRef::operation(&id),
        id,
        authority: manager.vibex_use_authority(),
        actor_key: fixture.parent.id.to_string(),
        tool: "vibex_delegate".into(),
        caller_key: request.idempotency_key.clone(),
        payload_fingerprint: request.payload_fingerprint(),
        state: VibexUseOperationState::Accepted,
        error_code: None,
        error_message: None,
        retryable: false,
        resources: vec![VibexUseOperationResource {
            kind: "task".into(),
            reference: VibexUseRef::task(&recoverable.id),
        }],
        checkpoint: std::collections::BTreeMap::from([(
            "delegation_request".into(),
            serde_json::to_string(&request).unwrap(),
        )]),
        created_at_ms: now,
        updated_at_ms: now,
    };
    VibexUseOperationRepository::reserve(
        &manager.open_migrated().unwrap(),
        &operation,
        &operation.caller_key,
    )
    .unwrap();

    let reopened = Arc::new(AgentManager::new(&fixture.db_path).unwrap());
    assert_eq!(reopened.reconcile_agent_delegations().unwrap(), 0);
    let orphan = reopened
        .get_agent_delegation(&fixture.parent.id, &orphan.id)
        .unwrap();
    assert_eq!(orphan.phase(), DelegationTaskPhase::Failed);
    assert_eq!(
        orphan.error_code.as_deref(),
        Some("agent_delegation_child_session_missing")
    );
    let retained = reopened
        .get_agent_delegation(&fixture.parent.id, &recoverable.id)
        .unwrap();
    assert_eq!(retained.phase(), DelegationTaskPhase::Starting);
    assert!(retained.child_session_id.is_none());
    assert!(retained.error_code.is_none());
}

#[tokio::test]
async fn legacy_manager_acceptance_finishes_its_operation_and_reuses_the_same_round() {
    let fixture = DelegationFixture::new().await;
    let request = fixture.request("accepted");
    let manager = fixture.harness.manager();
    let conn = manager.open_migrated().unwrap();
    let mut root = fixture.parent.clone();
    root.id = VibexSessionId::new();
    root.title = "Team root".into();
    SessionRepository::insert(&conn, &root).unwrap();
    vibex_db::SessionOwnershipRepository::upsert(&conn, &fixture.parent.id, &root.id, None)
        .unwrap();
    let first = manager
        .create_task_delegation(request.clone())
        .await
        .unwrap();
    let operation = fixture.operation(&request.idempotency_key).unwrap();
    assert_eq!(operation.state, VibexUseOperationState::Succeeded);
    assert!(operation.state.is_terminal());
    assert!(
        VibexUseOperationRepository::list_pending(&manager.open_migrated().unwrap())
            .unwrap()
            .iter()
            .all(|pending| pending.id != operation.id)
    );
    let task = fixture.wait_for_task(&first.delegation.id).await;
    assert_eq!(task.phase(), DelegationTaskPhase::AwaitingReview);
    let mut policy = vibex_db::VibexUseBudgetRepository::policy(&conn).unwrap();
    policy.max_depth = 1;
    vibex_db::VibexUseBudgetRepository::set_policy(&conn, &policy).unwrap();
    assert_eq!(
        manager
            .create_task_delegation(fixture.request("depth-limited"))
            .await
            .unwrap_err()
            .code,
        "delegation_depth_exceeded"
    );
    let retry = manager
        .create_task_delegation(request.clone())
        .await
        .unwrap();
    assert_eq!(retry.delegation.id, first.delegation.id);
    assert_eq!(retry.child_session_id, first.child_session_id);
    assert_eq!(retry.submission_id, first.submission_id);
    assert_eq!(retry.execution_id, first.execution_id);
    for state in [AgentSessionState::Closed, AgentSessionState::Archived] {
        if state == AgentSessionState::Archived {
            SessionRepository::archive(&conn, &fixture.parent.id).unwrap();
        } else {
            SessionRepository::update_state(&conn, &fixture.parent.id, state).unwrap();
        }
        let replay = manager
            .create_task_delegation(request.clone())
            .await
            .unwrap();
        assert_eq!(replay.delegation.id, first.delegation.id);
        assert_eq!(replay.child_session_id, first.child_session_id);
        assert_eq!(replay.submission_id, first.submission_id);
        assert_eq!(replay.execution_id, first.execution_id);
    }
    assert_eq!(SessionRepository::list(&conn, true).unwrap().len(), 3);
}

#[tokio::test]
async fn a_creation_retry_keeps_its_saved_selection_after_the_parent_switches_runtime() {
    let fixture = DelegationFixture::new().await;
    let request = fixture.request("selection-retry");
    let task = fixture.reserve_task(&request);
    let manager = fixture.harness.manager();
    let gate = manager.delegation_lifecycle_lock(&task.id).unwrap();
    let guard = gate.lock().await;
    let first_manager = manager.clone();
    let first_request = request.clone();
    let first =
        tokio::spawn(async move { first_manager.create_task_delegation(first_request).await });
    let operation = wait_for(|| {
        fixture
            .operation(&request.idempotency_key)
            .filter(|operation| operation.checkpoint.contains_key("delegation_selection"))
    })
    .await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(operation.state, VibexUseOperationState::Accepted);
    let changed = fixture.switch_mode(&fixture.parent.id, "plan").await;
    assert_ne!(changed.effective, fixture.selection);
    drop(guard);

    let resumed = manager
        .create_task_delegation(request.clone())
        .await
        .unwrap();
    let submission =
        MessageSubmissionRepository::get(&manager.open_migrated().unwrap(), &resumed.submission_id)
            .unwrap()
            .unwrap();
    assert_eq!(submission.desired_runtime_selection, fixture.selection);
    assert_eq!(resumed.delegation.id, task.id);
    assert_eq!(
        fixture.operation(&request.idempotency_key).unwrap().id,
        operation.id
    );
    assert_eq!(
        fixture.wait_for_task(&task.id).await.phase(),
        DelegationTaskPhase::AwaitingReview
    );
    assert_eq!(
        SessionRepository::list(&manager.open_migrated().unwrap(), false)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn cancellation_during_creation_keeps_the_fence_and_creates_no_child_or_round() {
    let fixture = DelegationFixture::new().await;
    let request = fixture.request("cancel-creation");
    let task = fixture.reserve_task(&request);
    let manager = fixture.harness.manager();
    let gate = manager.delegation_lifecycle_lock(&task.id).unwrap();
    let guard = gate.lock().await;
    let creation_manager = manager.clone();
    let creation_request = request.clone();
    let creation = tokio::spawn(async move {
        creation_manager
            .create_task_delegation(creation_request)
            .await
    });
    wait_for(|| {
        fixture
            .operation(&request.idempotency_key)
            .filter(|operation| operation.checkpoint.contains_key("delegation_selection"))
    })
    .await;
    let cancel_manager = manager.clone();
    let cancel_request = CancelAgentDelegationRequest {
        parent_session_id: fixture.parent.id.clone(),
        delegation_id: task.id.clone(),
    };
    let cancellation =
        tokio::spawn(async move { cancel_manager.cancel_agent_delegation(cancel_request).await });
    wait_for(|| {
        let current = manager
            .get_agent_delegation(&fixture.parent.id, &task.id)
            .unwrap();
        (current.phase() == DelegationTaskPhase::Cancelling).then_some(current)
    })
    .await;
    drop(guard);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), creation)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "vibex_use_task_cancelling"
    );
    tokio::time::timeout(Duration::from_secs(5), cancellation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let cancelled = manager
        .get_agent_delegation(&fixture.parent.id, &task.id)
        .unwrap();
    assert_eq!(cancelled.phase(), DelegationTaskPhase::Cancelled);
    assert!(cancelled.cancellation_requested_at_ms.is_some());
    assert!(cancelled.child_session_id.is_none());
    let conn = manager.open_migrated().unwrap();
    assert!(
        VibexUseExecutionRepository::list_for_task(&conn, &task.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(SessionRepository::list(&conn, false).unwrap().len(), 1);
}

#[tokio::test]
async fn dispatch_records_effective_runtime_without_overwriting_the_controller_revision() {
    let fixture = DelegationFixture::new().await;
    let manager = fixture.harness.manager();
    let first = manager
        .create_task_delegation(fixture.request("runtime-proof"))
        .await
        .unwrap();
    let task = fixture.wait_for_task(&first.delegation.id).await;
    let child = task.child_session_id.as_ref().unwrap();
    let controller = SessionControllerRepository::get(&manager.open_migrated().unwrap(), child)
        .unwrap()
        .unwrap();
    fixture.switch_mode(child, "review").await;
    let selected = fixture.switch_mode(child, "build").await;
    assert_ne!(selected.selection_revision as u64, controller.revision);
    let round = fixture.enqueue_round(&task, "proved-followup");
    let completed = fixture.wait_for_execution(&round.id).await;
    assert_eq!(completed.outcome, ExecutionOutcome::Completed);
    assert_eq!(
        completed.runtime_selection_revision,
        selected.selection_revision as u64
    );
    let projected = fixture.wait_for_task(&task.id).await;
    assert_eq!(projected.controller_revision, controller.revision);
    assert_eq!(
        projected
            .effective_runtime
            .as_ref()
            .unwrap()
            .mode_id
            .as_deref(),
        Some("build")
    );
    assert_eq!(projected.requested_runtime.as_ref().unwrap().mode_id, None);
    assert_eq!(
        SessionControllerRepository::get(&manager.open_migrated().unwrap(), child)
            .unwrap()
            .unwrap()
            .revision,
        controller.revision
    );
}

#[tokio::test]
async fn deadline_cancellation_settles_every_queued_round_without_dispatching_it() {
    let fixture = DelegationFixture::new().await;
    let manager = fixture.harness.manager();
    let first = manager
        .create_task_delegation(fixture.request("deadline-rounds"))
        .await
        .unwrap();
    let task = fixture.wait_for_task(&first.delegation.id).await;
    let coordinator = manager.message_submission_coordinator().unwrap();
    let guard = coordinator
        .pause_session_dispatch(task.child_session_id.as_ref().unwrap())
        .await
        .unwrap();
    let queued = [
        fixture.enqueue_round(&task, "queued-one"),
        fixture.enqueue_round(&task, "queued-two"),
    ];
    let deadline = vibex_db::VibexUseBudgetRepository::task_deadline(
        &manager.open_migrated().unwrap(),
        &task.id,
    )
    .unwrap()
    .unwrap();
    let maintenance_manager = manager.clone();
    let maintenance = tokio::spawn(async move {
        maintenance_manager
            .enforce_delegation_deadlines(deadline)
            .await
    });
    wait_for(|| {
        let current = manager
            .get_agent_delegation(&fixture.parent.id, &task.id)
            .unwrap();
        current
            .cancellation_requested_at_ms
            .is_some()
            .then_some(current)
    })
    .await;
    drop(guard);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), maintenance)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        1
    );
    for round in queued {
        let execution = fixture.wait_for_execution(&round.id).await;
        assert_eq!(execution.outcome, ExecutionOutcome::Cancelled);
        let submission = MessageSubmissionRepository::get(
            &manager.open_migrated().unwrap(),
            &round.submission_id,
        )
        .unwrap()
        .unwrap();
        assert_eq!(submission.status, MessageSubmissionStatus::Cancelled);
        assert!(submission.dispatched_at_ms.is_none());
    }
    let cancelled = manager
        .get_agent_delegation(&fixture.parent.id, &task.id)
        .unwrap();
    assert_eq!(cancelled.phase(), DelegationTaskPhase::Cancelled);
    assert!(
        SessionControllerRepository::get(
            &manager.open_migrated().unwrap(),
            task.child_session_id.as_ref().unwrap(),
        )
        .unwrap()
        .unwrap()
        .owner_task_id
        .is_none()
    );
}

#[tokio::test]
async fn cascade_cancels_taskless_external_work_while_single_task_cancellation_preserves_it() {
    for cascade in [false, true] {
        let fixture = DelegationFixture::new().await;
        let manager = fixture.harness.manager();
        let first = manager
            .create_task_delegation(fixture.request("external-work"))
            .await
            .unwrap();
        let task = fixture.wait_for_task(&first.delegation.id).await;
        let child = task.child_session_id.as_ref().unwrap();
        let external = manager
            .create_session(CreateAgentSessionRequest {
                session_id: None,
                defer_runtime_materialization: false,
                runtime: fixture.selection.clone(),
                workspace_root: fixture.parent.workspace_root.clone(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                title: Some("Independent external session".into()),
                safety: Some(AgentSessionSafety::workspace_write_ask_on_risk()),
            })
            .await
            .unwrap();
        let mut conn = manager.open_migrated().unwrap();
        SessionGrantRepository::grant(&conn, child, &external.id, "controlled", "human:local")
            .unwrap();
        assert!(
            vibex_db::SessionOwnershipRepository::parent_of(&conn, &external.id)
                .unwrap()
                .is_none()
        );
        let coordinator = manager.message_submission_coordinator().unwrap();
        let guard = coordinator
            .pause_session_dispatch(&external.id)
            .await
            .unwrap();
        let queued = fixture.enqueue_external_round(child, &external, "external-round");
        assert!(queued.task_ref.is_none());
        assert_eq!(queued.outcome, ExecutionOutcome::Queued);

        // The runtime service captures the cascade before asking the manager
        // to stop each affected task. The external session has no navigation
        // edge or task of its own, so only its durable work origin connects it.
        vibex_db::request_delegation_tree_cancellation(&mut conn, &task.id, cascade).unwrap();
        assert_eq!(
            VibexUseCancellationRepository::executions_for_task(&conn, &task.id)
                .unwrap()
                .iter()
                .any(|execution| execution.id == queued.id),
            cascade
        );
        let cancel_manager = manager.clone();
        let cancel_request = CancelAgentDelegationRequest {
            parent_session_id: fixture.parent.id.clone(),
            delegation_id: task.id.clone(),
        };
        let cancellation =
            tokio::spawn(
                async move { cancel_manager.cancel_agent_delegation(cancel_request).await },
            );
        drop(guard);
        tokio::time::timeout(Duration::from_secs(5), cancellation)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let execution = fixture.wait_for_execution(&queued.id).await;
        let submission = MessageSubmissionRepository::get(&conn, &queued.submission_id)
            .unwrap()
            .unwrap();
        if cascade {
            assert_eq!(execution.outcome, ExecutionOutcome::Cancelled);
            assert_eq!(submission.status, MessageSubmissionStatus::Cancelled);
            assert!(submission.dispatched_at_ms.is_none());
        } else {
            assert_eq!(execution.outcome, ExecutionOutcome::Completed);
            assert_eq!(submission.status, MessageSubmissionStatus::Completed);
            assert!(submission.dispatched_at_ms.is_some());
        }
        assert_eq!(
            fixture.wait_for_task(&task.id).await.phase(),
            DelegationTaskPhase::Cancelled
        );
    }
}

#[tokio::test]
async fn an_interrupt_during_provider_preparation_stops_only_that_round_and_keeps_the_task_usable()
{
    for policy in [
        DelegationCompletionPolicy::OwnerReview,
        DelegationCompletionPolicy::SingleTurnLegacy,
    ] {
        let provider = Arc::new(PreparationBlockedProvider::default());
        let fixture = DelegationFixture::with_provider(provider.clone()).await;
        let manager = fixture.harness.manager();
        provider.block_next.store(true, Ordering::SeqCst);
        let mut request = fixture.request("interrupt-preparation");
        request.completion_policy = policy;
        let first = manager.create_task_delegation(request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), provider.prepared.notified())
            .await
            .unwrap();
        let task = manager
            .get_agent_delegation(&fixture.parent.id, &first.delegation.id)
            .unwrap();
        let child = task.child_session_id.as_ref().unwrap();
        let controller = SessionControllerRepository::get(&manager.open_migrated().unwrap(), child)
            .unwrap()
            .unwrap();
        let already_queued = (policy == DelegationCompletionPolicy::OwnerReview)
            .then(|| fixture.enqueue_round(&task, "queued-before-interrupt"));
        tokio::time::timeout(
            Duration::from_secs(5),
            manager.interrupt_execution(&first.execution_id),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(provider.interrupts.load(Ordering::SeqCst), 1);
        assert!(provider.prompts.lock().unwrap().is_empty());
        assert!(
            VibexUseInterruptRepository::is_requested(
                &manager.open_migrated().unwrap(),
                &first.execution_id,
            )
            .unwrap()
        );
        let coordinator = manager.message_submission_coordinator().unwrap();
        let pause = coordinator.pause_session_dispatch(child).await.unwrap();
        provider.release.notify_one();
        let interrupted = fixture.wait_for_execution(&first.execution_id).await;
        assert_eq!(interrupted.outcome, ExecutionOutcome::Cancelled);
        assert_eq!(interrupted.stop_reason.as_deref(), Some("interrupted"));
        assert!(provider.prompts.lock().unwrap().is_empty());
        let retained = manager
            .get_agent_delegation(&fixture.parent.id, &task.id)
            .unwrap();
        assert_eq!(retained.phase(), DelegationTaskPhase::Active);
        assert!(retained.cancellation_requested_at_ms.is_none());
        assert_eq!(retained.controller_revision, controller.revision);
        assert_eq!(
            SessionControllerRepository::get(&manager.open_migrated().unwrap(), child)
                .unwrap()
                .unwrap()
                .owner_task_id,
            Some(task.id.clone())
        );
        let follow_up = fixture.enqueue_round(&retained, "accepted-after-interrupt");
        for execution in already_queued.iter().chain(std::iter::once(&follow_up)) {
            let submission = MessageSubmissionRepository::get(
                &manager.open_migrated().unwrap(),
                &execution.submission_id,
            )
            .unwrap()
            .unwrap();
            assert!(!submission.status.is_terminal());
            assert!(submission.dispatched_at_ms.is_none());
        }
        drop(pause);
        for execution in already_queued.iter().chain(std::iter::once(&follow_up)) {
            assert_eq!(
                fixture.wait_for_execution(&execution.id).await.outcome,
                ExecutionOutcome::Completed
            );
        }
        assert_eq!(
            provider.prompts.lock().unwrap().len(),
            1 + usize::from(already_queued.is_some())
        );
        manager
            .interrupt_execution(&first.execution_id)
            .await
            .unwrap();
        assert_eq!(provider.interrupts.load(Ordering::SeqCst), 1);
    }
}

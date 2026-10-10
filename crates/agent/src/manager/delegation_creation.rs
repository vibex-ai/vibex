use super::*;
use vibex_core::{ErrorCategory, VibexUseOperationState};
use vibex_db::VibexUseOperationRepository;

impl AgentManager {
    /// Resumes the same accepted creation across retries and runtime restarts.
    pub async fn create_task_delegation(
        self: &Arc<Self>,
        mut request: CreateAgentDelegationRequest,
    ) -> VibexResult<DelegationStartOutcome> {
        validate_delegation_request(&mut request)?;
        let fingerprint = request.payload_fingerprint();
        let operation_id = self.ensure_delegation_operation(&request, &fingerprint)?;
        request.operation_id = Some(operation_id.clone());
        let operation = VibexUseOperationRepository::set_checkpoint_once(
            &self.open_migrated()?,
            &operation_id,
            "delegation_request",
            &serde_json::to_string(&request).map_err(|_| {
                VibexError::storage(
                    "agent_delegation_checkpoint_failed",
                    "failed to encode delegation request",
                )
            })?,
        )?;
        let result = self
            .start_delegation_operation(request.clone(), fingerprint, operation_id.clone())
            .await;
        if let Err(error) = &result {
            // A cancellation fence wins over a concurrent creation failure.
            // Settle it only after creation released its lifecycle lock.
            if let Some(task) = AgentDelegationRepository::get_by_parent_and_idempotency(
                &self.open_migrated()?,
                &request.parent_session_id,
                &request.idempotency_key,
            )? && task.phase() == DelegationTaskPhase::Cancelling
            {
                self.cancel_agent_delegation(CancelAgentDelegationRequest {
                    parent_session_id: task.parent_session_id,
                    delegation_id: task.id,
                })
                .await?;
            }
            if operation.tool == "vibex_delegate_legacy" {
                VibexUseOperationRepository::update_state(
                    &self.open_migrated()?,
                    &operation_id,
                    VibexUseOperationState::Failed,
                    Some(&error.code),
                    Some(&error.message),
                    matches!(
                        error.category,
                        ErrorCategory::Storage | ErrorCategory::Process
                    ),
                )?;
            }
        } else if operation.tool == "vibex_delegate_legacy" {
            VibexUseOperationRepository::update_state(
                &self.open_migrated()?,
                &operation_id,
                VibexUseOperationState::Succeeded,
                None,
                None,
                false,
            )?;
        }
        if operation.tool == "vibex_delegate_legacy" {
            self.observability.increment(
                RuntimeMetricName::DelegationAdmission,
                None,
                if result.is_ok() {
                    RuntimeMetricResult::Success
                } else {
                    RuntimeMetricResult::Rejected
                },
            );
        }
        result
    }

    /// Starts one delegated task and reports its full durable identity.
    async fn start_delegation_operation(
        self: &Arc<Self>,
        request: CreateAgentDelegationRequest,
        fingerprint: String,
        operation_id: VibexOperationId,
    ) -> VibexResult<DelegationStartOutcome> {
        let (parent, selection, delegation) = {
            let mut conn = self.open_migrated()?;
            let parent =
                SessionRepository::get(&conn, &request.parent_session_id)?.ok_or_else(|| {
                    VibexError::validation("session_not_found", "Agent session was not found")
                })?;
            if let Some(existing) = AgentDelegationRepository::get_by_parent_and_idempotency(
                &conn,
                &parent.id,
                &request.idempotency_key,
            )? {
                // A retry of the same request returns the original task. A
                // different request under the same key is a conflict, not a
                // silent reuse of somebody else's work.
                if let Some(existing_fingerprint) = existing.payload_fingerprint.as_deref()
                    && existing_fingerprint != fingerprint
                {
                    return Err(VibexError::conflict(
                        "idempotency_payload_conflict",
                        "this idempotency key was already used for a different request",
                    )
                    .with_diagnostic("delegationId", existing.id.as_str()));
                }
                if let Some(outcome) = self.resume_delegation_outcome(&conn, existing)? {
                    self.start_execution_observer(&outcome.execution_id)?;
                    return Ok(outcome);
                }
            }
            if matches!(
                parent.state,
                AgentSessionState::Closed | AgentSessionState::Archived
            ) {
                return Err(VibexError::conflict(
                    "agent_delegation_parent_closed",
                    "a closed Agent session cannot delegate work",
                ));
            }
            let depth = vibex_db::vibex_use_delegation_depth(&conn, &parent.id)?;
            let budget_policy = vibex_db::VibexUseBudgetRepository::policy(&conn)?;
            if depth >= budget_policy.max_depth {
                return Err(VibexError::conflict(
                    "delegation_depth_exceeded",
                    "Agent delegation nesting depth is limited",
                ));
            }
            let checkpoint = VibexUseOperationRepository::get(&conn, &operation_id)?;
            let selection = if let Some(saved) = checkpoint
                .as_ref()
                .and_then(|operation| operation.checkpoint.get("delegation_selection"))
            {
                serde_json::from_str(saved).map_err(|_| {
                    VibexError::storage(
                        "agent_delegation_checkpoint_failed",
                        "saved delegation runtime is invalid",
                    )
                })?
            } else if let Some(selection) = request.runtime_selection.clone() {
                selection
            } else {
                let runtime_state =
                    AgentSessionRuntimeRepository::get_runtime_state(&conn, &parent.id)?
                        .ok_or_else(|| {
                            VibexError::conflict(
                                "agent_delegation_parent_runtime_missing",
                                "parent Agent session has no durable runtime selection",
                            )
                        })?;
                if runtime_state.runtime_selection_status
                    != Some(SessionRuntimeSelectionStatus::Ready)
                    || runtime_state.pending_switch_id.is_some()
                    || runtime_state.desired_runtime_selection
                        != runtime_state.effective_runtime_selection
                {
                    return Err(VibexError::conflict(
                        "agent_delegation_parent_runtime_not_ready",
                        "parent Agent runtime must be ready before delegation",
                    ));
                }
                let inherited_runtime =
                    runtime_state.effective_runtime_selection.ok_or_else(|| {
                        VibexError::conflict(
                            "agent_delegation_parent_runtime_missing",
                            "parent Agent session has no effective runtime selection",
                        )
                    })?;
                self.resolve_delegation_runtime(&conn, &parent, &inherited_runtime, &request)?
            };
            let saved = VibexUseOperationRepository::set_checkpoint_once(
                &conn,
                &operation_id,
                "delegation_selection",
                &serde_json::to_string(&selection).map_err(|_| {
                    VibexError::storage(
                        "agent_delegation_checkpoint_failed",
                        "failed to encode delegation runtime",
                    )
                })?,
            )?;
            let selection: SessionRuntimeSelection =
                serde_json::from_str(&saved.checkpoint["delegation_selection"]).map_err(|_| {
                    VibexError::storage(
                        "agent_delegation_checkpoint_failed",
                        "saved delegation runtime is invalid",
                    )
                })?;
            let root_session_id = vibex_db::vibex_use_root_session(&conn, &parent.id)?;
            let now = unix_timestamp_ms();
            let status = AgentDelegationStatus::Starting;
            let mut delegation = AgentDelegation::single_turn_legacy(
                parent.id.clone(),
                request.idempotency_key.clone(),
                request
                    .title
                    .clone()
                    .unwrap_or_else(|| "Delegated task".to_string()),
                bounded_text(&request.task, MAX_AGENT_DELEGATION_SUMMARY_CHARS),
                Some(selection.agent_id.clone()),
                status,
                now,
            );
            delegation.requested_agent_id = request.agent_id.clone();
            delegation.completion_policy = request.completion_policy;
            delegation.phase = DelegationTaskPhase::Starting;
            delegation.ownership_kind = request.ownership_kind;
            delegation.root_session_id = Some(root_session_id.clone());
            delegation.follows_task_id = request.follows_task_id.clone();
            delegation.context_refs = request.context_refs.clone();
            delegation.acceptance_criteria = request.acceptance_criteria.clone();
            delegation.payload_fingerprint = Some(fingerprint.clone());
            delegation.requested_runtime = Some(delegation_runtime_summary(&selection));
            // Reserving this row atomically claims its child-session creation
            // slot. A retried request returns this starting row.
            match AgentDelegationRepository::reserve_or_get(
                &mut conn,
                &delegation,
                budget_policy.root_execution_limit,
            )? {
                AgentDelegationReservation::Existing(existing) => (parent, selection, existing),
                AgentDelegationReservation::Claimed(persisted) => {
                    if let Some(follows) = request.follows_task_id.as_ref() {
                        AgentDelegationRepository::get(&conn, follows)?.ok_or_else(|| {
                            VibexError::validation(
                                "delegation_follows_task_not_found",
                                "the task this one continues was not found",
                            )
                        })?;
                    }
                    (parent, selection, persisted)
                }
            }
        };

        let lifecycle_lock = self.delegation_lifecycle_lock(&delegation.id)?;
        let lifecycle_guard = lifecycle_lock.lock().await;
        let current = self.get_agent_delegation(&delegation.parent_session_id, &delegation.id)?;
        if let Some(outcome) =
            self.resume_delegation_outcome(&self.open_migrated()?, current.clone())?
        {
            self.start_execution_observer(&outcome.execution_id)?;
            return Ok(outcome);
        }
        if current.phase().is_terminal() || current.phase() == DelegationTaskPhase::Cancelling {
            return Err(VibexError::conflict(
                "vibex_use_task_cancelling",
                "the task no longer accepts a first execution",
            ));
        }
        vibex_db::VibexUseBudgetRepository::check_task_deadline(
            &self.open_migrated()?,
            &current.id,
            unix_timestamp_ms(),
        )?;
        let coordinator = match self.message_submission.get().and_then(Weak::upgrade) {
            Some(coordinator) => coordinator,
            None => {
                let error = VibexError::process(
                    "message_submission_coordinator_unavailable",
                    "durable message submission coordinator is unavailable",
                );
                let _ = self.update_agent_delegation_status(
                    &delegation.id,
                    AgentDelegationStatus::Failed,
                    None,
                    Some(&error.code),
                );
                drop(lifecycle_guard);
                return Err(error);
            }
        };

        // Reserve the child id before deferred materialization so an error
        // after session persistence can still be cleaned up deterministically.
        let checkpoint =
            vibex_db::VibexUseOperationRepository::get(&self.open_migrated()?, &operation_id)?
                .ok_or_else(|| {
                    VibexError::storage(
                        "vibex_use_operation_missing",
                        "delegation operation was not found",
                    )
                })?;
        let child_session_id = current
            .child_session_id
            .clone()
            .or(checkpoint
                .checkpoint
                .get("delegation_child_id")
                .map(VibexSessionId::parse)
                .transpose()?)
            .unwrap_or_default();
        let saved = VibexUseOperationRepository::set_checkpoint_once(
            &self.open_migrated()?,
            &operation_id,
            "delegation_child_id",
            child_session_id.as_str(),
        )?;
        let child_session_id = VibexSessionId::parse(&saved.checkpoint["delegation_child_id"])?;
        vibex_db::VibexUseOperationRepository::append_resource(
            &self.open_migrated()?,
            &operation_id,
            &vibex_core::VibexUseOperationResource {
                kind: "task".into(),
                reference: VibexUseRef::task(&delegation.id),
            },
        )?;
        let existing_child = SessionRepository::get(&self.open_migrated()?, &child_session_id)?;
        let created_child = if let Some(child) = existing_child {
            Ok(child)
        } else {
            self.create_session_deferred_with_id(
                CreateAgentSessionRequest {
                    session_id: None,
                    defer_runtime_materialization: false,
                    runtime: selection.clone(),
                    workspace_root: parent.workspace_root.clone(),
                    workspace_mode: parent.workspace_mode,
                    title: Some(delegation.title.clone()),
                    safety: Some(parent.safety.clone()),
                },
                child_session_id.clone(),
            )
            .await
        };
        let child = match created_child {
            Ok(child) => child,
            Err(error) => {
                // Deferred creation persists the logical session before it
                // starts runtime materialization. Remove that known id on any
                // synchronous failure so the failed delegation is not paired
                // with an unreachable child-session view.
                let _ = self.delete_session(&child_session_id).await;
                let _ = self.update_agent_delegation_status(
                    &delegation.id,
                    AgentDelegationStatus::Failed,
                    None,
                    Some(&error.code),
                );
                return Err(error);
            }
        };

        // Transaction boundary: linking the child session and recording whose
        // child it is happen together, because an ownership edge without a
        // child (or the reverse) is what makes a tree node unreachable. The
        // parent timeline card is written afterwards on purpose — it is a
        // projection that `reconcile_agent_delegations` rebuilds, so it must
        // never be the reason a started task is rolled back.
        let delegation = {
            let mut conn = self.open_migrated()?;
            let linked = {
                let tx = conn.transaction().map_err(|error| {
                    VibexError::storage(
                        "agent_delegation_attach_transaction_failed",
                        "failed to start the delegation attach transaction",
                    )
                    .with_diagnostic("error", error.to_string())
                })?;
                let linked = if current.child_session_id.as_ref() == Some(&child.id) {
                    Some(current.clone())
                } else {
                    AgentDelegationRepository::attach_claimed_child_session(
                        &tx,
                        &delegation.id,
                        &child.id,
                        &selection.agent_id,
                    )?
                };
                if linked.is_some() {
                    // The ownership edge is the single answer to "whose child
                    // is this" for the tree, the depth check and the cascade
                    // delete.
                    SessionOwnershipRepository::upsert(
                        &tx,
                        &child.id,
                        &delegation.parent_session_id,
                        Some(&delegation.id),
                    )?;
                }
                tx.commit().map_err(|error| {
                    VibexError::storage(
                        "agent_delegation_attach_commit_failed",
                        "failed to commit the delegation attach transaction",
                    )
                    .with_diagnostic("error", error.to_string())
                })?;
                linked
            };
            let Some(delegation) = linked else {
                // The lifecycle lock normally rules this out. If durable state
                // changed outside this process, remove the unlinked child rather
                // than leaving an orphaned internal child session.
                drop(conn);
                let _ = self.delete_session(&child.id).await;
                drop(lifecycle_guard);
                return self
                    .get_agent_delegation(&delegation.parent_session_id, &delegation.id)
                    .and_then(|delegation| self.delegation_start_outcome(delegation));
            };
            let item = self.append_delegation_timeline(
                &mut conn,
                &delegation,
                AgentDelegationStatus::Starting,
                None,
            )?;
            AgentDelegationRepository::attach_parent_timeline_item(
                &conn,
                &delegation.id,
                &item.id,
            )?;
            AgentDelegationRepository::get(&conn, &delegation.id)?.ok_or_else(|| {
                VibexError::storage(
                    "agent_delegation_missing_after_start",
                    "Agent delegation disappeared while starting",
                )
            })?
        };

        let provenance = MessageProvenance::DelegatedInput {
            actor_session_ref: VibexUseRef::session(&delegation.parent_session_id),
            task_ref: Some(VibexUseRef::task(&delegation.id)),
            operation_ref: VibexUseRef::operation(&operation_id),
        };
        let prompt_context = delegation_prompt_context(
            request.prompt_context.as_deref(),
            &request.acceptance_criteria,
        );
        let submission_id = match coordinator.prepare_submission(SendAgentMessageRequest {
            session_id: child.id.clone(),
            message_idempotency_key: format!("delegation:{}", delegation.id.as_str()),
            desired_runtime: selection.clone(),
            text: request.task.clone(),
            attachments: request.attachments.clone(),
            mentions: Vec::new(),
            reasoning_effort: selection.reasoning_effort.clone(),
            correlation_id: None,
            delivery: UserMessageDelivery::Prompt,
            prompt_context,
            provenance: provenance.clone(),
        }) {
            Ok(id) => id,
            Err(error) => {
                let _ = self.update_agent_delegation_status(
                    &delegation.id,
                    AgentDelegationStatus::Failed,
                    None,
                    Some(&error.code),
                );
                return Err(error);
            }
        };

        let execution =
            VibexUseExecutionRepository::get_by_submission(&self.open_migrated()?, &submission_id)?
                .ok_or_else(|| {
                    VibexError::storage(
                        "vibex_use_execution_missing",
                        "accepted delegation has no execution",
                    )
                })?;
        drop(lifecycle_guard);

        self.start_execution_observer(&execution.id)?;
        self.delegation_start_outcome(
            AgentDelegationRepository::get(&self.open_migrated()?, &delegation.id)?.ok_or_else(
                || {
                    VibexError::storage(
                        "agent_delegation_missing_after_start",
                        "Agent delegation disappeared while starting",
                    )
                },
            )?,
        )
    }
}

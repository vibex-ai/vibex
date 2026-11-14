use super::*;
use vibex_db::VibexUseCancellationRepository;

impl AgentManager {
    pub(super) async fn cancel_agent_delegation_inner(
        self: &Arc<Self>,
        request: CancelAgentDelegationRequest,
    ) -> VibexResult<AgentDelegation> {
        let delegation =
            self.get_agent_delegation(&request.parent_session_id, &request.delegation_id)?;
        // Fence before the lifecycle lock: creation may be awaiting a provider.
        // Any previously captured cascade remains part of this same stop.
        vibex_db::request_delegation_tree_cancellation(
            &mut self.open_migrated()?,
            &delegation.id,
            false,
        )?;
        let lifecycle_lock = self.delegation_lifecycle_lock(&delegation.id)?;
        let _lifecycle_guard = lifecycle_lock.lock().await;
        let fenced =
            self.get_agent_delegation(&request.parent_session_id, &request.delegation_id)?;
        let executions = VibexUseCancellationRepository::executions_for_task(
            &self.open_migrated()?,
            &fenced.id,
        )?;
        let mut sessions =
            std::collections::BTreeMap::<VibexSessionId, Vec<VibexExecutionId>>::new();
        for execution in executions {
            if execution.is_settled() {
                continue;
            }
            self.start_execution_observer(&execution.id)?;
            if let Some(session) = execution.session_ref.session_id() {
                sessions.entry(session).or_default().push(execution.id);
            }
        }
        // A card belongs to the child session, not to a round. A task whose
        // round already settled — the child is parked on a card when the stop
        // arrives — contributes no unsettled execution, so the session has to
        // be taken from the task itself or the card outlives the stop.
        if let Some(child) = fenced.child_session_id.clone() {
            sessions.entry(child).or_default();
        }
        for (session, ids) in sessions {
            let coordinator = self.message_submission_coordinator();
            let _dispatch_guard = match coordinator {
                Some(coordinator) => Some(coordinator.pause_session_dispatch(&session).await?),
                None => None,
            };
            // Recheck after acquiring the dispatch pause. An earlier round may
            // have ended while waiting; never interrupt its successor.
            let mut admitted = None;
            for id in ids {
                let conn = self.open_migrated()?;
                if let Some(execution) = VibexUseExecutionRepository::get(&conn, &id)?
                    && !execution.is_settled()
                    && let Some(submission) =
                        MessageSubmissionRepository::get(&conn, &execution.submission_id)?
                    && matches!(
                        submission.status,
                        vibex_core::MessageSubmissionStatus::AboutToPrompt
                            | vibex_core::MessageSubmissionStatus::Dispatched
                    )
                {
                    admitted = Some(execution);
                    break;
                }
            }
            // The child's own approval cards die with the task. The provider
            // interrupt below only silences the process: the durable rows stay
            // `Pending`, and an ACP card has no `expires_at_ms` for the expiry
            // sweep to reclaim. Left in place the card stays clickable, the
            // reader's late approval runs the work the stop was meant to
            // prevent, and the parent's wait never sees a terminal task. The
            // rows are cleared before the interrupt so the unwinding turn
            // cannot re-park the session on them.
            let mut abandoned = {
                let mut conn = self.open_migrated()?;
                self.cancel_pending_user_input_requests(&mut conn, &session, "task_cancelled")?
            };
            if let Some(execution) = admitted
                && let Err(error) = self
                    .interrupt_captured_execution(&session, &execution)
                    .await
            {
                transition_delegation(
                    &self.open_migrated()?,
                    &fenced.id,
                    DelegationTaskPhase::Cancelling,
                    Some("Waiting for the execution to stop"),
                    Some(&error.code),
                )?;
            }
            // Answer the native callback the cleared card was raised from, so
            // the provider stops waiting on a decision that can no longer
            // arrive instead of keeping the attachment parked forever.
            for resolution in abandoned.drain(..) {
                self.drain_expired_permission(&session, resolution).await;
            }
            // The cleared rows are a fact every client must see: a surface that
            // keeps painting the card would still offer a button whose answer
            // is now refused.
            let conn = self.open_migrated()?;
            if let Some(settled) = self.client_session_snapshot(&conn, &session)? {
                self.publish_root_session_update(&conn, settled);
            }
        }
        // The provider's interrupt acknowledgement is not prompt completion.
        // Only settled, unambiguous execution facts may close the cancellation.
        let mut conn = self.open_migrated()?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| {
                VibexError::storage("agent_delegation_cancel_failed", error.to_string())
            })?;
        VibexUseCancellationRepository::settle_task(&tx, &fenced.id)?;
        tx.commit().map_err(|error| {
            VibexError::storage("agent_delegation_cancel_failed", error.to_string())
        })?;
        self.get_agent_delegation(&request.parent_session_id, &fenced.id)
    }

    /// Called while the coordinator dispatch pause is held. Unlike a human
    /// session-wide stop, it does not cancel other inputs queued in this session.
    pub(super) async fn interrupt_captured_execution(
        &self,
        session_id: &VibexSessionId,
        execution: &DelegationExecution,
    ) -> VibexResult<()> {
        let conn = self.open_migrated()?;
        let Some(submission) = MessageSubmissionRepository::get(&conn, &execution.submission_id)?
        else {
            return Ok(());
        };
        if !matches!(
            submission.status,
            vibex_core::MessageSubmissionStatus::AboutToPrompt
                | vibex_core::MessageSubmissionStatus::Dispatched
        ) {
            return Ok(());
        }
        let session = SessionRepository::get(&conn, session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        let (selection, binding, _, route) = self.durable_session_execution(&conn, &session)?;
        let provider = self.runtime(&route)?;
        let capabilities = provider.capabilities_for_profile(selection.provider_profile_id());
        if !capabilities.interrupt {
            return Err(VibexError::capability(
                "acp_interrupt_unsupported",
                "this provider profile does not support interrupt",
            ));
        }
        provider
            .interrupt(ProviderSessionHandle {
                binding,
                capabilities,
            })
            .await
    }
}

use super::*;
use vibex_core::{VibexUseOperation, VibexUseOperationState};
use vibex_db::{VibexUseOperationRepository, VibexUseOperationReservation};

/// Builds the hidden worker input from the declared context and task criteria.
pub fn delegation_prompt_context(context: Option<&str>, criteria: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(context) = context.filter(|value| !value.is_empty()) {
        parts.push(context.to_string());
    }
    if !criteria.is_empty() {
        parts.push(format!(
            "Acceptance criteria:\n{}",
            criteria
                .iter()
                .map(|criterion| format!("- {criterion}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

impl AgentManager {
    pub(super) fn record_delegation_dispatch_runtime(
        &self,
        request: &ProviderTurnRequest,
        identity: Option<&ProviderTurnExecutionIdentity>,
    ) -> VibexResult<()> {
        let Some(submission_id) = request.message_submission_id.as_ref() else {
            return Ok(());
        };
        let conn = self.open_migrated()?;
        let Some(execution) = VibexUseExecutionRepository::get_by_submission(&conn, submission_id)?
        else {
            return Ok(());
        };
        if let Some(payload) = MessageSubmissionRepository::get_payload(&conn, submission_id)? {
            vibex_db::authorize_vibex_use_submission(&conn, &payload.request, true)?;
        }
        let state = AgentSessionRuntimeRepository::get_runtime_state(&conn, &request.session_id)?
            .ok_or_else(|| {
            VibexError::conflict(
                "message_submission_runtime_gate_changed",
                "runtime state changed before dispatch",
            )
        })?;
        if state.effective_runtime_selection != request.required_runtime
            || state.desired_runtime_selection != request.required_runtime
            || state.runtime_selection_status != Some(SessionRuntimeSelectionStatus::Ready)
            || state.pending_switch_id.is_some()
            || identity.is_none_or(|identity| {
                state.current_binding_id.as_ref() != Some(&identity.binding_id)
                    || state.activation_generation != identity.activation_generation
            })
        {
            return Err(VibexError::conflict(
                "message_submission_runtime_gate_changed",
                "runtime state changed before dispatch",
            ));
        }
        let selection = state.effective_runtime_selection.ok_or_else(|| {
            VibexError::conflict(
                "message_submission_runtime_selection_missing",
                "runtime selection is unavailable",
            )
        })?;
        let mut summary = delegation_runtime_summary(&selection);
        if let Some(model) = identity.and_then(|identity| identity.model_id.as_ref()) {
            summary.model_id = Some(model.clone());
            summary.configuration_label = model.clone();
        }
        vibex_db::attach_delegation_runtime_summary(
            &conn,
            &execution.id,
            &summary,
            u64::try_from(state.selection_revision).unwrap_or_default(),
        )?;
        self.observability.observe_duration_ms(
            RuntimeMetricName::DelegationQueueWait,
            None,
            RuntimeMetricResult::Success,
            u64::try_from(unix_timestamp_ms().saturating_sub(execution.created_at_ms))
                .unwrap_or_default(),
        );
        Ok(())
    }

    pub fn execution_progress(&self) -> Arc<tokio::sync::Notify> {
        self.execution_progress.clone()
    }
    pub(super) fn ensure_delegation_operation(
        &self,
        request: &CreateAgentDelegationRequest,
        fingerprint: &str,
    ) -> VibexResult<VibexOperationId> {
        let conn = self.open_migrated()?;
        if let Some(id) = request.operation_id.as_ref() {
            let operation = VibexUseOperationRepository::get(&conn, id)?.ok_or_else(|| {
                VibexError::conflict(
                    "vibex_use_operation_missing",
                    "delegation operation was not found",
                )
            })?;
            if operation.actor_key != request.parent_session_id.as_str()
                || operation.authority != self.vibex_use_authority()
            {
                return Err(VibexError::conflict(
                    "vibex_use_scope_denied",
                    "delegation operation belongs to another session",
                ));
            }
            if operation.tool == "vibex_delegate_legacy"
                && operation.payload_fingerprint != fingerprint
            {
                return Err(VibexError::conflict(
                    "idempotency_payload_conflict",
                    "this operation belongs to another delegation request",
                ));
            }
            return Ok(id.clone());
        }
        let now = unix_timestamp_ms();
        let id = VibexOperationId::new();
        let operation = VibexUseOperation {
            operation_ref: VibexUseRef::operation(&id),
            id,
            authority: self.vibex_use_authority(),
            actor_key: request.parent_session_id.to_string(),
            tool: "vibex_delegate_legacy".to_string(),
            caller_key: request.idempotency_key.clone(),
            payload_fingerprint: fingerprint.to_string(),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: Default::default(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        match VibexUseOperationRepository::reserve(&conn, &operation, &request.idempotency_key)? {
            VibexUseOperationReservation::Claimed(operation)
            | VibexUseOperationReservation::Existing(operation) => Ok(operation.id),
            VibexUseOperationReservation::Conflict(_) => Err(VibexError::conflict(
                "idempotency_payload_conflict",
                "this idempotency key belongs to another delegation",
            )),
        }
    }

    /// Starts a detached, deduplicated observer for exactly one durable round.
    /// The caller's request lifetime never owns either the worker or observer.
    pub fn start_execution_observer(self: &Arc<Self>, id: &VibexExecutionId) -> VibexResult<()> {
        let mut observers = self.execution_observers.lock().map_err(|_| {
            VibexError::process(
                "vibex_use_observer_unavailable",
                "execution observer registry is unavailable",
            )
        })?;
        if !observers.insert(id.clone()) {
            return Ok(());
        }
        let manager = self.clone();
        let id = id.clone();
        tokio::spawn(async move {
            if let Err(error) = manager.observe_execution(&id).await {
                let _ = manager.settle_execution_by_id(
                    &id,
                    execution_error_outcome(&error.code),
                    Some(&error.code),
                    Some(&error.message),
                );
            }
            if let Ok(mut observers) = manager.execution_observers.lock() {
                observers.remove(&id);
            }
        });
        Ok(())
    }

    async fn observe_execution(self: &Arc<Self>, id: &VibexExecutionId) -> VibexResult<()> {
        let execution =
            VibexUseExecutionRepository::get(&self.open_migrated()?, id)?.ok_or_else(|| {
                VibexError::storage("vibex_use_execution_missing", "execution was not found")
            })?;
        if execution.is_settled() {
            vibex_db::settle_vibex_use_execution(&mut self.open_migrated()?, &execution)?;
            return Ok(());
        }
        let coordinator = self
            .message_submission
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                VibexError::process(
                    "message_submission_coordinator_unavailable",
                    "message submission coordinator is unavailable",
                )
            })?;
        let mut events = self.subscribe();
        let wait = coordinator.wait_for_submission(&execution.submission_id);
        tokio::pin!(wait);
        let mut attention_started: Option<std::time::Instant> = None;
        let items = loop {
            tokio::select! {
                result = &mut wait => {
                    if let Some(started) = attention_started.take() {
                        self.observability.observe_duration(
                            RuntimeMetricName::DelegationAttentionWait, None,
                            if result.is_ok() { RuntimeMetricResult::Success } else { RuntimeMetricResult::Failure },
                            started.elapsed(),
                        );
                    }
                    break result?;
                },
                _ = sleep(AGENT_DELEGATION_OBSERVE_INTERVAL) => {},
                _ = events.recv() => {},
            }
            let mut conn = self.open_migrated()?;
            let record = MessageSubmissionRepository::get(&conn, &execution.submission_id)?;
            if record.as_ref().is_some_and(|record| {
                matches!(
                    record.status,
                    vibex_core::MessageSubmissionStatus::AboutToPrompt
                        | vibex_core::MessageSubmissionStatus::Dispatched
                )
            }) {
                let blocked = self.pending_blocked_on(&record.as_ref().unwrap().session_id);
                if VibexUseExecutionRepository::record_attention(&mut conn, id, blocked.as_ref())? {
                    if let Some(started) = attention_started.take() {
                        self.observability.observe_duration(
                            RuntimeMetricName::DelegationAttentionWait,
                            None,
                            RuntimeMetricResult::Success,
                            started.elapsed(),
                        );
                    }
                    if blocked.is_some() {
                        attention_started = Some(std::time::Instant::now());
                    }
                    self.execution_progress.notify_waiters();
                }
            }
        };
        let (outcome, summary) = classify_execution(items.iter().map(|item| &item.payload));
        self.settle_execution_by_id(id, outcome, Some("end_turn"), summary.as_deref())?;
        if let Some(task_id) = execution.task_ref.as_ref().and_then(VibexUseRef::task_id)
            && let Some(task) = self.get_agent_delegation_by_id(&task_id)?
        {
            let mut conn = self.open_migrated()?;
            self.append_delegation_timeline(
                &mut conn,
                &task,
                task.status,
                task.result_summary.as_deref(),
            )?;
        }
        Ok(())
    }
}

fn execution_error_outcome(code: &str) -> ExecutionOutcome {
    if code.contains("ambiguous") {
        ExecutionOutcome::Ambiguous
    } else if code.contains("cancel") || code.contains("interrupted") {
        ExecutionOutcome::Cancelled
    } else if code.contains("authentication_required") || code.contains("auth_required") {
        ExecutionOutcome::AuthRequired
    } else if code.contains("empty_reply")
        || code.contains("empty_turn")
        || code == "acp_turn_without_output"
    {
        ExecutionOutcome::EmptyReply
    } else if code.contains("refusal") {
        ExecutionOutcome::Refusal
    } else if code.contains("max_tokens") {
        ExecutionOutcome::MaxTokens
    } else {
        ExecutionOutcome::Failed
    }
}

pub(super) fn execution_stop_outcome(reason: &str) -> Option<ExecutionOutcome> {
    match reason {
        "refusal" => Some(ExecutionOutcome::Refusal),
        "max_tokens" => Some(ExecutionOutcome::MaxTokens),
        "cancelled" | "canceled" | "interrupted" => Some(ExecutionOutcome::Cancelled),
        "end_turn" => None,
        _ => None,
    }
}

fn classify_execution<'a>(
    payloads: impl Iterator<Item = &'a TimelinePayload>,
) -> (ExecutionOutcome, Option<String>) {
    let mut reply = None;
    let mut actions = false;
    let mut error = None;
    for payload in payloads {
        match payload {
            TimelinePayload::AgentMessage(message) if !message.text.trim().is_empty() => {
                reply = Some(bounded_text(
                    &message.text,
                    MAX_AGENT_DELEGATION_SUMMARY_CHARS,
                ));
            }
            TimelinePayload::ToolCall(_)
            | TimelinePayload::Command(_)
            | TimelinePayload::FileOperation(_)
            | TimelinePayload::ImageGeneration(_)
            | TimelinePayload::WebSearch(_)
            | TimelinePayload::Collaboration(_) => actions = true,
            TimelinePayload::Error(failure) => {
                error = Some((
                    execution_error_outcome(&failure.code),
                    bounded_text(&failure.message, MAX_AGENT_DELEGATION_SUMMARY_CHARS),
                ))
            }
            _ => {}
        }
    }
    if let Some((outcome, message)) = error {
        (outcome, Some(message))
    } else if reply.is_some() {
        (ExecutionOutcome::Completed, reply)
    } else if actions {
        (ExecutionOutcome::ActionsOnly, None)
    } else {
        (ExecutionOutcome::EmptyReply, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_input_preserves_both_declared_context_and_acceptance_criteria() {
        let context = delegation_prompt_context(
            Some("[42] prior result"),
            &["run the regression".to_string()],
        )
        .unwrap();
        assert!(context.contains("[42] prior result"));
        assert!(context.contains("Acceptance criteria:\n- run the regression"));
        assert_eq!(delegation_prompt_context(None, &[]), None);
    }

    #[test]
    fn an_empty_round_cannot_reuse_a_previous_rounds_reply() {
        assert_eq!(
            classify_execution(std::iter::empty()),
            (ExecutionOutcome::EmptyReply, None)
        );
        let reply = TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text: "new result".to_string(),
            is_final: true,
        });
        assert_eq!(
            classify_execution([&reply].into_iter()),
            (ExecutionOutcome::Completed, Some("new result".to_string()))
        );
    }

    #[test]
    fn authentication_and_ambiguous_delivery_remain_distinct() {
        assert_eq!(
            execution_error_outcome("provider_authentication_required"),
            ExecutionOutcome::AuthRequired
        );
        assert_eq!(
            execution_error_outcome("message_submission_prompt_dispatch_ambiguous"),
            ExecutionOutcome::Ambiguous
        );
        assert_eq!(
            execution_error_outcome("message_submission_interrupted_before_dispatch"),
            ExecutionOutcome::Cancelled
        );
    }
}

use super::*;
use vibex_db::VibexUseInterruptRepository;

impl AgentManager {
    /// Interrupts one durable execution. Its task and other queued inputs stay
    /// available. Retrying this identity never selects a successor round.
    pub async fn interrupt_execution(
        self: &Arc<Self>,
        execution_id: &VibexExecutionId,
    ) -> VibexResult<()> {
        let execution = VibexUseExecutionRepository::get(&self.open_migrated()?, execution_id)?
            .ok_or_else(|| {
                VibexError::validation("vibex_use_execution_missing", "execution was not found")
            })?;
        let session_id = execution.session_ref.session_id().ok_or_else(|| {
            VibexError::storage(
                "vibex_use_execution_session_invalid",
                "execution session is invalid",
            )
        })?;
        let coordinator = self.message_submission_coordinator();
        let _dispatch_guard = match coordinator.as_ref() {
            Some(coordinator) => Some(coordinator.pause_session_dispatch(&session_id).await?),
            None => None,
        };
        let mut conn = self.open_migrated()?;
        if !VibexUseInterruptRepository::request(&conn, execution_id)? {
            // Recover a caller that stopped after the queue status committed
            // but before its execution observer committed the final result.
            if VibexUseInterruptRepository::is_requested(&conn, execution_id)?
                && let Some(submission) =
                    MessageSubmissionRepository::get(&conn, &execution.submission_id)?
                && submission.status.is_terminal()
            {
                self.settle_execution_by_id(
                    execution_id,
                    if submission.status
                        == vibex_core::MessageSubmissionStatus::AmbiguousPromptDispatch
                    {
                        ExecutionOutcome::Ambiguous
                    } else {
                        ExecutionOutcome::Cancelled
                    },
                    Some("interrupted"),
                    None,
                )?;
            }
            return Ok(());
        }
        // The marker is durable before resume/preparation or provider interrupt
        // can yield. The prompt boundary checks it again after preparation.
        let cancelled = MessageSubmissionRepository::cancel_before_dispatch_for_submission(
            &mut conn,
            &session_id,
            &execution.submission_id,
        )?;
        let stopped_before_dispatch = !cancelled.is_empty();
        for item in cancelled {
            self.publish_timeline_item(item)?;
        }
        if stopped_before_dispatch {
            self.settle_execution_by_id(
                execution_id,
                ExecutionOutcome::Cancelled,
                Some("interrupted"),
                None,
            )?;
            self.execution_progress.notify_waiters();
            return Ok(());
        }
        if coordinator.is_some() {
            self.start_execution_observer(execution_id)?;
        }
        self.interrupt_captured_execution(&session_id, &execution)
            .await
    }
}

use super::*;

const DEADLINE_CANCELLATION_CONCURRENCY: usize = 4;
const DEADLINE_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

impl AgentManager {
    pub fn observability(&self) -> Arc<RuntimeObservability> {
        self.observability.clone()
    }

    /// Runs from the runtime's maintenance task. Every expired subtree is
    /// fenced before an interrupt can wait on a provider or lifecycle lock.
    /// Failed or timed-out interruptions remain cancelling for the next pass.
    pub async fn enforce_delegation_deadlines(self: &Arc<Self>, now_ms: i64) -> VibexResult<usize> {
        let tasks = {
            let mut conn = self.open_migrated()?;
            let mut tasks = Vec::new();
            let mut seen = HashSet::new();
            for expired in vibex_db::VibexUseBudgetRepository::expired_tasks(&conn, now_ms)? {
                if expired.cancellation_requested_at_ms.is_none() {
                    self.observability.increment(
                        RuntimeMetricName::DelegationDeadline,
                        None,
                        RuntimeMetricResult::TimedOut,
                    );
                }
                for task in
                    vibex_db::request_delegation_tree_cancellation(&mut conn, &expired.id, true)?
                {
                    if seen.insert(task.id.clone()) {
                        tasks.push(task);
                    }
                }
            }
            for task in vibex_db::VibexUseCancellationRepository::pending_tasks(&conn)? {
                if seen.insert(task.id.clone()) {
                    tasks.push(task);
                }
            }
            tasks
        };
        self.execution_progress.notify_waiters();
        let count = tasks.len();
        let mut pending = tasks.into_iter();
        let mut attempts = tokio::task::JoinSet::new();
        loop {
            while attempts.len() < DEADLINE_CANCELLATION_CONCURRENCY {
                let Some(task) = pending.next() else {
                    break;
                };
                let manager = self.clone();
                attempts.spawn(async move {
                    let result = tokio::time::timeout(
                        DEADLINE_CANCELLATION_TIMEOUT,
                        manager.cancel_agent_delegation(CancelAgentDelegationRequest {
                            parent_session_id: task.parent_session_id,
                            delegation_id: task.id,
                        }),
                    )
                    .await;
                    if result.is_err() {
                        manager.observability.increment(
                            RuntimeMetricName::DelegationCancellation,
                            None,
                            RuntimeMetricResult::TimedOut,
                        );
                    }
                });
            }
            if attempts.join_next().await.is_none() {
                break;
            }
        }
        Ok(count)
    }
}

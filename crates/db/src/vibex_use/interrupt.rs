use super::*;

/// A round stop is independent from cancellation of the task that owns it.
pub struct VibexUseInterruptRepository;

impl VibexUseInterruptRepository {
    /// Prefers an admitted round, otherwise the first queued input. Callers
    /// selecting a mutation target hold the session dispatch pause.
    pub fn current_execution(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<DelegationExecution>> {
        let id: Option<String> = conn
            .query_row(
                "SELECT execution.execution_id FROM vibex_use_executions execution
                 JOIN agent_message_submissions submission USING(submission_id)
                 JOIN agent_message_submission_payloads payload USING(submission_id)
                 WHERE execution.session_id = ?1 AND execution.finished_at_ms IS NULL
                   AND execution.outcome IN ('queued', 'running')
                   AND submission.status IN
                       ('about_to_prompt', 'dispatched', 'awaiting_runtime', 'ready_to_dispatch')
                 ORDER BY CASE WHEN submission.status IN ('about_to_prompt', 'dispatched')
                               THEN 0 ELSE 1 END,
                          payload.submission_sequence ASC LIMIT 1",
                params![session_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err(
                "vibex_use_execution_lookup_failed",
                "failed to read the current execution",
            ))?;
        id.map(|id| VibexUseExecutionRepository::get(conn, &VibexExecutionId::parse(id)?))
            .transpose()
            .map(Option::flatten)
    }

    /// Records intent only while this exact round is queued or admitted. The
    /// submission condition prevents a delayed observer from making an already
    /// completed prompt look interruptible. Repeating an accepted stop retains
    /// its original timestamp.
    pub fn request(conn: &Connection, execution_id: &VibexExecutionId) -> VibexResult<bool> {
        conn.execute(
            "UPDATE vibex_use_executions
             SET interrupt_requested_at_ms = COALESCE(interrupt_requested_at_ms, ?2),
                 updated_at_ms = CASE WHEN interrupt_requested_at_ms IS NULL
                                      THEN ?2 ELSE updated_at_ms END
             WHERE execution_id = ?1 AND finished_at_ms IS NULL
               AND outcome IN ('queued', 'running')
               AND EXISTS (SELECT 1 FROM agent_message_submissions submission
                   WHERE submission.submission_id = vibex_use_executions.submission_id
                     AND submission.status IN
                         ('awaiting_runtime', 'ready_to_dispatch', 'about_to_prompt', 'dispatched'))",
            params![execution_id.as_str(), unix_timestamp_ms()],
        )
        .map(|changed| changed == 1)
        .map_err(storage_err(
            "vibex_use_interrupt_write_failed",
            "failed to record execution interruption",
        ))
    }

    pub fn is_requested(conn: &Connection, execution_id: &VibexExecutionId) -> VibexResult<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vibex_use_executions
             WHERE execution_id = ?1 AND interrupt_requested_at_ms IS NOT NULL)",
            params![execution_id.as_str()],
            |row| row.get(0),
        )
        .map_err(storage_err(
            "vibex_use_interrupt_read_failed",
            "failed to read execution interruption",
        ))
    }
}

use super::*;

/// Captures the task that caused new work while keeping navigation ownership
/// independent. A controlled worker carries its admitted execution's origin.
pub fn vibex_use_origin_task(
    conn: &Connection,
    session_id: &VibexSessionId,
) -> VibexResult<Option<AgentDelegationId>> {
    let mut current = session_id.clone();
    let mut seen = std::collections::BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) || seen.len() > 32 {
            return Err(VibexError::storage(
                "vibex_use_ownership_cycle",
                "invalid work ancestry",
            ));
        }
        if let Some(task) = list_delegations_for_child(conn, &current)?
            .into_iter()
            .rfind(|task| !task.phase().is_terminal())
        {
            return Ok(Some(task.id));
        }
        let active: Option<Option<String>> = conn
            .query_row(
                "SELECT e.origin_task_id FROM vibex_use_executions e
             JOIN agent_message_submissions s ON s.submission_id = e.submission_id
             WHERE e.session_id = ?1 AND e.outcome = 'running'
               AND s.status IN ('about_to_prompt', 'dispatched')
             ORDER BY e.created_at_ms DESC, e.rowid DESC LIMIT 1",
                params![current.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err(
                "vibex_use_origin_read_failed",
                "failed to read work origin",
            ))?;
        if let Some(origin) = active {
            return origin.map(AgentDelegationId::parse).transpose();
        }
        let edge: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT parent_session_id, created_by_task_id FROM session_ownership_edges
             WHERE child_session_id = ?1",
                params![current.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage_err(
                "vibex_use_origin_read_failed",
                "failed to read work origin",
            ))?;
        let Some((parent, origin)) = edge else {
            return Ok(None);
        };
        if let Some(origin) = origin {
            let id = AgentDelegationId::parse(origin)?;
            if crate::AgentDelegationRepository::get(conn, &id)?
                .is_some_and(|task| !task.phase().is_terminal())
            {
                return Ok(Some(id));
            }
        }
        current = VibexSessionId::parse(parent)?;
    }
}

pub struct VibexUseCancellationRepository;

impl VibexUseCancellationRepository {
    pub(super) fn origin_ancestry(
        conn: &Connection,
        session: &VibexSessionId,
    ) -> VibexResult<Vec<AgentDelegation>> {
        let mut current = vibex_use_origin_task(conn, session)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut tasks = Vec::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) || seen.len() > 32 {
                return Err(VibexError::storage(
                    "vibex_use_ownership_cycle",
                    "invalid task ancestry",
                ));
            }
            if let Some(task) = crate::AgentDelegationRepository::get(conn, &id)? {
                tasks.push(task);
            }
            let parent: Option<String> = conn
                .query_row(
                    "SELECT parent_task_id FROM agent_delegations WHERE delegation_id = ?1",
                    params![id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage_err(
                    "vibex_use_origin_read_failed",
                    "failed to read task ancestry",
                ))?
                .flatten();
            current = parent.map(AgentDelegationId::parse).transpose()?;
        }
        Ok(tasks)
    }

    /// Causal task ancestry never includes a controlled session's unrelated,
    /// pre-existing children. UNION also terminates malformed historical cycles.
    pub(super) fn task_subtree(
        conn: &Connection,
        task_id: &AgentDelegationId,
        cascade: bool,
    ) -> VibexResult<Vec<AgentDelegation>> {
        let mut statement = conn
            .prepare(
                "WITH RECURSIVE subtree(task_id) AS (
                SELECT delegation_id FROM agent_delegations WHERE delegation_id = ?1
                UNION SELECT d.delegation_id FROM agent_delegations d
                    JOIN subtree s ON d.parent_task_id = s.task_id WHERE ?2 = 1
             ) SELECT task_id FROM subtree",
            )
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read task subtree",
            ))?;
        let rows = statement
            .query_map(params![task_id.as_str(), i64::from(cascade)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read task subtree",
            ))?;
        let ids = collect_rows(
            rows,
            "vibex_use_cancellation_read_failed",
            "failed to decode task subtree",
        )?;
        ids.into_iter()
            .map(|id| crate::AgentDelegationRepository::get(conn, &AgentDelegationId::parse(id)?))
            .collect::<VibexResult<Vec<_>>>()
            .map(|tasks| tasks.into_iter().flatten().collect())
    }

    pub(super) fn capture(
        conn: &Connection,
        owner: &AgentDelegationId,
        source: &AgentDelegationId,
        include_origin_work: bool,
    ) -> VibexResult<()> {
        conn.execute(
            "INSERT OR IGNORE INTO vibex_use_cancellation_executions(task_id, execution_id, requested_at_ms)
             SELECT ?1, execution_id, ?4 FROM vibex_use_executions
             WHERE outcome IN ('queued', 'running', 'ambiguous')
                AND (task_id = ?2 OR (?3 = 1 AND origin_task_id = ?2))",
            params![owner.as_str(), source.as_str(), i64::from(include_origin_work), unix_timestamp_ms()],
        ).map_err(storage_err("vibex_use_cancellation_write_failed", "failed to capture cancelled executions"))?;
        Ok(())
    }

    pub fn executions_for_task(
        conn: &Connection,
        task_id: &AgentDelegationId,
    ) -> VibexResult<Vec<DelegationExecution>> {
        let mut statement = conn
            .prepare(&execution_select_sql(
                "WHERE task_id = ?1 OR execution_id IN
                (SELECT execution_id FROM vibex_use_cancellation_executions WHERE task_id = ?1)",
            ))
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read cancelled executions",
            ))?;
        let rows = statement
            .query_map(params![task_id.as_str()], map_execution)
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read cancelled executions",
            ))?;
        collect_rows(
            rows,
            "vibex_use_cancellation_read_failed",
            "failed to decode cancelled executions",
        )
    }

    pub fn is_requested(conn: &Connection, execution_id: &VibexExecutionId) -> VibexResult<bool> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vibex_use_cancellation_executions WHERE execution_id = ?1)",
            params![execution_id.as_str()], |row| row.get(0),
        ).map_err(storage_err("vibex_use_cancellation_read_failed", "failed to read execution cancellation"))
    }

    pub fn is_confirmed(conn: &Connection, task_id: &AgentDelegationId) -> VibexResult<bool> {
        Ok(Self::executions_for_task(conn, task_id)?
            .iter()
            .all(|execution| {
                execution.is_settled() && execution.outcome != ExecutionOutcome::Ambiguous
            }))
    }

    /// Maintenance retries manual and deadline cancellations after restarts.
    /// Terminal task rows can still own an unfinished cascade execution.
    pub fn pending_tasks(conn: &Connection) -> VibexResult<Vec<AgentDelegation>> {
        let mut statement = conn
            .prepare(
                "SELECT d.delegation_id FROM agent_delegations d
             WHERE d.task_phase = 'cancelling' OR EXISTS (
                SELECT 1 FROM vibex_use_cancellation_executions c
                JOIN vibex_use_executions e ON e.execution_id = c.execution_id
                WHERE c.task_id = d.delegation_id AND e.outcome IN ('queued', 'running'))
             ORDER BY COALESCE(d.cancellation_last_attempt_at_ms, 0), d.updated_at_ms, d.delegation_id LIMIT 64",
            )
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read pending cancellations",
            ))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read pending cancellations",
            ))?;
        let ids = collect_rows(
            rows,
            "vibex_use_cancellation_read_failed",
            "failed to decode pending cancellations",
        )?;
        // Rotate the bounded batch even when an ambiguous execution cannot be
        // resolved, or an interrupt later times out. Do not alter task revisions
        // or user-visible update timestamps merely because maintenance ran.
        for id in &ids {
            conn.execute(
                "UPDATE agent_delegations SET cancellation_last_attempt_at_ms = ?2 WHERE delegation_id = ?1",
                params![id, unix_timestamp_ms()],
            ).map_err(storage_err("vibex_use_cancellation_write_failed", "failed to record cancellation attempt"))?;
        }
        ids.into_iter()
            .map(|id| crate::AgentDelegationRepository::get(conn, &AgentDelegationId::parse(id)?))
            .collect::<VibexResult<Vec<_>>>()
            .map(|tasks| tasks.into_iter().flatten().collect())
    }

    pub fn settle_task(conn: &Connection, task_id: &AgentDelegationId) -> VibexResult<()> {
        let Some(task) = crate::AgentDelegationRepository::get(conn, task_id)? else {
            return Ok(());
        };
        if task.phase() != DelegationTaskPhase::Cancelling || !Self::is_confirmed(conn, task_id)? {
            return Ok(());
        }
        if let Some(cancelled) = transition_delegation(
            conn,
            task_id,
            DelegationTaskPhase::Cancelled,
            Some("Task cancelled"),
            None,
        )? {
            if let Some(child) = cancelled.child_session_id.as_ref() {
                SessionControllerRepository::release(conn, child, task_id)?;
            }
            VibexUseEventRepository::append(
                conn,
                &DelegationTaskEventKind::TaskCancelled.stable_id(task_id, None),
                cancelled
                    .root_session_id
                    .as_ref()
                    .or(Some(&cancelled.parent_session_id)),
                DelegationTaskEventKind::TaskCancelled,
                Some(task_id),
                cancelled.child_session_id.as_ref(),
                cancelled.revision,
                &serde_json::json!({"phase": "cancelled"}),
            )?;
        }
        Ok(())
    }

    pub(super) fn settle_dependents(
        conn: &Connection,
        execution_id: &VibexExecutionId,
    ) -> VibexResult<()> {
        let mut statement = conn
            .prepare(
                "SELECT task_id FROM vibex_use_cancellation_executions WHERE execution_id = ?1",
            )
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read cancellation dependents",
            ))?;
        let rows = statement
            .query_map(params![execution_id.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err(
                "vibex_use_cancellation_read_failed",
                "failed to read cancellation dependents",
            ))?;
        let ids = collect_rows(
            rows,
            "vibex_use_cancellation_read_failed",
            "failed to decode cancellation dependents",
        )?;
        for id in ids {
            Self::settle_task(conn, &AgentDelegationId::parse(id)?)?;
        }
        Ok(())
    }
}

use super::*;
use vibex_core::{AgentId, VibexUseBudgetPolicy};

pub struct VibexUseBudgetRepository;

impl VibexUseBudgetRepository {
    pub fn policy(conn: &Connection) -> VibexResult<VibexUseBudgetPolicy> {
        let value: Option<String> = conn
            .query_row(
                "SELECT policy_json FROM vibex_use_policies WHERE policy_key = 'runtime'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err(
                "vibex_use_budget_read_failed",
                "failed to read team budget policy",
            ))?;
        let policy = value
            .map(crate::json_from_db)
            .transpose()?
            .unwrap_or_default();
        VibexUseBudgetPolicy::validate(&policy)?;
        Ok(policy)
    }

    pub fn set_policy(conn: &Connection, policy: &VibexUseBudgetPolicy) -> VibexResult<()> {
        policy.validate()?;
        conn.execute(
            "INSERT INTO vibex_use_policies(policy_key, policy_json, updated_at_ms)
            VALUES ('runtime', ?1, ?2) ON CONFLICT(policy_key) DO UPDATE
            SET policy_json = excluded.policy_json, updated_at_ms = excluded.updated_at_ms",
            params![json_to_db_value(policy)?, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "vibex_use_budget_write_failed",
            "failed to store team budget policy",
        ))?;
        Ok(())
    }

    pub fn reserve_task(conn: &Connection, task: &AgentDelegation) -> VibexResult<()> {
        let policy = Self::policy(conn)?;
        let deadline = task
            .created_at_ms
            .saturating_add(policy.task_timeout_ms as i64);
        conn.execute(
            "INSERT OR IGNORE INTO vibex_use_task_limits(task_id, deadline_at_ms)
            VALUES (?1, ?2)",
            params![task.id.as_str(), deadline],
        )
        .map_err(storage_err(
            "vibex_use_budget_write_failed",
            "failed to store task deadline",
        ))?;
        Ok(())
    }

    pub fn task_deadline(
        conn: &Connection,
        task_id: &AgentDelegationId,
    ) -> VibexResult<Option<i64>> {
        conn.query_row(
            "SELECT deadline_at_ms FROM vibex_use_task_limits WHERE task_id = ?1",
            params![task_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_budget_read_failed",
            "failed to read task deadline",
        ))
    }

    pub fn check_task_deadline(
        conn: &Connection,
        task_id: &AgentDelegationId,
        now_ms: i64,
    ) -> VibexResult<()> {
        if Self::task_deadline(conn, task_id)?.is_some_and(|deadline| deadline <= now_ms) {
            return Err(VibexError::conflict(
                "vibex_use_task_deadline_exceeded",
                "the task has reached its time limit",
            ));
        }
        Ok(())
    }

    /// Checks the current task at each ownership edge, so an expired parent
    /// cannot create another child while cancellation is being scheduled.
    pub fn check_session_deadline(
        conn: &Connection,
        session_id: &VibexSessionId,
        now_ms: i64,
    ) -> VibexResult<()> {
        for task in VibexUseCancellationRepository::origin_ancestry(conn, session_id)? {
            Self::check_task_deadline(conn, &task.id, now_ms)?;
        }
        let mut current = session_id.clone();
        let mut seen = std::collections::BTreeSet::new();
        loop {
            if !seen.insert(current.clone()) || seen.len() > 32 {
                return Err(VibexError::storage(
                    "vibex_use_ownership_cycle",
                    "invalid session ancestry",
                ));
            }
            if let Some(task) = list_delegations_for_child(conn, &current)?
                .into_iter()
                .rfind(|task| !task.phase().is_terminal())
            {
                Self::check_task_deadline(conn, &task.id, now_ms)?;
            }
            let Some(parent) = SessionOwnershipRepository::parent_of(conn, &current)? else {
                return Ok(());
            };
            current = parent;
        }
    }

    pub fn expired_tasks(conn: &Connection, now_ms: i64) -> VibexResult<Vec<AgentDelegation>> {
        let mut statement = conn
            .prepare(
                "SELECT d.delegation_id FROM agent_delegations d
            JOIN vibex_use_task_limits l ON l.task_id = d.delegation_id
            WHERE l.deadline_at_ms <= ?1 AND d.cancellation_requested_at_ms IS NULL
              AND d.task_phase IN ('queued', 'starting', 'active', 'awaiting_review')
            ORDER BY l.deadline_at_ms, d.delegation_id LIMIT 64",
            )
            .map_err(storage_err(
                "vibex_use_budget_read_failed",
                "failed to read expired tasks",
            ))?;
        let ids = statement
            .query_map(params![now_ms], |row| row.get::<_, String>(0))
            .map_err(storage_err(
                "vibex_use_budget_read_failed",
                "failed to read expired tasks",
            ))?;
        let ids = collect_rows(
            ids,
            "vibex_use_budget_read_failed",
            "failed to decode expired task ids",
        )?;
        ids.into_iter()
            .map(|id| crate::AgentDelegationRepository::get(conn, &AgentDelegationId::parse(id)?))
            .collect::<VibexResult<Vec<_>>>()
            .map(|tasks| tasks.into_iter().flatten().collect())
    }

    pub fn active_for_agent(conn: &Connection, agent_id: &AgentId) -> VibexResult<u32> {
        let count: i64 = conn.query_row("SELECT COUNT(*) + (
                SELECT COUNT(*) FROM agent_delegations d
                WHERE d.current_execution_id IS NULL AND d.task_phase IN ('starting', 'active')
                  AND COALESCE(json_extract(d.requested_runtime_json, '$.agentId'),
                    d.requested_agent_id, d.effective_agent_id,
                    (SELECT current_agent_id FROM agent_sessions WHERE session_id = d.parent_session_id)) = ?1)
            FROM vibex_use_executions e JOIN agent_message_submissions s ON s.submission_id = e.submission_id
            WHERE e.outcome IN ('queued', 'running')
              AND json_extract(s.desired_runtime_selection_json, '$.agentId') = ?1",
            params![agent_id.as_str()], |row| row.get(0))
            .map_err(storage_err("vibex_use_budget_read_failed", "failed to count Agent executions"))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Reported deltas form a lower bound when some turns provide no usage.
    /// No bytes-to-tokens estimate or pricing assumption enters admission.
    pub fn reported_tokens(conn: &Connection, root: &VibexSessionId) -> VibexResult<Option<u64>> {
        let mut statement = conn
            .prepare(
                "SELECT f.total_delta, f.input_delta, f.output_delta
            FROM agent_turn_usage_facts f JOIN vibex_use_executions e
                ON e.submission_id = f.message_submission_id
            WHERE e.root_session_id = ?1",
            )
            .map_err(storage_err(
                "vibex_use_budget_read_failed",
                "failed to read reported team usage",
            ))?;
        let values = statement
            .query_map(params![root.as_str()], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(storage_err(
                "vibex_use_budget_read_failed",
                "failed to read reported team usage",
            ))?;
        let mut total: Option<u64> = None;
        for value in values {
            let (reported, input, output) = value.map_err(storage_err(
                "vibex_use_budget_read_failed",
                "failed to decode team usage",
            ))?;
            let nonnegative = |value: i64| u64::try_from(value).unwrap_or_default();
            if let Some(value) = reported.map(nonnegative).or_else(|| {
                input
                    .zip(output)
                    .map(|(input, output)| nonnegative(input).saturating_add(nonnegative(output)))
            }) {
                total = Some(total.unwrap_or_default().saturating_add(value));
            }
        }
        Ok(total)
    }

    pub fn check_admission(
        conn: &Connection,
        root: &VibexSessionId,
        agent: Option<&AgentId>,
        reserved_slots: u32,
    ) -> VibexResult<()> {
        let policy = Self::policy(conn)?;
        if count_active_executions_for_root(conn, root)?.saturating_sub(reserved_slots)
            >= policy.root_execution_limit
        {
            return Err(VibexError::conflict(
                "vibex_use_root_budget_exceeded",
                "the team has reached its execution limit",
            ));
        }
        if let Some(agent) = agent
            && Self::active_for_agent(conn, agent)?.saturating_sub(reserved_slots)
                >= policy.per_agent_execution_limit
        {
            return Err(VibexError::conflict(
                "vibex_use_agent_budget_exceeded",
                "the Agent has reached its execution limit",
            ));
        }
        if let Some(limit) = policy.max_reported_tokens
            && Self::reported_tokens(conn, root)?.is_some_and(|total| total >= limit)
        {
            return Err(VibexError::conflict(
                "vibex_use_token_budget_exceeded",
                "the team's reported token usage reached its limit",
            ));
        }
        Ok(())
    }
}

pub(crate) fn migrate(conn: &mut Connection, applied: &mut Vec<String>) -> VibexResult<()> {
    const VERSION: i64 = 63;
    const NAME: &str = "vibex_use_budget_policy";
    if crate::migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start team budget migration",
    ))?;
    tx.execute_batch(
        "CREATE TABLE vibex_use_policies (
            policy_key TEXT PRIMARY KEY, policy_json TEXT NOT NULL, updated_at_ms INTEGER NOT NULL);
        CREATE TABLE vibex_use_task_limits (
            task_id TEXT PRIMARY KEY REFERENCES agent_delegations(delegation_id) ON DELETE CASCADE,
            deadline_at_ms INTEGER NOT NULL);
        CREATE INDEX idx_vibex_use_task_deadline ON vibex_use_task_limits(deadline_at_ms);
        ALTER TABLE vibex_use_executions ADD COLUMN root_session_id TEXT NULL;
        CREATE INDEX idx_vibex_use_execution_root ON vibex_use_executions(root_session_id, outcome);
        ALTER TABLE agent_delegations ADD COLUMN parent_task_id TEXT NULL
            REFERENCES agent_delegations(delegation_id) ON DELETE SET NULL;
        CREATE INDEX idx_vibex_use_task_parent ON agent_delegations(parent_task_id);
        ALTER TABLE agent_delegations ADD COLUMN cancellation_last_attempt_at_ms INTEGER NULL;
        ALTER TABLE vibex_use_executions ADD COLUMN origin_task_id TEXT NULL
            REFERENCES agent_delegations(delegation_id) ON DELETE SET NULL;
        ALTER TABLE vibex_use_executions ADD COLUMN interrupt_requested_at_ms INTEGER NULL;
        ALTER TABLE vibex_use_executions ADD COLUMN controller_revision INTEGER NOT NULL DEFAULT 1;
        UPDATE vibex_use_executions SET controller_revision = COALESCE(
            (SELECT revision FROM vibex_use_session_controllers controller
             WHERE controller.session_id = vibex_use_executions.session_id), 1);
        CREATE INDEX idx_vibex_use_execution_origin ON vibex_use_executions(origin_task_id, outcome);
        CREATE TABLE vibex_use_cancellation_executions (
            task_id TEXT NOT NULL REFERENCES agent_delegations(delegation_id) ON DELETE CASCADE,
            execution_id TEXT NOT NULL REFERENCES vibex_use_executions(execution_id) ON DELETE CASCADE,
            requested_at_ms INTEGER NOT NULL,
            PRIMARY KEY(task_id, execution_id));
        CREATE INDEX idx_vibex_use_execution_cancellation ON vibex_use_cancellation_executions(execution_id);
        ALTER TABLE session_ownership_edges ADD COLUMN root_session_id TEXT NULL;
        UPDATE session_ownership_edges SET root_session_id = (
            SELECT COALESCE(d.root_session_id, d.parent_session_id) FROM agent_delegations d
            WHERE d.delegation_id = session_ownership_edges.created_by_task_id);
        UPDATE vibex_use_executions SET root_session_id = (
            SELECT COALESCE(d.root_session_id, d.parent_session_id) FROM agent_delegations d
            WHERE d.delegation_id = vibex_use_executions.task_id);
        UPDATE agent_delegations AS child SET parent_task_id = (
            SELECT parent.delegation_id FROM agent_delegations parent
            WHERE parent.child_session_id = child.parent_session_id
                AND parent.delegation_id <> child.delegation_id
                AND parent.created_at_ms <= child.created_at_ms
                AND (parent.finished_at_ms IS NULL OR parent.finished_at_ms >= child.created_at_ms)
                AND COALESCE(parent.root_session_id, parent.parent_session_id)
                    = COALESCE(child.root_session_id, child.parent_session_id)
            ORDER BY parent.created_at_ms DESC, parent.rowid DESC LIMIT 1);
        UPDATE vibex_use_executions SET origin_task_id = task_id WHERE task_id IS NOT NULL;",
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to create team budget storage",
    ))?;
    // Older taskless executions have no explicit root; their creation edges are
    // the only surviving origin evidence. Capture it once during migration.
    let mut statement = tx.prepare("SELECT execution_id, session_id FROM vibex_use_executions WHERE root_session_id IS NULL")
        .map_err(storage_err("migration_apply_failed", "failed to read execution roots"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to read execution roots",
        ))?;
    let rows = collect_rows(
        rows,
        "migration_apply_failed",
        "failed to decode execution roots",
    )?;
    drop(statement);
    for (id, session) in rows {
        let root = ownership_origin_root(&tx, &VibexSessionId::parse(session)?)?;
        tx.execute(
            "UPDATE vibex_use_executions SET root_session_id = ?2 WHERE execution_id = ?1",
            params![id, root.as_str()],
        )
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to capture execution root",
        ))?;
    }
    let mut statement = tx
        .prepare(
            "SELECT child_session_id FROM session_ownership_edges WHERE root_session_id IS NULL",
        )
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to read session origins",
        ))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to read session origins",
        ))?;
    let rows = collect_rows(
        rows,
        "migration_apply_failed",
        "failed to decode session origins",
    )?;
    drop(statement);
    for session in rows {
        let root = ownership_origin_root(&tx, &VibexSessionId::parse(&session)?)?;
        tx.execute(
            "UPDATE session_ownership_edges SET root_session_id = ?2 WHERE child_session_id = ?1",
            params![session, root.as_str()],
        )
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to capture session origins",
        ))?;
    }
    // Existing tasks deliberately receive no deadline during an upgrade.
    tx.execute(
        "INSERT INTO schema_migrations(version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record team budget migration",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit team budget migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Migration must use creation evidence, never a temporary current controller
/// that could incorrectly attribute an older session to a different team.
fn ownership_origin_root(
    conn: &Connection,
    session: &VibexSessionId,
) -> VibexResult<VibexSessionId> {
    let mut current = session.clone();
    let mut seen = std::collections::BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) || seen.len() > 32 {
            return Err(VibexError::storage(
                "vibex_use_ownership_cycle",
                "invalid session ancestry",
            ));
        }
        let edge: Option<(String, Option<String>)> = conn.query_row(
            "SELECT parent_session_id, root_session_id FROM session_ownership_edges WHERE child_session_id = ?1",
            params![current.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(storage_err("migration_apply_failed", "failed to read session origins"))?;
        match edge {
            Some((_, Some(root))) => return VibexSessionId::parse(root),
            Some((parent, None)) => current = VibexSessionId::parse(parent)?,
            None => return Ok(current),
        }
    }
}

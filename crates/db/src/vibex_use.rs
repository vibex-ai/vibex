//! Durable records behind Vibex-use.
//!
//! The repositories here only own transactional primitives. Deciding whether a
//! task may start, whether a cancel fence wins a race, or which group layout a
//! client should apply all belongs to the service that composes them.
//!
//! Two invariants are enforced at this layer because they cannot be repaired
//! later: a task phase and its compatibility status are written by one
//! statement, and an idempotency key that was already used with a different
//! payload is reported as a conflict rather than silently reused.

use rusqlite::{Connection, OptionalExtension, params};
use vibex_core::{
    AgentDelegation, AgentDelegationId, DelegationBlockedOn, DelegationContextRef,
    DelegationExecution, DelegationOwnershipKind, DelegationResultRef, DelegationRuntimeSummary,
    DelegationTaskEvent, DelegationTaskEventKind, DelegationTaskPhase, ExecutionOutcome,
    ExecutionResultRange, ExecutionUsageState, MessageProvenance, MessageSubmissionId,
    SessionController, SessionGroupLayoutIntent, SessionGroupScope, VibexError, VibexExecutionId,
    VibexOperationId, VibexResult, VibexSessionId, VibexUseOperation, VibexUseOperationResource,
    VibexUseOperationState, VibexUseRef, legacy_status_for_phase, unix_timestamp_ms,
};

use crate::{
    collect_rows, enum_from_db_sql, enum_to_db, json_from_db_sql, optional_json_from_db_sql,
    parse_id_sql, parse_optional_id_sql, storage_err, u64_from_sql,
};

// ---------------------------------------------------------------------------
// Executions
// ---------------------------------------------------------------------------

pub struct VibexUseExecutionRepository;

impl VibexUseExecutionRepository {
    /// Inserts one execution or returns the row that already owns the same
    /// `(session, input idempotency key)` pair.
    ///
    /// A retried send must never create a second execution for one message, so
    /// the unique index does the deduplication and the caller gets back
    /// whichever row is authoritative.
    pub fn insert_or_get(
        conn: &Connection,
        execution: &DelegationExecution,
    ) -> VibexResult<(DelegationExecution, bool)> {
        // `INSERT OR IGNORE` reports zero changed rows when the input key was
        // already claimed, which is what distinguishes a first execution from a
        // retry of the same one.
        let inserted = conn
            .execute(
                "
            INSERT OR IGNORE INTO vibex_use_executions (
                execution_id, task_id, session_id, submission_id, input_idempotency_key,
                provenance_kind, provenance_json, start_sequence, end_sequence,
                runtime_selection_revision, outcome, stop_reason, summary,
                result_ranges_json, artifact_refs_json, usage_state, truncated,
                blocked_on_json, created_at_ms, updated_at_ms, finished_at_ms, error_code
            )
            VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21, ?22
            )
            ",
                params![
                    execution.id.as_str(),
                    execution
                        .task_ref
                        .as_ref()
                        .and_then(VibexUseRef::task_id)
                        .map(|id| id.into_string()),
                    execution
                        .session_ref
                        .session_id()
                        .map(|id| id.into_string()),
                    execution.submission_id.as_str(),
                    execution.input_idempotency_key,
                    execution.provenance.kind(),
                    json_to_db_value(&execution.provenance)?,
                    execution.start_sequence,
                    execution.end_sequence,
                    i64::try_from(execution.runtime_selection_revision).unwrap_or(i64::MAX),
                    enum_to_db(&execution.outcome)?,
                    execution.stop_reason,
                    execution.summary,
                    json_to_db_value(&execution.result_ranges)?,
                    json_to_db_value(&execution.artifact_refs)?,
                    enum_to_db(&execution.usage)?,
                    i64::from(execution.truncated),
                    execution
                        .blocked_on
                        .as_ref()
                        .map(json_to_db_value)
                        .transpose()?,
                    execution.created_at_ms,
                    execution.updated_at_ms,
                    execution.finished_at_ms,
                    execution.error_code,
                ],
            )
            .map_err(storage_err(
                "vibex_use_execution_insert_failed",
                "failed to persist a task execution",
            ))?;
        let stored = Self::get(conn, &execution.id)?.or(Self::get_by_input(
            conn,
            execution.session_ref.session_id().as_ref(),
            &execution.input_idempotency_key,
        )?);
        let stored = stored.ok_or_else(|| {
            VibexError::storage(
                "vibex_use_execution_missing_after_insert",
                "a task execution disappeared after it was persisted",
            )
        })?;
        Ok((stored, inserted > 0))
    }

    pub fn get(
        conn: &Connection,
        execution_id: &VibexExecutionId,
    ) -> VibexResult<Option<DelegationExecution>> {
        conn.query_row(
            &execution_select_sql("WHERE execution_id = ?1"),
            params![execution_id.as_str()],
            map_execution,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_execution_lookup_failed",
            "failed to read a task execution",
        ))
    }

    pub fn get_by_submission(
        conn: &Connection,
        submission_id: &MessageSubmissionId,
    ) -> VibexResult<Option<DelegationExecution>> {
        conn.query_row(
            &execution_select_sql("WHERE submission_id = ?1"),
            params![submission_id.as_str()],
            map_execution,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_execution_lookup_failed",
            "failed to read a task execution",
        ))
    }

    pub fn get_by_input(
        conn: &Connection,
        session_id: Option<&VibexSessionId>,
        input_idempotency_key: &str,
    ) -> VibexResult<Option<DelegationExecution>> {
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        conn.query_row(
            &execution_select_sql("WHERE session_id = ?1 AND input_idempotency_key = ?2"),
            params![session_id.as_str(), input_idempotency_key],
            map_execution,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_execution_lookup_failed",
            "failed to read a task execution",
        ))
    }

    pub fn list_for_task(
        conn: &Connection,
        task_id: &AgentDelegationId,
    ) -> VibexResult<Vec<DelegationExecution>> {
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY created_at_ms ASC, execution_id ASC",
                execution_select_sql("WHERE task_id = ?1")
            ))
            .map_err(storage_err(
                "vibex_use_execution_list_failed",
                "failed to prepare an execution list",
            ))?;
        let rows = statement
            .query_map(params![task_id.as_str()], map_execution)
            .map_err(storage_err(
                "vibex_use_execution_list_failed",
                "failed to list task executions",
            ))?;
        collect_rows(
            rows,
            "vibex_use_execution_decode_failed",
            "failed to decode a task execution",
        )
    }

    /// Executions that have not settled yet. Startup recovery walks these.
    pub fn list_open(conn: &Connection) -> VibexResult<Vec<DelegationExecution>> {
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY updated_at_ms ASC, execution_id ASC",
                execution_select_sql("WHERE outcome IN (?1, ?2)")
            ))
            .map_err(storage_err(
                "vibex_use_execution_list_failed",
                "failed to prepare the open execution list",
            ))?;
        let rows = statement
            .query_map(
                params![
                    enum_to_db(&ExecutionOutcome::Queued)?,
                    enum_to_db(&ExecutionOutcome::Running)?,
                ],
                map_execution,
            )
            .map_err(storage_err(
                "vibex_use_execution_list_failed",
                "failed to list open task executions",
            ))?;
        collect_rows(
            rows,
            "vibex_use_execution_decode_failed",
            "failed to decode a task execution",
        )
    }

    pub fn mark_started(
        conn: &Connection,
        execution_id: &VibexExecutionId,
        start_sequence: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE vibex_use_executions
            SET outcome = ?2,
                start_sequence = COALESCE(start_sequence, ?3),
                updated_at_ms = ?4
            WHERE execution_id = ?1
            ",
            params![
                execution_id.as_str(),
                enum_to_db(&ExecutionOutcome::Running)?,
                start_sequence,
                unix_timestamp_ms(),
            ],
        )
        .map_err(storage_err(
            "vibex_use_execution_update_failed",
            "failed to mark a task execution as started",
        ))?;
        Ok(())
    }

    /// Fixes the result window of one execution.
    ///
    /// The range is written once, when the execution settles. A later turn in
    /// the same session must never move an earlier result window, which is why
    /// this is a plain write at settlement time rather than a "latest reply"
    /// lookup at read time.
    #[allow(clippy::too_many_arguments)]
    pub fn settle(
        conn: &Connection,
        execution_id: &VibexExecutionId,
        outcome: ExecutionOutcome,
        stop_reason: Option<&str>,
        summary: Option<&str>,
        result_ranges: &[ExecutionResultRange],
        artifact_refs: &[VibexUseRef],
        usage: ExecutionUsageState,
        truncated: bool,
        end_sequence: Option<i64>,
        finished_at_ms: i64,
    ) -> VibexResult<Option<DelegationExecution>> {
        let changed = conn
            .execute(
                "
                UPDATE vibex_use_executions
                SET outcome = ?2,
                    stop_reason = ?3,
                    error_code = CASE
                        WHEN ?2 = 'failed' THEN COALESCE(error_code, ?3)
                        ELSE error_code
                    END,
                    summary = ?4,
                    result_ranges_json = ?5,
                    artifact_refs_json = ?6,
                    usage_state = ?7,
                    truncated = ?8,
                    end_sequence = COALESCE(?9, end_sequence),
                    finished_at_ms = COALESCE(finished_at_ms, ?10),
                    updated_at_ms = ?10
                WHERE execution_id = ?1
                  AND outcome IN (?11, ?12)
                ",
                params![
                    execution_id.as_str(),
                    enum_to_db(&outcome)?,
                    stop_reason,
                    summary,
                    json_to_db_value(result_ranges)?,
                    json_to_db_value(artifact_refs)?,
                    enum_to_db(&usage)?,
                    i64::from(truncated),
                    end_sequence,
                    finished_at_ms,
                    enum_to_db(&ExecutionOutcome::Queued)?,
                    enum_to_db(&ExecutionOutcome::Running)?,
                ],
            )
            .map_err(storage_err(
                "vibex_use_execution_update_failed",
                "failed to settle a task execution",
            ))?;
        if changed == 0 {
            // Either the row is already settled (idempotent) or it vanished.
            return Self::get(conn, execution_id);
        }
        Self::get(conn, execution_id)
    }

    /// Records the stable failure code of an execution that already settled.
    pub fn set_error_code(
        conn: &Connection,
        execution_id: &VibexExecutionId,
        error_code: &str,
    ) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "
                UPDATE vibex_use_executions
                SET error_code = COALESCE(error_code, ?2), updated_at_ms = ?3
                WHERE execution_id = ?1
                ",
                params![execution_id.as_str(), error_code, unix_timestamp_ms()],
            )
            .map_err(storage_err(
                "vibex_use_execution_error_failed",
                "failed to record the failure code of a task execution",
            ))?;
        Ok(changed > 0)
    }

    pub fn set_blocked_on(
        conn: &Connection,
        execution_id: &VibexExecutionId,
        blocked_on: Option<&DelegationBlockedOn>,
    ) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE vibex_use_executions
            SET blocked_on_json = ?2, updated_at_ms = ?3
            WHERE execution_id = ?1
            ",
            params![
                execution_id.as_str(),
                blocked_on.map(json_to_db_value).transpose()?,
                unix_timestamp_ms(),
            ],
        )
        .map_err(storage_err(
            "vibex_use_execution_update_failed",
            "failed to record what a task execution is waiting for",
        ))?;
        Ok(())
    }
}

fn execution_select_sql(where_clause: &str) -> String {
    format!(
        "
        SELECT execution_id, task_id, session_id, submission_id, input_idempotency_key,
            provenance_kind, provenance_json, start_sequence, end_sequence,
            runtime_selection_revision, outcome, stop_reason, summary,
            result_ranges_json, artifact_refs_json, usage_state, truncated,
            blocked_on_json, created_at_ms, updated_at_ms, finished_at_ms, error_code
        FROM vibex_use_executions
        {where_clause}
        "
    )
}

fn map_execution(row: &rusqlite::Row<'_>) -> rusqlite::Result<DelegationExecution> {
    let execution_id = parse_id_sql(row.get(0)?, VibexExecutionId::parse)?;
    let session_id = parse_id_sql(row.get(2)?, VibexSessionId::parse)?;
    let task_id: Option<String> = row.get(1)?;
    let provenance_kind: String = row.get(5)?;
    let provenance: MessageProvenance = if provenance_kind == "legacy_unknown" {
        MessageProvenance::LegacyUnknown
    } else {
        json_from_db_sql(row.get(6)?)?
    };
    Ok(DelegationExecution {
        execution_ref: VibexUseRef::execution(&execution_id),
        id: execution_id,
        task_ref: task_id
            .map(|value| {
                parse_id_sql(value, AgentDelegationId::parse).map(|id| VibexUseRef::task(&id))
            })
            .transpose()?,
        session_ref: VibexUseRef::session(&session_id),
        submission_id: parse_id_sql(row.get(3)?, MessageSubmissionId::parse)?,
        input_idempotency_key: row.get(4)?,
        provenance,
        start_sequence: row.get(7)?,
        end_sequence: row.get(8)?,
        runtime_selection_revision: u64_from_sql(row.get(9)?)?,
        outcome: enum_from_db_sql(row.get(10)?)?,
        stop_reason: row.get(11)?,
        error_code: row.get(21)?,
        summary: row.get(12)?,
        result_ranges: json_from_db_sql(row.get(13)?)?,
        artifact_refs: json_from_db_sql(row.get(14)?)?,
        usage: enum_from_db_sql(row.get(15)?)?,
        truncated: row.get::<_, i64>(16)? != 0,
        blocked_on: optional_json_from_db_sql(row.get(17)?)?,
        created_at_ms: row.get(18)?,
        updated_at_ms: row.get(19)?,
        finished_at_ms: row.get(20)?,
    })
}

fn json_to_db_value<T: serde::Serialize + ?Sized>(value: &T) -> VibexResult<String> {
    crate::json_to_db(value)
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// Outcome of reserving one idempotency key.
#[derive(Debug, Clone)]
pub enum VibexUseOperationReservation {
    /// The key was free and now belongs to this operation.
    Claimed(VibexUseOperation),
    /// The key was already used with exactly this payload.
    Existing(VibexUseOperation),
    /// The key was already used with a different payload.
    Conflict(VibexUseOperation),
}

pub struct VibexUseOperationRepository;

impl VibexUseOperationRepository {
    /// Reserves `(authority, actor, tool, caller key)` for one request.
    ///
    /// The fingerprint covers everything that changes what the request does —
    /// target, context range, workspace and task text — while leaving out
    /// service-generated values such as timestamps, so an honest retry of the
    /// same intent resolves to the original operation.
    pub fn reserve(
        conn: &Connection,
        operation: &VibexUseOperation,
        caller_key: &str,
    ) -> VibexResult<VibexUseOperationReservation> {
        if let Some(existing) = Self::get_by_key(
            conn,
            &operation.authority_key(),
            &operation.tool,
            caller_key,
        )? {
            return Ok(
                if existing.payload_fingerprint == operation.payload_fingerprint {
                    VibexUseOperationReservation::Existing(existing)
                } else {
                    VibexUseOperationReservation::Conflict(existing)
                },
            );
        }
        let now = unix_timestamp_ms();
        let inserted = conn
            .execute(
                "
            INSERT INTO vibex_use_operations (
                operation_id, authority, actor_key, tool_kind, caller_key,
                payload_fingerprint, state, error_code, error_message, retryable,
                resources_json, checkpoint_json, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT (authority, actor_key, tool_kind, caller_key) DO NOTHING
            ",
                params![
                    operation.id.as_str(),
                    operation.authority,
                    operation.actor_key,
                    operation.tool,
                    caller_key,
                    operation.payload_fingerprint,
                    enum_to_db(&operation.state)?,
                    operation.error_code,
                    operation.error_message,
                    i64::from(operation.retryable),
                    json_to_db_value(&operation.resources)?,
                    json_to_db_value(&operation.checkpoint)?,
                    now,
                    now,
                ],
            )
            .map_err(storage_err(
                "vibex_use_operation_reserve_failed",
                "failed to reserve a Vibex-use operation",
            ))?;
        if inserted == 0 {
            // Another caller won the insert between the lookup above and here.
            // The unique key is the arbiter, and the loser answers with the
            // winner's operation instead of a storage error: a retried request
            // must get the original acceptance, not a failure it cannot act on.
            let existing = Self::get_by_key(
                conn,
                &operation.authority_key(),
                &operation.tool,
                caller_key,
            )?
            .ok_or_else(|| {
                VibexError::storage(
                    "vibex_use_operation_missing_after_conflict",
                    "a concurrent Vibex-use operation could not be read back",
                )
            })?;
            return Ok(
                if existing.payload_fingerprint == operation.payload_fingerprint {
                    VibexUseOperationReservation::Existing(existing)
                } else {
                    VibexUseOperationReservation::Conflict(existing)
                },
            );
        }
        Self::get(conn, &operation.id)?
            .map(VibexUseOperationReservation::Claimed)
            .ok_or_else(|| {
                VibexError::storage(
                    "vibex_use_operation_missing_after_insert",
                    "a Vibex-use operation disappeared after it was persisted",
                )
            })
    }

    pub fn get(
        conn: &Connection,
        operation_id: &VibexOperationId,
    ) -> VibexResult<Option<VibexUseOperation>> {
        conn.query_row(
            &operation_select_sql("WHERE operation_id = ?1"),
            params![operation_id.as_str()],
            map_operation,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_operation_lookup_failed",
            "failed to read a Vibex-use operation",
        ))
    }

    pub fn get_by_key(
        conn: &Connection,
        authority_key: &str,
        tool: &str,
        caller_key: &str,
    ) -> VibexResult<Option<VibexUseOperation>> {
        conn.query_row(
            &operation_select_sql(
                "WHERE authority || '\u{1f}' || actor_key = ?1 AND tool_kind = ?2 AND caller_key = ?3",
            ),
            params![authority_key, tool, caller_key],
            map_operation,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_operation_lookup_failed",
            "failed to read a Vibex-use operation by idempotency key",
        ))
    }

    pub fn list_for_actor(
        conn: &Connection,
        authority: &str,
        actor_key: &str,
        limit: usize,
    ) -> VibexResult<Vec<VibexUseOperation>> {
        let limit = i64::try_from(limit.min(500)).unwrap_or(500);
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY updated_at_ms DESC, operation_id ASC LIMIT ?3",
                operation_select_sql("WHERE authority = ?1 AND actor_key = ?2")
            ))
            .map_err(storage_err(
                "vibex_use_operation_list_failed",
                "failed to prepare a Vibex-use operation list",
            ))?;
        let rows = statement
            .query_map(params![authority, actor_key, limit], map_operation)
            .map_err(storage_err(
                "vibex_use_operation_list_failed",
                "failed to list Vibex-use operations",
            ))?;
        collect_rows(
            rows,
            "vibex_use_operation_decode_failed",
            "failed to decode a Vibex-use operation",
        )
    }

    pub fn update_state(
        conn: &Connection,
        operation_id: &VibexOperationId,
        state: VibexUseOperationState,
        error_code: Option<&str>,
        error_message: Option<&str>,
        retryable: bool,
    ) -> VibexResult<Option<VibexUseOperation>> {
        let changed = conn
            .execute(
                "
                UPDATE vibex_use_operations
                SET state = ?2,
                    error_code = ?3,
                    error_message = ?4,
                    retryable = ?5,
                    updated_at_ms = ?6
                WHERE operation_id = ?1
                ",
                params![
                    operation_id.as_str(),
                    enum_to_db(&state)?,
                    error_code,
                    error_message,
                    i64::from(retryable),
                    unix_timestamp_ms(),
                ],
            )
            .map_err(storage_err(
                "vibex_use_operation_update_failed",
                "failed to update a Vibex-use operation",
            ))?;
        if changed == 0 {
            return Ok(None);
        }
        Self::get(conn, operation_id)
    }

    /// Appends one produced resource. Re-appending the same reference is a
    /// no-op so a retried operation never lists the same session twice.
    pub fn append_resource(
        conn: &Connection,
        operation_id: &VibexOperationId,
        resource: &VibexUseOperationResource,
    ) -> VibexResult<Option<VibexUseOperation>> {
        let Some(mut operation) = Self::get(conn, operation_id)? else {
            return Ok(None);
        };
        if operation
            .resources
            .iter()
            .any(|existing| existing.reference == resource.reference)
        {
            return Ok(Some(operation));
        }
        operation.resources.push(resource.clone());
        conn.execute(
            "
            UPDATE vibex_use_operations
            SET resources_json = ?2, updated_at_ms = ?3
            WHERE operation_id = ?1
            ",
            params![
                operation_id.as_str(),
                json_to_db_value(&operation.resources)?,
                unix_timestamp_ms(),
            ],
        )
        .map_err(storage_err(
            "vibex_use_operation_update_failed",
            "failed to record a resource produced by a Vibex-use operation",
        ))?;
        Self::get(conn, operation_id)
    }

    pub fn set_checkpoint(
        conn: &Connection,
        operation_id: &VibexOperationId,
        key: &str,
        value: &str,
    ) -> VibexResult<Option<VibexUseOperation>> {
        let Some(mut operation) = Self::get(conn, operation_id)? else {
            return Ok(None);
        };
        operation
            .checkpoint
            .insert(key.to_string(), value.to_string());
        conn.execute(
            "
            UPDATE vibex_use_operations
            SET checkpoint_json = ?2, updated_at_ms = ?3
            WHERE operation_id = ?1
            ",
            params![
                operation_id.as_str(),
                json_to_db_value(&operation.checkpoint)?,
                unix_timestamp_ms(),
            ],
        )
        .map_err(storage_err(
            "vibex_use_operation_update_failed",
            "failed to checkpoint a Vibex-use operation",
        ))?;
        Self::get(conn, operation_id)
    }
}

fn operation_select_sql(where_clause: &str) -> String {
    format!(
        "
        SELECT operation_id, authority, actor_key, tool_kind, caller_key,
            payload_fingerprint, state, error_code, error_message, retryable,
            resources_json, checkpoint_json, created_at_ms, updated_at_ms
        FROM vibex_use_operations
        {where_clause}
        "
    )
}

fn map_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<VibexUseOperation> {
    let operation_id = parse_id_sql(row.get(0)?, VibexOperationId::parse)?;
    Ok(VibexUseOperation {
        operation_ref: VibexUseRef::operation(&operation_id),
        id: operation_id,
        authority: row.get(1)?,
        actor_key: row.get(2)?,
        tool: row.get(3)?,
        caller_key: row.get(4)?,
        payload_fingerprint: row.get(5)?,
        state: enum_from_db_sql(row.get(6)?)?,
        error_code: row.get(7)?,
        error_message: row.get(8)?,
        retryable: row.get::<_, i64>(9)? != 0,
        resources: json_from_db_sql(row.get(10)?)?,
        checkpoint: json_from_db_sql(row.get(11)?)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
    })
}

// ---------------------------------------------------------------------------
// Session ownership
// ---------------------------------------------------------------------------

pub struct SessionOwnershipRepository;

impl SessionOwnershipRepository {
    /// Records the unique parent of one delegated child session.
    ///
    /// A second, different parent is a conflict: the sidebar tree, the depth
    /// walk and the cascade delete would otherwise each answer differently.
    /// Re-recording the same parent is idempotent.
    pub fn upsert(
        conn: &Connection,
        child_session_id: &VibexSessionId,
        parent_session_id: &VibexSessionId,
        created_by_task_id: Option<&AgentDelegationId>,
    ) -> VibexResult<()> {
        let now = unix_timestamp_ms();
        if let Some(existing) = Self::parent_of(conn, child_session_id)?
            && existing != *parent_session_id
        {
            return Err(VibexError::conflict(
                "vibex_use_ownership_conflict",
                "a delegated child session already belongs to another parent session",
            )
            .with_diagnostic("childSessionId", child_session_id.as_str())
            .with_diagnostic("parentSessionId", existing.as_str()));
        }
        conn.execute(
            "
            INSERT INTO session_ownership_edges (
                child_session_id, parent_session_id, origin, created_by_task_id,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, 'agent_delegation', ?3, ?4, ?4)
            ON CONFLICT(child_session_id) DO UPDATE SET
                updated_at_ms = excluded.updated_at_ms,
                created_by_task_id = COALESCE(
                    session_ownership_edges.created_by_task_id,
                    excluded.created_by_task_id
                )
            ",
            params![
                child_session_id.as_str(),
                parent_session_id.as_str(),
                created_by_task_id.map(AgentDelegationId::as_str),
                now,
            ],
        )
        .map_err(storage_err(
            "vibex_use_ownership_write_failed",
            "failed to record delegated session ownership",
        ))?;
        Ok(())
    }

    pub fn parent_of(
        conn: &Connection,
        child_session_id: &VibexSessionId,
    ) -> VibexResult<Option<VibexSessionId>> {
        conn.query_row(
            "SELECT parent_session_id FROM session_ownership_edges WHERE child_session_id = ?1",
            params![child_session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_ownership_lookup_failed",
            "failed to read delegated session ownership",
        ))?
        .map(VibexSessionId::parse)
        .transpose()
    }

    pub fn child_ids(
        conn: &Connection,
        parent_session_id: &VibexSessionId,
    ) -> VibexResult<Vec<VibexSessionId>> {
        let mut statement = conn
            .prepare(
                "
                SELECT child_session_id
                FROM session_ownership_edges
                WHERE parent_session_id = ?1
                ORDER BY created_at_ms ASC, child_session_id ASC
                ",
            )
            .map_err(storage_err(
                "vibex_use_ownership_list_failed",
                "failed to prepare a delegated child list",
            ))?;
        let rows = statement
            .query_map(params![parent_session_id.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err(
                "vibex_use_ownership_list_failed",
                "failed to list delegated child sessions",
            ))?;
        let mut children = Vec::new();
        for row in rows {
            let value = row.map_err(storage_err(
                "vibex_use_ownership_decode_failed",
                "failed to decode a delegated child session",
            ))?;
            children.push(VibexSessionId::parse(value)?);
        }
        Ok(children)
    }

    pub fn child_count(
        conn: &Connection,
        parent_session_id: &VibexSessionId,
    ) -> VibexResult<usize> {
        let count = conn
            .query_row(
                "SELECT COUNT(*) FROM session_ownership_edges WHERE parent_session_id = ?1",
                params![parent_session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(storage_err(
                "vibex_use_ownership_count_failed",
                "failed to count delegated child sessions",
            ))?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    /// Depth of a session below its root. A root session is depth zero.
    pub fn ancestor_depth(conn: &Connection, session_id: &VibexSessionId) -> VibexResult<u32> {
        let mut current = session_id.clone();
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(current.as_str().to_string());
        for depth in 0..32_u32 {
            let Some(parent) = Self::parent_of(conn, &current)? else {
                return Ok(depth);
            };
            if !seen.insert(parent.as_str().to_string()) {
                return Err(VibexError::storage(
                    "vibex_use_ownership_cycle",
                    "the session ownership tree contains a cycle",
                ));
            }
            current = parent;
        }
        Err(VibexError::storage(
            "vibex_use_ownership_depth_exceeded",
            "the session ownership tree exceeds the supported depth",
        ))
    }

    /// Topmost session of the ownership tree that contains `session_id`.
    pub fn root_of(conn: &Connection, session_id: &VibexSessionId) -> VibexResult<VibexSessionId> {
        let mut current = session_id.clone();
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(current.as_str().to_string());
        for _ in 0..32 {
            let Some(parent) = Self::parent_of(conn, &current)? else {
                return Ok(current);
            };
            if !seen.insert(parent.as_str().to_string()) {
                return Err(VibexError::storage(
                    "vibex_use_ownership_cycle",
                    "the session ownership tree contains a cycle",
                ));
            }
            current = parent;
        }
        Err(VibexError::storage(
            "vibex_use_ownership_depth_exceeded",
            "the session ownership tree exceeds the supported depth",
        ))
    }

    /// Every descendant of `session_id`, deepest first, excluding the root.
    ///
    /// The order is what a cascade delete needs: a child is always removed
    /// before the session that owns it.
    pub fn descendant_ids(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Vec<VibexSessionId>> {
        let mut seen = std::collections::BTreeSet::from([session_id.as_str().to_string()]);
        let mut frontier = vec![session_id.clone()];
        let mut descendants = Vec::new();
        for _ in 0..32 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for parent in frontier {
                for child in Self::child_ids(conn, &parent)? {
                    if !seen.insert(child.as_str().to_string()) {
                        return Err(VibexError::storage(
                            "vibex_use_ownership_cycle",
                            "the session ownership tree contains a duplicate or cycle",
                        ));
                    }
                    descendants.push(child.clone());
                    next.push(child);
                }
            }
            frontier = next;
        }
        if !frontier.is_empty() {
            return Err(VibexError::storage(
                "vibex_use_ownership_depth_exceeded",
                "the session ownership tree exceeds the supported depth",
            ));
        }
        descendants.reverse();
        Ok(descendants)
    }

    /// Applies the ownership edges of one delegation row after the fact.
    ///
    /// It exists for recovery: a crash between persisting the child session and
    /// writing the edge leaves a task that can still be repaired.
    pub fn sync_from_delegation(
        conn: &Connection,
        delegation: &AgentDelegation,
    ) -> VibexResult<()> {
        let Some(child) = delegation.child_session_id.as_ref() else {
            return Ok(());
        };
        if delegation.ownership_kind != DelegationOwnershipKind::OwnedChild {
            return Ok(());
        }
        Self::upsert(
            conn,
            child,
            &delegation.parent_session_id,
            Some(&delegation.id),
        )
    }
}

// ---------------------------------------------------------------------------
// Session controllers
// ---------------------------------------------------------------------------

/// Result of trying to become the automated writer of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionControllerClaim {
    Claimed(SessionController),
    /// Another task already owns the write path.
    Denied(SessionController),
    /// The user took the session over.
    HumanControlled(SessionController),
    /// `expected_revision` did not match.
    StaleRevision(SessionController),
}

pub struct SessionControllerRepository;

impl SessionControllerRepository {
    pub fn ensure(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<SessionController> {
        conn.execute(
            "
            INSERT OR IGNORE INTO vibex_use_session_controllers (
                session_id, owner_task_id, owner_parent_session_id, revision,
                human_controlled, updated_at_ms
            )
            VALUES (?1, NULL, NULL, 1, 0, ?2)
            ",
            params![session_id.as_str(), unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "vibex_use_controller_write_failed",
            "failed to initialize a session controller",
        ))?;
        Self::get(conn, session_id)?.ok_or_else(|| {
            VibexError::storage(
                "vibex_use_controller_missing",
                "a session controller disappeared after it was created",
            )
        })
    }

    pub fn get(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<SessionController>> {
        conn.query_row(
            "
            SELECT session_id, owner_task_id, owner_parent_session_id, revision,
                human_controlled, updated_at_ms
            FROM vibex_use_session_controllers
            WHERE session_id = ?1
            ",
            params![session_id.as_str()],
            |row| {
                Ok(SessionController {
                    session_id: parse_id_sql(row.get(0)?, VibexSessionId::parse)?,
                    owner_task_id: parse_optional_id_sql(row.get(1)?, AgentDelegationId::parse)?,
                    owner_parent_session_id: parse_optional_id_sql(
                        row.get(2)?,
                        VibexSessionId::parse,
                    )?,
                    revision: u64_from_sql(row.get(3)?)?,
                    human_controlled: row.get::<_, i64>(4)? != 0,
                    updated_at_ms: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_controller_lookup_failed",
            "failed to read a session controller",
        ))
    }

    /// Claims the automated write path for one task.
    ///
    /// The claim is a compare-and-swap on `revision`, so two tasks racing for
    /// the same session cannot both believe they own it.
    pub fn claim(
        conn: &Connection,
        session_id: &VibexSessionId,
        task_id: &AgentDelegationId,
        parent_session_id: &VibexSessionId,
        expected_revision: Option<u64>,
    ) -> VibexResult<SessionControllerClaim> {
        let current = Self::ensure(conn, session_id)?;
        if current.human_controlled {
            return Ok(SessionControllerClaim::HumanControlled(current));
        }
        if let Some(expected) = expected_revision
            && expected != current.revision
        {
            return Ok(SessionControllerClaim::StaleRevision(current));
        }
        let is_same_task = current.owner_task_id.as_ref() == Some(task_id);
        if !is_same_task && current.owner_task_id.is_some() {
            return Ok(SessionControllerClaim::Denied(current));
        }
        if is_same_task {
            return Ok(SessionControllerClaim::Claimed(current));
        }
        let next_revision = current.revision.saturating_add(1);
        let changed = conn
            .execute(
                "
                UPDATE vibex_use_session_controllers
                SET owner_task_id = ?2,
                    owner_parent_session_id = ?3,
                    revision = ?4,
                    updated_at_ms = ?5
                WHERE session_id = ?1 AND revision = ?6 AND human_controlled = 0
                ",
                params![
                    session_id.as_str(),
                    task_id.as_str(),
                    parent_session_id.as_str(),
                    i64::try_from(next_revision).unwrap_or(i64::MAX),
                    unix_timestamp_ms(),
                    i64::try_from(current.revision).unwrap_or(i64::MAX),
                ],
            )
            .map_err(storage_err(
                "vibex_use_controller_write_failed",
                "failed to claim a session controller",
            ))?;
        if changed == 0 {
            let latest = Self::ensure(conn, session_id)?;
            return Ok(if latest.human_controlled {
                SessionControllerClaim::HumanControlled(latest)
            } else {
                SessionControllerClaim::Denied(latest)
            });
        }
        Self::ensure(conn, session_id).map(SessionControllerClaim::Claimed)
    }

    /// Records an explicit human intervention. It is never reversed silently.
    pub fn mark_human_controlled(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<SessionController> {
        let current = Self::ensure(conn, session_id)?;
        let next_revision = current.revision.saturating_add(1);
        conn.execute(
            "
            UPDATE vibex_use_session_controllers
            SET human_controlled = 1, revision = ?2, updated_at_ms = ?3
            WHERE session_id = ?1
            ",
            params![
                session_id.as_str(),
                i64::try_from(next_revision).unwrap_or(i64::MAX),
                unix_timestamp_ms(),
            ],
        )
        .map_err(storage_err(
            "vibex_use_controller_write_failed",
            "failed to record a human session takeover",
        ))?;
        Self::ensure(conn, session_id)
    }

    /// Releases a task's claim once the task settles.
    pub fn release(
        conn: &Connection,
        session_id: &VibexSessionId,
        task_id: &AgentDelegationId,
    ) -> VibexResult<SessionController> {
        conn.execute(
            "
            UPDATE vibex_use_session_controllers
            SET owner_task_id = NULL,
                owner_parent_session_id = NULL,
                revision = revision + 1,
                updated_at_ms = ?3
            WHERE session_id = ?1 AND owner_task_id = ?2
            ",
            params![session_id.as_str(), task_id.as_str(), unix_timestamp_ms(),],
        )
        .map_err(storage_err(
            "vibex_use_controller_write_failed",
            "failed to release a session controller",
        ))?;
        Self::ensure(conn, session_id)
    }
}

// ---------------------------------------------------------------------------
// Task events and inbox
// ---------------------------------------------------------------------------

pub struct VibexUseEventRepository;

impl VibexUseEventRepository {
    /// Appends one event, or returns the existing row for the same event id.
    ///
    /// Idempotent by `event_id`, so a recovery pass that re-emits an event
    /// after a failed live delivery cannot produce a duplicate.
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        conn: &Connection,
        event_id: &str,
        root_session_id: Option<&VibexSessionId>,
        kind: DelegationTaskEventKind,
        task_id: Option<&AgentDelegationId>,
        session_id: Option<&VibexSessionId>,
        revision: u64,
        payload: &serde_json::Value,
    ) -> VibexResult<DelegationTaskEvent> {
        if let Some(existing) = Self::get(conn, event_id)? {
            return Ok(existing);
        }
        conn.execute(
            "
            INSERT OR IGNORE INTO vibex_use_task_events (
                event_id, root_session_id, task_id, session_id, kind, revision,
                payload_json, occurred_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                event_id,
                root_session_id.map(VibexSessionId::as_str),
                task_id.map(AgentDelegationId::as_str),
                session_id.map(VibexSessionId::as_str),
                enum_to_db(&kind)?,
                i64::try_from(revision).unwrap_or(i64::MAX),
                json_to_db_value(payload)?,
                unix_timestamp_ms(),
            ],
        )
        .map_err(storage_err(
            "vibex_use_event_write_failed",
            "failed to persist a delegation task event",
        ))?;
        Self::get(conn, event_id)?.ok_or_else(|| {
            VibexError::storage(
                "vibex_use_event_missing_after_insert",
                "a delegation task event disappeared after it was persisted",
            )
        })
    }

    pub fn get(conn: &Connection, event_id: &str) -> VibexResult<Option<DelegationTaskEvent>> {
        conn.query_row(
            &event_select_sql("WHERE event_id = ?1"),
            params![event_id],
            map_event,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_event_lookup_failed",
            "failed to read a delegation task event",
        ))
    }

    pub fn list_for_root(
        conn: &Connection,
        root_session_id: &VibexSessionId,
        after_cursor: i64,
        limit: usize,
    ) -> VibexResult<Vec<DelegationTaskEvent>> {
        let limit = i64::try_from(limit.min(500)).unwrap_or(500);
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY sequence ASC LIMIT ?3",
                event_select_sql("WHERE root_session_id = ?1 AND sequence > ?2")
            ))
            .map_err(storage_err(
                "vibex_use_event_list_failed",
                "failed to prepare a delegation task event list",
            ))?;
        let rows = statement
            .query_map(
                params![root_session_id.as_str(), after_cursor, limit],
                map_event,
            )
            .map_err(storage_err(
                "vibex_use_event_list_failed",
                "failed to list delegation task events",
            ))?;
        collect_rows(
            rows,
            "vibex_use_event_decode_failed",
            "failed to decode a delegation task event",
        )
    }

    pub fn list_for_task(
        conn: &Connection,
        task_id: &AgentDelegationId,
        after_cursor: i64,
        limit: usize,
    ) -> VibexResult<Vec<DelegationTaskEvent>> {
        let limit = i64::try_from(limit.min(500)).unwrap_or(500);
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY sequence ASC LIMIT ?3",
                event_select_sql("WHERE task_id = ?1 AND sequence > ?2")
            ))
            .map_err(storage_err(
                "vibex_use_event_list_failed",
                "failed to prepare a delegation task event list",
            ))?;
        let rows = statement
            .query_map(params![task_id.as_str(), after_cursor, limit], map_event)
            .map_err(storage_err(
                "vibex_use_event_list_failed",
                "failed to list delegation task events",
            ))?;
        collect_rows(
            rows,
            "vibex_use_event_decode_failed",
            "failed to decode a delegation task event",
        )
    }

    /// Marks events as delivered to one consumer.
    ///
    /// Delivery is a transport fact and is recorded separately from
    /// acknowledgement, because writing a response successfully is not evidence
    /// that the model read it.
    pub fn mark_delivered(
        conn: &Connection,
        consumer_key: &str,
        event_ids: &[String],
    ) -> VibexResult<usize> {
        let now = unix_timestamp_ms();
        let mut delivered = 0;
        for event_id in event_ids {
            delivered += conn
                .execute(
                    "
                    INSERT INTO vibex_use_event_deliveries (
                        event_id, consumer_key, delivered_at_ms, acknowledged_at_ms
                    )
                    VALUES (?1, ?2, ?3, NULL)
                    ON CONFLICT(event_id, consumer_key) DO NOTHING
                    ",
                    params![event_id, consumer_key, now],
                )
                .map_err(storage_err(
                    "vibex_use_event_delivery_failed",
                    "failed to record an event delivery",
                ))?;
        }
        Ok(delivered)
    }

    /// Acknowledges explicit event ids. Unknown ids are ignored.
    pub fn acknowledge(
        conn: &Connection,
        consumer_key: &str,
        event_ids: &[String],
    ) -> VibexResult<usize> {
        let now = unix_timestamp_ms();
        let mut acknowledged = 0;
        for event_id in event_ids {
            acknowledged += conn
                .execute(
                    "
                    UPDATE vibex_use_event_deliveries
                    SET acknowledged_at_ms = ?3
                    WHERE event_id = ?1 AND consumer_key = ?2 AND acknowledged_at_ms IS NULL
                    ",
                    params![event_id, consumer_key, now],
                )
                .map_err(storage_err(
                    "vibex_use_event_acknowledge_failed",
                    "failed to acknowledge a delegation task event",
                ))?;
        }
        Ok(acknowledged)
    }

    /// Acknowledges every event up to and including `through_cursor`.
    pub fn acknowledge_through(
        conn: &Connection,
        consumer_key: &str,
        through_cursor: i64,
    ) -> VibexResult<usize> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            INSERT INTO vibex_use_event_deliveries (
                event_id, consumer_key, delivered_at_ms, acknowledged_at_ms
            )
            SELECT event_id, ?1, ?2, ?2
            FROM vibex_use_task_events
            WHERE sequence <= ?3
            ON CONFLICT(event_id, consumer_key) DO UPDATE SET
                acknowledged_at_ms = COALESCE(
                    vibex_use_event_deliveries.acknowledged_at_ms,
                    excluded.acknowledged_at_ms
                )
            ",
            params![consumer_key, now, through_cursor],
        )
        .map_err(storage_err(
            "vibex_use_event_acknowledge_failed",
            "failed to acknowledge a range of delegation task events",
        ))
    }

    /// Events of one root that this consumer has not acknowledged yet.
    ///
    /// `include_acknowledged` turns this into a plain replay read: a client that
    /// reconnects after acknowledging can still walk its own history from a
    /// cursor instead of finding the events it never rendered simply gone.
    pub fn unacknowledged_for_root(
        conn: &Connection,
        consumer_key: &str,
        root_session_id: &VibexSessionId,
        after_cursor: i64,
        limit: usize,
        include_acknowledged: bool,
    ) -> VibexResult<Vec<DelegationTaskEvent>> {
        let limit = i64::try_from(limit.min(500)).unwrap_or(500);
        let mut statement = conn
            .prepare(
                "
                SELECT e.sequence, e.event_id, e.root_session_id, e.task_id, e.session_id,
                    e.kind, e.revision, e.payload_json, e.occurred_at_ms,
                    d.acknowledged_at_ms IS NOT NULL AS acknowledged
                FROM vibex_use_task_events e
                LEFT JOIN vibex_use_event_deliveries d
                    ON d.event_id = e.event_id AND d.consumer_key = ?2
                WHERE e.root_session_id = ?1
                  AND e.sequence > ?3
                  AND (?5 = 1 OR d.acknowledged_at_ms IS NULL)
                ORDER BY e.sequence ASC
                LIMIT ?4
                ",
            )
            .map_err(storage_err(
                "vibex_use_event_list_failed",
                "failed to prepare the unacknowledged event list",
            ))?;
        let rows = statement
            .query_map(
                params![
                    root_session_id.as_str(),
                    consumer_key,
                    after_cursor,
                    limit,
                    i64::from(include_acknowledged)
                ],
                map_event_with_ack,
            )
            .map_err(storage_err(
                "vibex_use_event_list_failed",
                "failed to list unacknowledged delegation task events",
            ))?;
        let events = collect_rows(
            rows,
            "vibex_use_event_decode_failed",
            "failed to decode a delegation task event",
        )?;
        // Delivery means "this page was handed to the consumer". An
        // acknowledged replay is a history read, so it must not re-mark
        // anything.
        if !include_acknowledged {
            let ids: Vec<String> = events.iter().map(|event| event.event_id.clone()).collect();
            Self::mark_delivered(conn, consumer_key, &ids)?;
        }
        Ok(events)
    }
}

fn event_select_sql(where_clause: &str) -> String {
    format!(
        "
        SELECT sequence, event_id, root_session_id, task_id, session_id, kind,
            revision, payload_json, occurred_at_ms, NULL
        FROM vibex_use_task_events
        {where_clause}
        "
    )
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<DelegationTaskEvent> {
    map_event_with_ack(row)
}

fn map_event_with_ack(row: &rusqlite::Row<'_>) -> rusqlite::Result<DelegationTaskEvent> {
    let root: Option<String> = row.get(2)?;
    let task: Option<String> = row.get(3)?;
    let session: Option<String> = row.get(4)?;
    Ok(DelegationTaskEvent {
        cursor: row.get(0)?,
        event_id: row.get(1)?,
        kind: enum_from_db_sql(row.get(5)?)?,
        task_ref: task
            .map(|value| parse_id_sql(value, AgentDelegationId::parse))
            .transpose()?
            .map(|id| VibexUseRef::task(&id)),
        session_ref: session
            .map(|value| parse_id_sql(value, VibexSessionId::parse))
            .transpose()?
            .map(|id| VibexUseRef::session(&id)),
        revision: u64_from_sql(row.get(6)?)?,
        payload: json_from_db_sql(row.get(7)?)?,
        occurred_at_ms: row.get(8)?,
        delivered: true,
        acknowledged: row.get::<_, Option<i64>>(9)?.is_some(),
        // The root travels inside the payload so a compact projection can
        // still answer "which team is this" without another query.
        root_session_ref: root
            .map(|value| parse_id_sql(value, VibexSessionId::parse))
            .transpose()?
            .map(|id| VibexUseRef::session(&id)),
    })
}

// ---------------------------------------------------------------------------
// Group presentation intents
// ---------------------------------------------------------------------------

/// One runtime-side record of a group an Agent asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPresentationRecord {
    pub group_id: String,
    pub operation_id: Option<VibexOperationId>,
    pub actor_key: String,
    pub name: String,
    pub project_id: String,
    pub workspace_id: String,
    pub member_session_ids: Vec<VibexSessionId>,
    pub layout: SessionGroupLayoutIntent,
    /// Definition revision owned by this repository. It moves when the stored
    /// definition changes.
    pub revision: u64,
    /// Revision the connected client reported for the layout it actually
    /// rendered. It is the client's *content fingerprint*, so it is compared
    /// for equality and never treated as a counter that only goes up.
    pub client_revision: Option<u64>,
    pub created_by_caller: bool,
    pub state: GroupPresentationState,
    pub applied_layout: Option<serde_json::Value>,
    pub presentation_reason: Option<String>,
    /// Fingerprint of the request that created the group, so a retry is
    /// recognized before a second group row is written.
    pub payload_fingerprint: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl GroupPresentationRecord {
    /// The revision a caller should read and quote back.
    ///
    /// Once a client has applied a layout, the client's fingerprint is the
    /// authority: it is what the user's manual rearrangement moves, so quoting
    /// the repository's own counter back would let a stale write look fresh.
    pub fn presentation_revision(&self) -> u64 {
        self.client_revision.unwrap_or(self.revision)
    }
}

/// Durable stage of one group request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupPresentationState {
    Created,
    Prepared,
    Applied,
    Presented,
    Deferred,
    Unavailable,
}

impl GroupPresentationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Prepared => "prepared",
            Self::Applied => "applied",
            Self::Presented => "presented",
            Self::Deferred => "deferred",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "created" => Self::Created,
            "prepared" => Self::Prepared,
            "applied" => Self::Applied,
            "presented" => Self::Presented,
            "deferred" => Self::Deferred,
            "unavailable" => Self::Unavailable,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub enum GroupPresentationReservation {
    Claimed(GroupPresentationRecord),
    /// The same idempotency key already produced this group.
    Existing(GroupPresentationRecord),
    /// The key was reused with a different payload.
    Conflict(GroupPresentationRecord),
}

pub struct GroupPresentationRepository;

impl GroupPresentationRepository {
    /// Reserves the stable group id of one operation.
    ///
    /// The id is chosen before the shell is asked to create anything, so a
    /// retry after a lost reply resolves to the same group instead of drawing a
    /// second copy of the team.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_or_get(
        conn: &Connection,
        operation_id: Option<&VibexOperationId>,
        actor_key: &str,
        group_id: &str,
        scope: &SessionGroupScope,
        name: &str,
        members: &[VibexSessionId],
        layout: &SessionGroupLayoutIntent,
        fingerprint: &str,
    ) -> VibexResult<GroupPresentationReservation> {
        if let Some(existing) = Self::get_by_actor_and_fingerprint(conn, actor_key, fingerprint)? {
            return Ok(GroupPresentationReservation::Existing(existing));
        }
        if let Some(operation_id) = operation_id
            && let Some(existing) = Self::get_by_operation(conn, operation_id)?
        {
            return Ok(GroupPresentationReservation::Existing(existing));
        }
        let (project_id, workspace_id) = match scope {
            SessionGroupScope::Workspace { workspace_ref } => {
                (String::new(), workspace_ref.id.clone())
            }
            SessionGroupScope::ProjectTeam { project_id } => (project_id.clone(), String::new()),
        };
        let now = unix_timestamp_ms();
        conn.execute(
            "
            INSERT INTO vibex_use_group_presentations (
                group_id, operation_id, actor_key, name, workspace_id, project_id,
                member_session_ids_json, layout_json, revision, created_by_caller,
                state, applied_layout_json, presentation_reason, payload_fingerprint,
                created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, 1, 'created', NULL, NULL, ?9, ?10, ?10)
            ",
            params![
                group_id,
                operation_id.map(VibexOperationId::as_str),
                actor_key,
                name,
                workspace_id,
                project_id,
                json_to_db_value(&members)?,
                json_to_db_value(layout)?,
                fingerprint,
                now,
            ],
        )
        .map_err(storage_err(
            "vibex_use_group_reserve_failed",
            "failed to reserve a session group presentation",
        ))?;
        Self::get(conn, group_id)?
            .map(GroupPresentationReservation::Claimed)
            .ok_or_else(|| {
                VibexError::storage(
                    "vibex_use_group_missing_after_insert",
                    "a session group presentation disappeared after it was persisted",
                )
            })
    }

    /// Finds an earlier request with the same caller key and payload.
    pub fn get_by_actor_and_fingerprint(
        conn: &Connection,
        actor_key: &str,
        fingerprint: &str,
    ) -> VibexResult<Option<GroupPresentationRecord>> {
        conn.query_row(
            &group_select_sql(
                "WHERE actor_key = ?1 AND payload_fingerprint = ?2 ORDER BY created_at_ms ASC LIMIT 1",
            ),
            params![actor_key, fingerprint],
            map_group,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_group_lookup_failed",
            "failed to read a session group presentation",
        ))
    }

    pub fn get(conn: &Connection, group_id: &str) -> VibexResult<Option<GroupPresentationRecord>> {
        conn.query_row(
            &group_select_sql("WHERE group_id = ?1"),
            params![group_id],
            map_group,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_group_lookup_failed",
            "failed to read a session group presentation",
        ))
    }

    /// Groups whose presentation was never confirmed by a client.
    ///
    /// Startup recovery walks these so a group that was stored while no shell
    /// was attached is offered to the next client that connects, instead of
    /// staying invisible until somebody asks again.
    pub fn list_unpresented(
        conn: &Connection,
        limit: usize,
    ) -> VibexResult<Vec<GroupPresentationRecord>> {
        let limit = i64::try_from(limit.min(500)).unwrap_or(500);
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY updated_at_ms ASC, group_id ASC LIMIT ?1",
                group_select_sql("WHERE state IN ('created', 'prepared', 'deferred')")
            ))
            .map_err(storage_err(
                "vibex_use_group_list_failed",
                "failed to prepare an unpresented session group list",
            ))?;
        let rows = statement
            .query_map(params![limit], map_group)
            .map_err(storage_err(
                "vibex_use_group_list_failed",
                "failed to list unpresented session groups",
            ))?;
        collect_rows(
            rows,
            "vibex_use_group_decode_failed",
            "failed to decode a session group presentation",
        )
    }

    pub fn get_by_operation(
        conn: &Connection,
        operation_id: &VibexOperationId,
    ) -> VibexResult<Option<GroupPresentationRecord>> {
        conn.query_row(
            &group_select_sql("WHERE operation_id = ?1"),
            params![operation_id.as_str()],
            map_group,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_group_lookup_failed",
            "failed to read a session group presentation",
        ))
    }

    pub fn list_for_actor(
        conn: &Connection,
        actor_key: &str,
        limit: usize,
    ) -> VibexResult<Vec<GroupPresentationRecord>> {
        Self::list(conn, "actor_key = ?1", params![actor_key], limit)
    }

    pub fn list_for_workspace(
        conn: &Connection,
        workspace_id: &str,
        limit: usize,
    ) -> VibexResult<Vec<GroupPresentationRecord>> {
        Self::list(conn, "workspace_id = ?1", params![workspace_id], limit)
    }

    fn list(
        conn: &Connection,
        predicate: &str,
        parameters: impl rusqlite::Params,
        limit: usize,
    ) -> VibexResult<Vec<GroupPresentationRecord>> {
        let limit = i64::try_from(limit.min(500)).unwrap_or(500);
        let mut statement = conn
            .prepare(&format!(
                "{} ORDER BY updated_at_ms DESC, group_id ASC LIMIT {limit}",
                group_select_sql(&format!("WHERE {predicate}"))
            ))
            .map_err(storage_err(
                "vibex_use_group_list_failed",
                "failed to prepare a session group presentation list",
            ))?;
        let rows = statement
            .query_map(parameters, map_group)
            .map_err(storage_err(
                "vibex_use_group_list_failed",
                "failed to list session group presentations",
            ))?;
        collect_rows(
            rows,
            "vibex_use_group_decode_failed",
            "failed to decode a session group presentation",
        )
    }

    /// Applies a typed membership/name/layout change under a revision guard.
    ///
    /// `None` from the guard means the stored revision no longer matches, which
    /// the service reports as a layout conflict instead of overwriting a layout
    /// the user has since adjusted by hand.
    pub fn update(
        conn: &Connection,
        group_id: &str,
        expected_revision: Option<u64>,
        name: Option<&str>,
        members: Option<&[VibexSessionId]>,
        layout: Option<&SessionGroupLayoutIntent>,
    ) -> VibexResult<Option<GroupPresentationRecord>> {
        let Some(current) = Self::get(conn, group_id)? else {
            return Ok(None);
        };
        if let Some(expected) = expected_revision
            && expected != current.presentation_revision()
        {
            return Ok(None);
        }
        let next_revision = current.revision.saturating_add(1);
        conn.execute(
            "
            UPDATE vibex_use_group_presentations
            SET name = COALESCE(?2, name),
                member_session_ids_json = COALESCE(?3, member_session_ids_json),
                layout_json = COALESCE(?4, layout_json),
                payload_fingerprint = NULL,
                client_revision = NULL,
                revision = ?5,
                updated_at_ms = ?6
            WHERE group_id = ?1 AND revision = ?7
            ",
            params![
                group_id,
                name,
                members.map(json_to_db_value).transpose()?,
                layout.map(json_to_db_value).transpose()?,
                i64::try_from(next_revision).unwrap_or(i64::MAX),
                unix_timestamp_ms(),
                i64::try_from(current.revision).unwrap_or(i64::MAX),
            ],
        )
        .map_err(storage_err(
            "vibex_use_group_update_failed",
            "failed to update a session group presentation",
        ))?;
        Self::get(conn, group_id)
    }

    pub fn record_presentation(
        conn: &Connection,
        group_id: &str,
        state: GroupPresentationState,
        applied_layout: Option<&serde_json::Value>,
        reason: Option<&str>,
        client_revision: Option<u64>,
    ) -> VibexResult<Option<GroupPresentationRecord>> {
        conn.execute(
            "
            UPDATE vibex_use_group_presentations
            SET state = ?2,
                applied_layout_json = COALESCE(?3, applied_layout_json),
                presentation_reason = ?4,
                client_revision = COALESCE(?6, client_revision),
                updated_at_ms = ?5
            WHERE group_id = ?1
            ",
            params![
                group_id,
                state.as_str(),
                applied_layout.map(json_to_db_value).transpose()?,
                reason,
                unix_timestamp_ms(),
                client_revision.map(|revision| i64::try_from(revision).unwrap_or(i64::MAX)),
            ],
        )
        .map_err(storage_err(
            "vibex_use_group_update_failed",
            "failed to record a session group presentation outcome",
        ))?;
        Self::get(conn, group_id)
    }

    /// Hands the group back to the user so later layout edits are not treated
    /// as an Agent-owned change.
    pub fn release_to_user(
        conn: &Connection,
        group_id: &str,
    ) -> VibexResult<Option<GroupPresentationRecord>> {
        conn.execute(
            "
            UPDATE vibex_use_group_presentations
            SET created_by_caller = 0, revision = revision + 1, client_revision = NULL,
                updated_at_ms = ?2
            WHERE group_id = ?1
            ",
            params![group_id, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "vibex_use_group_update_failed",
            "failed to release a session group presentation",
        ))?;
        Self::get(conn, group_id)
    }

    pub fn delete(conn: &Connection, group_id: &str) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "DELETE FROM vibex_use_group_presentations WHERE group_id = ?1",
                params![group_id],
            )
            .map_err(storage_err(
                "vibex_use_group_delete_failed",
                "failed to dissolve a session group presentation",
            ))?;
        Ok(changed > 0)
    }
}

fn group_select_sql(where_clause: &str) -> String {
    format!(
        "
        SELECT group_id, operation_id, actor_key, name, workspace_id, project_id,
            member_session_ids_json, layout_json, revision, created_by_caller,
            state, applied_layout_json, presentation_reason, created_at_ms,
            updated_at_ms, payload_fingerprint, client_revision
        FROM vibex_use_group_presentations
        {where_clause}
        "
    )
}

fn map_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupPresentationRecord> {
    let state: String = row.get(10)?;
    let state = GroupPresentationState::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            10,
            rusqlite::types::Type::Text,
            Box::new(VibexError::storage(
                "vibex_use_group_state_invalid",
                "a stored session group presentation has an unknown state",
            )),
        )
    })?;
    let members: Vec<String> = json_from_db_sql(row.get(6)?)?;
    let members = members
        .into_iter()
        .map(|value| parse_id_sql(value, VibexSessionId::parse))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(GroupPresentationRecord {
        group_id: row.get(0)?,
        operation_id: parse_optional_id_sql(row.get(1)?, VibexOperationId::parse)?,
        actor_key: row.get(2)?,
        name: row.get(3)?,
        workspace_id: row.get(4)?,
        project_id: row.get(5)?,
        member_session_ids: members,
        layout: optional_json_from_db_sql(row.get(7)?)?.unwrap_or_default(),
        revision: u64_from_sql(row.get(8)?)?,
        client_revision: row
            .get::<_, Option<i64>>(16)?
            .map(|value| u64::try_from(value).unwrap_or_default()),
        created_by_caller: row.get::<_, i64>(9)? != 0,
        state,
        applied_layout: optional_json_from_db_sql(row.get(11)?)?,
        presentation_reason: row.get(12)?,
        created_at_ms: row.get(13)?,
        updated_at_ms: row.get(14)?,
        payload_fingerprint: row.get(15)?,
    })
}

// ---------------------------------------------------------------------------
// Delegation task extensions
// ---------------------------------------------------------------------------

const DELEGATION_TASK_COLUMNS: &str = "
    completion_policy, task_phase, ownership_kind, root_session_id, follows_task_id,
    revision, controller_revision, current_execution_id, blocked_on_json,
    context_refs_json, acceptance_criteria_json, requested_runtime_json,
    effective_runtime_json, result_refs_json, cancellation_requested_at_ms,
    finished_at_ms, payload_fingerprint
";

/// Writes the explicit phase and the compatibility status together.
///
/// Keeping this in one statement is what makes "the new phase is the domain
/// state and the old status is a projection" true rather than aspirational: no
/// second writer can move one column without the other.
///
/// Returns `None` when the row is already terminal.
#[allow(clippy::too_many_arguments)]
pub fn transition_delegation(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
    phase: DelegationTaskPhase,
    result_summary: Option<&str>,
    error_code: Option<&str>,
) -> VibexResult<Option<AgentDelegation>> {
    let status = legacy_status_for_phase(phase);
    let now = unix_timestamp_ms();
    let changed = conn
        .execute(
            "
            UPDATE agent_delegations
            SET task_phase = ?2,
                status = ?3,
                result_summary = COALESCE(?4, result_summary),
                error_code = ?5,
                revision = revision + 1,
                updated_at_ms = ?6,
                started_at_ms = CASE
                    WHEN ?7 = 1 THEN COALESCE(started_at_ms, ?6)
                    ELSE started_at_ms
                END,
                completed_at_ms = CASE
                    WHEN ?8 = 1 THEN COALESCE(completed_at_ms, ?6)
                    ELSE completed_at_ms
                END,
                finished_at_ms = CASE
                    WHEN ?9 = 1 THEN COALESCE(finished_at_ms, ?6)
                    ELSE finished_at_ms
                END
            WHERE delegation_id = ?1
              AND task_phase NOT IN ('completed', 'failed', 'cancelled')
              -- A fence that was accepted is not undone by a late snapshot. A
              -- cancelling task may only move on to a stop outcome; a watcher
              -- seeing the child go idle must not paint it back to working.
              AND (
                    task_phase <> 'cancelling'
                    OR ?2 IN ('cancelling', 'cancelled', 'failed')
                  )
            ",
            params![
                delegation_id.as_str(),
                enum_to_db(&phase)?,
                enum_to_db(&status)?,
                result_summary,
                error_code,
                now,
                i64::from(matches!(
                    phase,
                    DelegationTaskPhase::Starting
                        | DelegationTaskPhase::Active
                        | DelegationTaskPhase::AwaitingReview
                        | DelegationTaskPhase::Cancelling
                )),
                i64::from(phase.is_terminal()),
                i64::from(phase.is_terminal()),
            ],
        )
        .map_err(storage_err(
            "agent_delegation_status_update_failed",
            "failed to update the phase of an Agent delegation",
        ))?;
    if changed == 0 {
        return crate::AgentDelegationRepository::get(conn, delegation_id);
    }
    crate::AgentDelegationRepository::get(conn, delegation_id)
}

/// Reads one delegation with the Vibex-use columns included.
pub fn get_delegation(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
) -> VibexResult<Option<AgentDelegation>> {
    crate::AgentDelegationRepository::get(conn, delegation_id)
}

/// Tasks that a root session currently owns, newest first.
pub fn list_delegations_for_root(
    conn: &Connection,
    root_session_id: &VibexSessionId,
    include_finished: bool,
) -> VibexResult<Vec<AgentDelegation>> {
    let mut statement = conn
        .prepare(&format!(
            "
            SELECT delegation_id, parent_session_id, parent_timeline_item_id,
                child_session_id, idempotency_key, title, task_summary,
                requested_agent_id, effective_agent_id, status, result_summary,
                error_code, created_at_ms, updated_at_ms, started_at_ms,
                completed_at_ms, {DELEGATION_TASK_COLUMNS}
            FROM agent_delegations
            WHERE root_session_id = ?1
            {}
            ORDER BY created_at_ms DESC, delegation_id ASC
            ",
            if include_finished {
                ""
            } else {
                "AND task_phase NOT IN ('completed', 'failed', 'cancelled')"
            }
        ))
        .map_err(storage_err(
            "agent_delegation_list_failed",
            "failed to prepare a root delegation list",
        ))?;
    let rows = statement
        .query_map(
            params![root_session_id.as_str()],
            crate::map_agent_delegation,
        )
        .map_err(storage_err(
            "agent_delegation_list_failed",
            "failed to list the delegations of a root session",
        ))?;
    collect_rows(
        rows,
        "agent_delegation_decode_failed",
        "failed to decode an Agent delegation",
    )
}

/// Every task that ever used one child session, oldest first.
///
/// A long-lived worker session can serve several tasks; the ownership tree
/// shows one node for it while the detail view lists this history.
pub fn list_delegations_for_child(
    conn: &Connection,
    child_session_id: &VibexSessionId,
) -> VibexResult<Vec<AgentDelegation>> {
    let mut statement = conn
        .prepare(&format!(
            "
            SELECT delegation_id, parent_session_id, parent_timeline_item_id,
                child_session_id, idempotency_key, title, task_summary,
                requested_agent_id, effective_agent_id, status, result_summary,
                error_code, created_at_ms, updated_at_ms, started_at_ms,
                completed_at_ms, {DELEGATION_TASK_COLUMNS}
            FROM agent_delegations
            WHERE child_session_id = ?1
            ORDER BY created_at_ms ASC, delegation_id ASC
            "
        ))
        .map_err(storage_err(
            "agent_delegation_list_failed",
            "failed to prepare a child delegation list",
        ))?;
    let rows = statement
        .query_map(
            params![child_session_id.as_str()],
            crate::map_agent_delegation,
        )
        .map_err(storage_err(
            "agent_delegation_list_failed",
            "failed to list the delegations of a child session",
        ))?;
    collect_rows(
        rows,
        "agent_delegation_decode_failed",
        "failed to decode an Agent delegation",
    )
}

/// The cancelling task that fences a session, if one exists.
///
/// An accepted cancel is a persistent fence, not a best-effort interrupt: once
/// the fence is written, nothing underneath it may accept a new message or
/// start a new child while the stop is still being confirmed. The walk covers
/// the session's own ancestors, so a fence on a parent task covers its whole
/// subtree — and never a sibling, because a sibling's session is not on this
/// path.
pub fn delegation_cancellation_fence(
    conn: &Connection,
    session_id: &VibexSessionId,
) -> VibexResult<Option<AgentDelegationId>> {
    let mut statement = conn
        .prepare(
            "
            SELECT delegation_id FROM agent_delegations
            WHERE child_session_id = ?1 AND task_phase = 'cancelling'
            ORDER BY created_at_ms ASC, delegation_id ASC
            LIMIT 1
            ",
        )
        .map_err(storage_err(
            "agent_delegation_fence_failed",
            "failed to prepare a delegation cancellation fence lookup",
        ))?;
    let mut current = session_id.clone();
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(current.as_str().to_string());
    for _ in 0..32 {
        let fenced = statement
            .query_row(params![current.as_str()], |row| row.get::<_, String>(0))
            .optional()
            .map_err(storage_err(
                "agent_delegation_fence_failed",
                "failed to look up a delegation cancellation fence",
            ))?;
        if let Some(id) = fenced {
            return Ok(Some(AgentDelegationId::parse(id).map_err(|_| {
                VibexError::storage(
                    "agent_delegation_fence_failed",
                    "a delegation cancellation fence held an invalid task id",
                )
            })?));
        }
        let Some(parent) = SessionOwnershipRepository::parent_of(conn, &current)? else {
            return Ok(None);
        };
        if !seen.insert(parent.as_str().to_string()) {
            return Err(VibexError::storage(
                "vibex_use_ownership_cycle",
                "the session ownership tree contains a cycle",
            ));
        }
        current = parent;
    }
    Err(VibexError::storage(
        "vibex_use_ownership_depth_exceeded",
        "the session ownership tree exceeds the supported depth",
    ))
}

/// Active executions owned by one root session, used for the root budget.
pub fn count_active_executions_for_root(
    conn: &Connection,
    root_session_id: &VibexSessionId,
) -> VibexResult<u32> {
    let count = conn
        .query_row(
            "
            SELECT COUNT(*)
            FROM vibex_use_executions e
            JOIN agent_delegations d ON d.delegation_id = e.task_id
            WHERE d.root_session_id = ?1
              AND e.outcome IN ('queued', 'running')
            ",
            params![root_session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage_err(
            "vibex_use_execution_count_failed",
            "failed to count the active executions of a root session",
        ))?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Tasks of a root whose phase is still active.
pub fn count_active_delegations_for_root(
    conn: &Connection,
    root_session_id: &VibexSessionId,
) -> VibexResult<u32> {
    let count = conn
        .query_row(
            "
            SELECT COUNT(*) FROM agent_delegations
            WHERE root_session_id = ?1
              AND task_phase NOT IN ('completed', 'failed', 'cancelled')
            ",
            params![root_session_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage_err(
            "agent_delegation_active_count_failed",
            "failed to count the active delegations of a root session",
        ))?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Persists the parts of a delegation that only Vibex-use writes.
#[allow(clippy::too_many_arguments)]
pub fn update_delegation_vibex_use_fields(
    conn: &Connection,
    delegation: &AgentDelegation,
) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE agent_delegations
        SET current_execution_id = ?2,
            blocked_on_json = ?3,
            context_refs_json = ?4,
            acceptance_criteria_json = ?5,
            requested_runtime_json = ?6,
            effective_runtime_json = ?7,
            result_refs_json = ?8,
            controller_revision = ?9,
            root_session_id = COALESCE(root_session_id, ?10),
            payload_fingerprint = COALESCE(payload_fingerprint, ?11),
            updated_at_ms = ?12
        WHERE delegation_id = ?1
        ",
        params![
            delegation.id.as_str(),
            delegation
                .current_execution_id
                .as_ref()
                .map(VibexExecutionId::as_str),
            delegation
                .blocked_on
                .as_ref()
                .map(json_to_db_value)
                .transpose()?,
            json_to_db_value(&delegation.context_refs)?,
            json_to_db_value(&delegation.acceptance_criteria)?,
            delegation
                .requested_runtime
                .as_ref()
                .map(json_to_db_value)
                .transpose()?,
            delegation
                .effective_runtime
                .as_ref()
                .map(json_to_db_value)
                .transpose()?,
            json_to_db_value(&delegation.result_refs)?,
            i64::try_from(delegation.controller_revision).unwrap_or(i64::MAX),
            delegation
                .root_session_id
                .as_ref()
                .map(VibexSessionId::as_str),
            delegation.payload_fingerprint,
            unix_timestamp_ms(),
        ],
    )
    .map_err(storage_err(
        "agent_delegation_vibex_use_update_failed",
        "failed to persist the Vibex-use fields of an Agent delegation",
    ))?;
    Ok(())
}

/// Requests cancellation by moving the phase to `cancelling` first.
///
/// The fence is durable before the slow interrupt runs, so a task that is
/// mid-spawn cannot accept new work while it is being stopped.
pub fn request_delegation_cancellation(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
) -> VibexResult<Option<AgentDelegation>> {
    let now = unix_timestamp_ms();
    let changed = conn
        .execute(
            "
            UPDATE agent_delegations
            SET task_phase = 'cancelling',
                status = 'running',
                cancellation_requested_at_ms = COALESCE(cancellation_requested_at_ms, ?2),
                revision = revision + 1,
                updated_at_ms = ?2
            WHERE delegation_id = ?1
              AND task_phase NOT IN ('completed', 'failed', 'cancelled')
            ",
            params![delegation_id.as_str(), now],
        )
        .map_err(storage_err(
            "agent_delegation_cancel_request_failed",
            "failed to record a delegation cancellation request",
        ))?;
    if changed == 0 {
        return crate::AgentDelegationRepository::get(conn, delegation_id);
    }
    crate::AgentDelegationRepository::get(conn, delegation_id)
}

/// Marks a queued execution ambiguous after a crash across the dispatch edge.
pub fn mark_execution_ambiguous(
    conn: &Connection,
    execution_id: &VibexExecutionId,
    stop_reason: &str,
) -> VibexResult<Option<DelegationExecution>> {
    VibexUseExecutionRepository::settle(
        conn,
        execution_id,
        ExecutionOutcome::Ambiguous,
        Some(stop_reason),
        Some("the prompt may have reached the provider; the outcome is unknown"),
        &[],
        &[],
        ExecutionUsageState::Unknown,
        false,
        None,
        unix_timestamp_ms(),
    )
}

/// Applies the ownership, runtime summary and acceptance criteria of a new task
/// in one statement.
pub fn initialize_delegation_task(
    conn: &Connection,
    delegation: &AgentDelegation,
    root_session_id: &VibexSessionId,
    requested_runtime: Option<&DelegationRuntimeSummary>,
    context_refs: &[DelegationContextRef],
    acceptance_criteria: &[String],
    payload_fingerprint: &str,
) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE agent_delegations
        SET root_session_id = ?2,
            requested_runtime_json = ?3,
            context_refs_json = ?4,
            acceptance_criteria_json = ?5,
            payload_fingerprint = ?6,
            controller_revision = 0,
            updated_at_ms = ?7
        WHERE delegation_id = ?1
        ",
        params![
            delegation.id.as_str(),
            root_session_id.as_str(),
            requested_runtime.map(json_to_db_value).transpose()?,
            json_to_db_value(context_refs)?,
            json_to_db_value(acceptance_criteria)?,
            payload_fingerprint,
            unix_timestamp_ms(),
        ],
    )
    .map_err(storage_err(
        "agent_delegation_initialize_failed",
        "failed to initialize an Agent delegation task",
    ))?;
    Ok(())
}

/// Persists the effective runtime summary and the completion policy together,
/// once the task's runtime has actually been resolved.
pub fn attach_delegation_runtime_summary(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
    effective_runtime: &DelegationRuntimeSummary,
    runtime_selection_revision: u64,
) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE agent_delegations
        SET effective_runtime_json = ?2,
            controller_revision = ?3,
            updated_at_ms = ?4
        WHERE delegation_id = ?1
        ",
        params![
            delegation_id.as_str(),
            json_to_db_value(effective_runtime)?,
            i64::try_from(runtime_selection_revision).unwrap_or(i64::MAX),
            unix_timestamp_ms(),
        ],
    )
    .map_err(storage_err(
        "agent_delegation_runtime_update_failed",
        "failed to persist the effective runtime of an Agent delegation",
    ))?;
    Ok(())
}

/// Appends one result reference, ignoring a repeat of the same execution.
pub fn append_delegation_result_ref(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
    result: &DelegationResultRef,
) -> VibexResult<Option<AgentDelegation>> {
    let Some(mut delegation) = crate::AgentDelegationRepository::get(conn, delegation_id)? else {
        return Ok(None);
    };
    if delegation
        .result_refs
        .iter()
        .any(|existing| existing.execution_ref == result.execution_ref)
    {
        return Ok(Some(delegation));
    }
    delegation.result_refs.push(result.clone());
    conn.execute(
        "
        UPDATE agent_delegations
        SET result_refs_json = ?2, updated_at_ms = ?3
        WHERE delegation_id = ?1
        ",
        params![
            delegation_id.as_str(),
            json_to_db_value(&delegation.result_refs)?,
            unix_timestamp_ms(),
        ],
    )
    .map_err(storage_err(
        "agent_delegation_result_update_failed",
        "failed to record a delegation result reference",
    ))?;
    crate::AgentDelegationRepository::get(conn, delegation_id)
}

/// Stores the child-session id and the ownership edge of a claimed task.
pub fn attach_child_session_with_ownership(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
    child_session_id: &VibexSessionId,
    effective_agent_id: &vibex_core::AgentId,
    parent_session_id: &VibexSessionId,
) -> VibexResult<Option<AgentDelegation>> {
    let updated = crate::AgentDelegationRepository::attach_claimed_child_session(
        conn,
        delegation_id,
        child_session_id,
        effective_agent_id,
    )?;
    if updated.is_some() {
        SessionOwnershipRepository::upsert(
            conn,
            child_session_id,
            parent_session_id,
            Some(delegation_id),
        )?;
    }
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Session grants
// ---------------------------------------------------------------------------

/// One explicit, revocable grant over a session the caller did not create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGrantRecord {
    pub grantee_session_id: VibexSessionId,
    pub target_session_id: VibexSessionId,
    pub scope: String,
    pub granted_by: String,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub struct SessionGrantRepository;

impl SessionGrantRepository {
    /// Grants (or upgrades) one scope. Upgrading is a revision bump, never a
    /// silent widening: the caller must ask for the wider scope explicitly.
    pub fn grant(
        conn: &Connection,
        grantee_session_id: &VibexSessionId,
        target_session_id: &VibexSessionId,
        scope: &str,
        granted_by: &str,
    ) -> VibexResult<SessionGrantRecord> {
        let now = unix_timestamp_ms();
        conn.execute(
            "
            INSERT INTO vibex_use_session_grants (
                grantee_session_id, target_session_id, scope, granted_by,
                revision, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)
            ON CONFLICT(grantee_session_id, target_session_id) DO UPDATE SET
                scope = excluded.scope,
                granted_by = excluded.granted_by,
                revision = vibex_use_session_grants.revision + 1,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                grantee_session_id.as_str(),
                target_session_id.as_str(),
                scope,
                granted_by,
                now,
            ],
        )
        .map_err(storage_err(
            "vibex_use_grant_write_failed",
            "failed to record a session grant",
        ))?;
        Self::get(conn, grantee_session_id, target_session_id)?.ok_or_else(|| {
            VibexError::storage(
                "vibex_use_grant_missing",
                "a session grant disappeared after it was persisted",
            )
        })
    }

    pub fn get(
        conn: &Connection,
        grantee_session_id: &VibexSessionId,
        target_session_id: &VibexSessionId,
    ) -> VibexResult<Option<SessionGrantRecord>> {
        conn.query_row(
            "
            SELECT grantee_session_id, target_session_id, scope, granted_by,
                revision, created_at_ms, updated_at_ms
            FROM vibex_use_session_grants
            WHERE grantee_session_id = ?1 AND target_session_id = ?2
            ",
            params![grantee_session_id.as_str(), target_session_id.as_str()],
            map_grant,
        )
        .optional()
        .map_err(storage_err(
            "vibex_use_grant_lookup_failed",
            "failed to read a session grant",
        ))
    }

    pub fn revoke(
        conn: &Connection,
        grantee_session_id: &VibexSessionId,
        target_session_id: &VibexSessionId,
    ) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "
                DELETE FROM vibex_use_session_grants
                WHERE grantee_session_id = ?1 AND target_session_id = ?2
                ",
                params![grantee_session_id.as_str(), target_session_id.as_str()],
            )
            .map_err(storage_err(
                "vibex_use_grant_delete_failed",
                "failed to revoke a session grant",
            ))?;
        Ok(changed > 0)
    }
}

fn map_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionGrantRecord> {
    Ok(SessionGrantRecord {
        grantee_session_id: parse_id_sql(row.get(0)?, VibexSessionId::parse)?,
        target_session_id: parse_id_sql(row.get(1)?, VibexSessionId::parse)?,
        scope: row.get(2)?,
        granted_by: row.get(3)?,
        revision: u64_from_sql(row.get(4)?)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

/// Records what a task is waiting for without touching its phase.
///
/// Blocking is deliberately not a terminal phase: a task that waits for a
/// permission grant is still active, and the waiting fact is what a team view
/// summarises.
pub fn set_delegation_blocked_on(
    conn: &Connection,
    delegation_id: &AgentDelegationId,
    blocked_on: Option<&DelegationBlockedOn>,
) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE agent_delegations
        SET blocked_on_json = ?2, updated_at_ms = ?3
        WHERE delegation_id = ?1
        ",
        params![
            delegation_id.as_str(),
            blocked_on.map(json_to_db_value).transpose()?,
            unix_timestamp_ms(),
        ],
    )
    .map_err(storage_err(
        "agent_delegation_blocked_update_failed",
        "failed to record what an Agent delegation is waiting for",
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use vibex_core::{
        AgentDelegationStatus, AgentId, AgentSession, AgentSessionSafety, AgentSessionState,
        RequestId, SendAgentMessageRequest, SessionRuntimeSelection, VibexUseResourceKind,
        WorkspaceMode,
    };

    use crate::{
        DbConnection, MessageSubmissionRepository, SessionRepository, WorkspaceRepository,
    };

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-vibex-use-{label}-{}.db",
            RequestId::new().as_str()
        ))
    }

    fn open_test_db(label: &str) -> (DbConnection, PathBuf) {
        let path = temp_db_path(label);
        let mut conn = crate::open_database(&path).unwrap();
        crate::apply_migrations(&mut conn).unwrap();
        (conn, path)
    }

    fn seed_session(conn: &DbConnection, title: &str) -> AgentSession {
        let workspace_root = std::env::temp_dir().join(format!(
            "vibex-vibex-use-workspace-{}",
            RequestId::new().as_str()
        ));
        let (project, workspace) =
            WorkspaceRepository::ensure(conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: title.to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(conn, &session).unwrap();
        session
    }

    fn seed_task(
        conn: &mut DbConnection,
        parent: &AgentSession,
        child: &AgentSession,
        key: &str,
    ) -> AgentDelegation {
        let now = unix_timestamp_ms();
        let mut task = AgentDelegation::single_turn_legacy(
            parent.id.clone(),
            key,
            "Review storage",
            "Inspect the transaction boundaries",
            Some(parent.agent_id.clone()),
            AgentDelegationStatus::Starting,
            now,
        );
        task.child_session_id = Some(child.id.clone());
        task.root_session_id = Some(parent.id.clone());
        let mut task =
            match crate::AgentDelegationRepository::reserve_or_get(conn, &task, 8).unwrap() {
                crate::AgentDelegationReservation::Claimed(task)
                | crate::AgentDelegationReservation::Existing(task) => task,
            };
        task.payload_fingerprint = Some("fingerprint".to_string());
        update_delegation_vibex_use_fields(conn, &task).unwrap();
        SessionOwnershipRepository::upsert(conn, &child.id, &parent.id, Some(&task.id)).unwrap();
        task
    }

    #[test]
    fn a_task_phase_and_its_legacy_status_always_move_together() {
        let (mut conn, path) = open_test_db("phase");
        let parent = seed_session(&conn, "Parent");
        let child = seed_session(&conn, "Child");
        let task = seed_task(&mut conn, &parent, &child, "phase-task");

        let active =
            transition_delegation(&conn, &task.id, DelegationTaskPhase::Active, None, None)
                .unwrap()
                .unwrap();
        assert_eq!(active.phase(), DelegationTaskPhase::Active);
        assert_eq!(active.status, AgentDelegationStatus::Running);

        // A round ending is not the task ending under `owner_review`.
        let awaiting = transition_delegation(
            &conn,
            &task.id,
            DelegationTaskPhase::AwaitingReview,
            Some("round finished"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(awaiting.phase(), DelegationTaskPhase::AwaitingReview);
        assert!(!awaiting.is_terminal());
        assert_eq!(awaiting.status, AgentDelegationStatus::Running);

        let completed = transition_delegation(
            &conn,
            &task.id,
            DelegationTaskPhase::Completed,
            Some("accepted"),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(completed.status, AgentDelegationStatus::Completed);
        assert_eq!(completed.phase(), DelegationTaskPhase::Completed);
        assert!(completed.finished_at_ms.is_some());

        // A terminal task is immutable: no later write moves it back.
        let after = transition_delegation(&conn, &task.id, DelegationTaskPhase::Active, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(after.phase(), DelegationTaskPhase::Completed);

        crate::tests_support::cleanup(path);
    }

    #[test]
    fn reusing_an_idempotency_key_with_another_payload_is_a_conflict() {
        let (mut conn, path) = open_test_db("idempotency");
        let parent = seed_session(&conn, "Parent");
        let now = unix_timestamp_ms();
        let mut first = AgentDelegation::single_turn_legacy(
            parent.id.clone(),
            "same-key",
            "First task",
            "Do the first thing",
            Some(parent.agent_id.clone()),
            AgentDelegationStatus::Starting,
            now,
        );
        first.payload_fingerprint = Some("fingerprint-a".to_string());
        assert!(matches!(
            crate::AgentDelegationRepository::reserve_or_get(&mut conn, &first, 8).unwrap(),
            crate::AgentDelegationReservation::Claimed(_)
        ));

        let mut retry = first.clone();
        retry.id = AgentDelegationId::new();
        assert!(matches!(
            crate::AgentDelegationRepository::reserve_or_get(&mut conn, &retry, 8).unwrap(),
            crate::AgentDelegationReservation::Existing(_)
        ));

        let mut different = first.clone();
        different.id = AgentDelegationId::new();
        different.payload_fingerprint = Some("fingerprint-b".to_string());
        let error =
            crate::AgentDelegationRepository::reserve_or_get(&mut conn, &different, 8).unwrap_err();
        assert_eq!(error.code, "idempotency_payload_conflict");

        crate::tests_support::cleanup(path);
    }

    #[test]
    fn a_concurrent_same_key_claim_resolves_to_the_winner_instead_of_failing() {
        let (conn, path) = open_test_db("operation-race");
        // Two connections stand in for two callers that both passed the
        // "have I seen this key" lookup before either insert landed. The unique
        // index is the arbiter, and the loser must still be told about the
        // operation it asked for.
        let mut second = crate::open_database(&path).unwrap();
        crate::apply_migrations(&mut second).unwrap();
        let operation = |fingerprint: &str| VibexUseOperation {
            id: VibexOperationId::new(),
            operation_ref: VibexUseRef::new(VibexUseResourceKind::Operation, "pending"),
            authority: "authority".to_string(),
            actor_key: "session_parent".to_string(),
            tool: "vibex_delegate".to_string(),
            caller_key: "race-001".to_string(),
            payload_fingerprint: fingerprint.to_string(),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: BTreeMap::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let winner = operation("same");
        let loser = operation("same");
        let first = VibexUseOperationRepository::reserve(&conn, &winner, "race-001").unwrap();
        let second_result =
            VibexUseOperationRepository::reserve(&second, &loser, "race-001").unwrap();
        let VibexUseOperationReservation::Claimed(claimed) = first else {
            panic!("the first caller should own the operation");
        };
        let VibexUseOperationReservation::Existing(existing) = second_result else {
            panic!("a concurrent retry must resolve to the original operation");
        };
        assert_eq!(existing.id, claimed.id);
        assert_eq!(existing.operation_ref, claimed.operation_ref);
        // A concurrent *different* payload is still a conflict, not a silent
        // reuse of somebody else's request.
        let mut third = crate::open_database(&path).unwrap();
        crate::apply_migrations(&mut third).unwrap();
        assert!(matches!(
            VibexUseOperationRepository::reserve(&third, &operation("other"), "race-001").unwrap(),
            VibexUseOperationReservation::Conflict(_)
        ));
    }

    #[test]
    fn a_cancelling_child_fences_its_whole_subtree() {
        let (mut conn, path) = open_test_db("fence");
        let parent = seed_session(&conn, "Parent");
        let child = seed_session(&conn, "Child");
        let grandchild = seed_session(&conn, "Grandchild");
        let sibling = seed_session(&conn, "Sibling");
        SessionOwnershipRepository::upsert(&conn, &child.id, &parent.id, None).unwrap();
        SessionOwnershipRepository::upsert(&conn, &grandchild.id, &child.id, None).unwrap();
        // `seed_task` already writes the ownership edge for the child.
        let task = seed_task(&mut conn, &parent, &child, "fence-task");
        transition_delegation(&conn, &task.id, DelegationTaskPhase::Cancelling, None, None)
            .unwrap();
        // The cancelling task covers the session it owns and everything under
        // it, and never a sibling branch.
        assert_eq!(
            delegation_cancellation_fence(&conn, &child.id).unwrap(),
            Some(task.id.clone())
        );
        assert_eq!(
            delegation_cancellation_fence(&conn, &grandchild.id).unwrap(),
            Some(task.id.clone())
        );
        assert_eq!(
            delegation_cancellation_fence(&conn, &parent.id).unwrap(),
            None
        );
        assert_eq!(
            delegation_cancellation_fence(&conn, &sibling.id).unwrap(),
            None
        );
        // Once the stop is confirmed the fence is gone.
        transition_delegation(&conn, &task.id, DelegationTaskPhase::Cancelled, None, None).unwrap();
        assert_eq!(
            delegation_cancellation_fence(&conn, &child.id).unwrap(),
            None
        );
        let _ = path;
    }

    #[test]
    fn an_operation_is_claimed_once_and_a_changed_payload_is_refused() {
        let (conn, path) = open_test_db("operation");
        let operation = |fingerprint: &str| VibexUseOperation {
            id: VibexOperationId::new(),
            operation_ref: VibexUseRef::new(VibexUseResourceKind::Operation, "pending"),
            authority: "authority".to_string(),
            actor_key: "session_parent".to_string(),
            tool: "vibex_delegate".to_string(),
            caller_key: "review-001".to_string(),
            payload_fingerprint: fingerprint.to_string(),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: BTreeMap::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let claimed = operation("aaa");
        assert!(matches!(
            VibexUseOperationRepository::reserve(&conn, &claimed, "review-001").unwrap(),
            VibexUseOperationReservation::Claimed(_)
        ));
        let retry = operation("aaa");
        assert!(matches!(
            VibexUseOperationRepository::reserve(&conn, &retry, "review-001").unwrap(),
            VibexUseOperationReservation::Existing(_)
        ));
        let changed = operation("bbb");
        assert!(matches!(
            VibexUseOperationRepository::reserve(&conn, &changed, "review-001").unwrap(),
            VibexUseOperationReservation::Conflict(_)
        ));

        // A produced resource is recorded once even when the operation is
        // retried, so an Agent never sees the same session listed twice.
        let reference = VibexUseRef::new(VibexUseResourceKind::Session, "session_worker");
        VibexUseOperationRepository::append_resource(
            &conn,
            &claimed.id,
            &VibexUseOperationResource {
                kind: "session".to_string(),
                reference: reference.clone(),
            },
        )
        .unwrap();
        let updated = VibexUseOperationRepository::append_resource(
            &conn,
            &claimed.id,
            &VibexUseOperationResource {
                kind: "session".to_string(),
                reference,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated.resources.len(), 1);

        crate::tests_support::cleanup(path);
    }

    #[test]
    fn ownership_edges_are_unique_and_deletion_ordered() {
        let (conn, path) = open_test_db("ownership");
        let root = seed_session(&conn, "Root");
        let child = seed_session(&conn, "Child");
        let grandchild = seed_session(&conn, "Grandchild");
        SessionOwnershipRepository::upsert(&conn, &child.id, &root.id, None).unwrap();
        SessionOwnershipRepository::upsert(&conn, &grandchild.id, &child.id, None).unwrap();
        assert_eq!(
            SessionOwnershipRepository::root_of(&conn, &grandchild.id).unwrap(),
            root.id
        );
        assert_eq!(
            SessionOwnershipRepository::ancestor_depth(&conn, &grandchild.id).unwrap(),
            2
        );

        let other = seed_session(&conn, "Other root");
        let error =
            SessionOwnershipRepository::upsert(&conn, &child.id, &other.id, None).unwrap_err();
        assert_eq!(error.code, "vibex_use_ownership_conflict");

        // Deepest first: a child is always removed before the session that owns
        // it, so no cascade delete can leave a dangling reference.
        let descendants = SessionOwnershipRepository::descendant_ids(&conn, &root.id).unwrap();
        assert_eq!(descendants, vec![grandchild.id.clone(), child.id.clone()]);

        crate::tests_support::cleanup(path);
    }

    /// Enqueues one real durable submission so an execution can link to it.
    fn seed_submission(
        conn: &mut DbConnection,
        session: &AgentSession,
        key: &str,
    ) -> MessageSubmissionId {
        let request = SendAgentMessageRequest {
            session_id: session.id.clone(),
            message_idempotency_key: key.to_string(),
            desired_runtime: SessionRuntimeSelection::provider(
                session.agent_id.clone(),
                vibex_core::ProviderProfileId::parse("provider_local").unwrap(),
                "model",
            ),
            text: "Do the work".to_string(),
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
            delivery: vibex_core::UserMessageDelivery::Prompt,
            prompt_context: None,
            provenance: vibex_core::MessageProvenance::HumanInput,
        };
        MessageSubmissionRepository::enqueue(conn, MessageSubmissionId::new(), &request)
            .unwrap()
            .submission_id
    }

    #[test]
    fn a_result_range_is_fixed_when_the_execution_settles() {
        let (mut conn, path) = open_test_db("execution");
        let parent = seed_session(&conn, "Parent");
        let child = seed_session(&conn, "Child");
        let task = seed_task(&mut conn, &parent, &child, "execution-task");
        let submission_id = seed_submission(&mut conn, &child, "delegation:first");
        let execution = DelegationExecution {
            id: VibexExecutionId::new(),
            execution_ref: VibexUseRef::new(VibexUseResourceKind::Execution, "pending"),
            task_ref: Some(VibexUseRef::task(&task.id)),
            session_ref: VibexUseRef::session(&child.id),
            submission_id,
            input_idempotency_key: "delegation:first".to_string(),
            provenance: MessageProvenance::LegacyUnknown,
            start_sequence: None,
            end_sequence: None,
            runtime_selection_revision: 0,
            outcome: ExecutionOutcome::Queued,
            error_code: None,
            stop_reason: None,
            summary: None,
            result_ranges: Vec::new(),
            artifact_refs: Vec::new(),
            usage: ExecutionUsageState::Unknown,
            truncated: false,
            blocked_on: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            finished_at_ms: None,
        };
        let execution = DelegationExecution {
            execution_ref: VibexUseRef::execution(&execution.id),
            ..execution
        };
        let (stored, created) =
            VibexUseExecutionRepository::insert_or_get(&conn, &execution).unwrap();
        assert!(created);
        // The same input key resolves to the same execution instead of starting
        // a parallel one.
        let (again, created) =
            VibexUseExecutionRepository::insert_or_get(&conn, &execution).unwrap();
        assert!(!created);
        assert_eq!(again.id, stored.id);

        let settled = VibexUseExecutionRepository::settle(
            &conn,
            &stored.id,
            ExecutionOutcome::Completed,
            Some("end_turn"),
            Some("Done"),
            &[ExecutionResultRange {
                start_sequence: 4,
                end_sequence: 9,
            }],
            &[],
            ExecutionUsageState::Unknown,
            false,
            Some(9),
            100,
        )
        .unwrap()
        .unwrap();
        assert!(settled.is_settled());
        assert_eq!(settled.result_ranges.len(), 1);

        // Settling again cannot move an already fixed window.
        let again = VibexUseExecutionRepository::settle(
            &conn,
            &stored.id,
            ExecutionOutcome::Completed,
            Some("end_turn"),
            Some("Done"),
            &[ExecutionResultRange {
                start_sequence: 20,
                end_sequence: 30,
            }],
            &[],
            ExecutionUsageState::Unknown,
            false,
            Some(30),
            200,
        )
        .unwrap()
        .unwrap();
        assert_eq!(again.result_ranges[0].start_sequence, 4);

        crate::tests_support::cleanup(path);
    }

    #[test]
    fn events_deliver_until_acknowledged_and_are_idempotent() {
        let (mut conn, path) = open_test_db("events");
        let root = seed_session(&conn, "Root");
        let child = seed_session(&conn, "Child");
        let task = seed_task(&mut conn, &root, &child, "event-task");
        let payload = serde_json::json!({ "taskRef": "vibex://task/x" });
        let first = VibexUseEventRepository::append(
            &conn,
            "event_one",
            Some(&root.id),
            DelegationTaskEventKind::TaskAccepted,
            Some(&task.id),
            Some(&child.id),
            1,
            &payload,
        )
        .unwrap();
        // Re-emitting the same event id after a failed live delivery does not
        // produce a duplicate.
        let again = VibexUseEventRepository::append(
            &conn,
            "event_one",
            Some(&root.id),
            DelegationTaskEventKind::TaskAccepted,
            Some(&task.id),
            Some(&child.id),
            1,
            &payload,
        )
        .unwrap();
        assert_eq!(again.cursor, first.cursor);

        let unread = VibexUseEventRepository::unacknowledged_for_root(
            &conn, "consumer", &root.id, 0, 10, false,
        )
        .unwrap();
        assert_eq!(unread.len(), 1);
        // It repeats until acknowledged.
        let unread = VibexUseEventRepository::unacknowledged_for_root(
            &conn, "consumer", &root.id, 0, 10, false,
        )
        .unwrap();
        assert_eq!(unread.len(), 1);

        VibexUseEventRepository::acknowledge(&conn, "consumer", &["event_one".to_string()])
            .unwrap();
        let unread = VibexUseEventRepository::unacknowledged_for_root(
            &conn, "consumer", &root.id, 0, 10, false,
        )
        .unwrap();
        assert!(unread.is_empty());

        crate::tests_support::cleanup(path);
    }

    #[test]
    fn a_session_controller_claim_is_a_compare_and_swap() {
        let (conn, path) = open_test_db("controller");
        let session = seed_session(&conn, "Worker");
        let owner = AgentDelegationId::new();
        let other = AgentDelegationId::new();
        let parent = VibexSessionId::new();

        assert!(matches!(
            SessionControllerRepository::claim(&conn, &session.id, &owner, &parent, None).unwrap(),
            SessionControllerClaim::Claimed(_)
        ));
        // A second task cannot take over silently.
        assert!(matches!(
            SessionControllerRepository::claim(&conn, &session.id, &other, &parent, None).unwrap(),
            SessionControllerClaim::Denied(_)
        ));
        // A stale expectation is refused instead of overwriting a newer one.
        assert!(matches!(
            SessionControllerRepository::claim(&conn, &session.id, &owner, &parent, Some(99))
                .unwrap(),
            SessionControllerClaim::StaleRevision(_)
        ));
        // A user takeover is respected and never reversed by an Agent.
        SessionControllerRepository::mark_human_controlled(&conn, &session.id).unwrap();
        assert!(matches!(
            SessionControllerRepository::claim(&conn, &session.id, &owner, &parent, None).unwrap(),
            SessionControllerClaim::HumanControlled(_)
        ));

        crate::tests_support::cleanup(path);
    }

    #[test]
    fn a_group_request_is_recognized_when_it_is_retried() {
        let (conn, path) = open_test_db("group");
        let workspace_ref = VibexUseRef::new(VibexUseResourceKind::Workspace, "workspace_a");
        let scope = SessionGroupScope::Workspace {
            workspace_ref: workspace_ref.clone(),
        };
        let layout = SessionGroupLayoutIntent::default();
        let members = vec![VibexSessionId::new()];
        let first = GroupPresentationRepository::reserve_or_get(
            &conn,
            None,
            "actor",
            "group_one",
            &scope,
            "Storage review",
            &members,
            &layout,
            "fingerprint",
        )
        .unwrap();
        assert!(matches!(first, GroupPresentationReservation::Claimed(_)));
        let retry = GroupPresentationRepository::reserve_or_get(
            &conn,
            None,
            "actor",
            "group_two",
            &scope,
            "Storage review",
            &members,
            &layout,
            "fingerprint",
        )
        .unwrap();
        match retry {
            GroupPresentationReservation::Existing(record) => {
                // The retry resolves to the original group instead of drawing a
                // second copy of the same team.
                assert_eq!(record.group_id, "group_one");
            }
            other => panic!("expected the original group, got {other:?}"),
        }

        let updated = GroupPresentationRepository::update(
            &conn,
            "group_one",
            Some(1),
            None,
            None,
            Some(&SessionGroupLayoutIntent {
                preset: vibex_core::SessionGroupLayoutPreset::Grid,
                lead_session_ref: None,
                preferred_live_panes: Some(2),
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated.revision, 2);
        // A stale expected revision is refused rather than overwriting a layout
        // the user has since changed.
        assert!(
            GroupPresentationRepository::update(&conn, "group_one", Some(1), None, None, None)
                .unwrap()
                .is_none()
        );

        crate::tests_support::cleanup(path);
    }
}

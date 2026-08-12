use rusqlite::{Connection, OptionalExtension, params};
use vibex_core::{
    AgentAuthContext, AgentAuthContextId, AgentAuthContextStatus, AgentAuthModelCatalogSnapshot,
    AgentAuthenticationOperation, AgentAuthenticationOperationId,
    AgentAuthenticationOperationState, AgentId, VibexError, VibexResult, VibexSessionId,
    unix_timestamp_ms,
};

use crate::{enum_from_db, enum_to_db, json_from_db, json_to_db, parse_id, storage_err};

pub struct AgentAuthContextRepository;

impl AgentAuthContextRepository {
    /// Returns the one durable default account context for an Agent, creating
    /// it when first needed. The UNIQUE(agent_id) constraint is the final
    /// concurrency fence against multiple account rows.
    pub fn ensure_default(conn: &Connection, agent_id: &AgentId) -> VibexResult<AgentAuthContext> {
        let now = unix_timestamp_ms();
        let context_id = AgentAuthContextId::new();
        conn.execute(
            "INSERT INTO agent_auth_contexts (
                auth_context_id, agent_id, status, revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'unverified', 1, ?3, ?3)
             ON CONFLICT(agent_id) DO NOTHING",
            params![context_id.as_str(), agent_id.as_str(), now],
        )
        .map_err(storage_err(
            "agent_auth_context_create_failed",
            "failed to create the Agent default authentication context",
        ))?;
        Self::get_by_agent(conn, agent_id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_auth_context_create_failed",
                "Agent default authentication context was not persisted",
            )
        })
    }

    pub fn get_by_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Option<AgentAuthContext>> {
        Self::query_one(conn, "agent_id = ?1", agent_id.as_str())
    }

    pub fn get_by_id(
        conn: &Connection,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<Option<AgentAuthContext>> {
        Self::query_one(conn, "auth_context_id = ?1", auth_context_id.as_str())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<AgentAuthContext>> {
        let mut statement = conn
            .prepare(&format!(
                "SELECT {} FROM agent_auth_contexts ORDER BY agent_id ASC",
                Self::COLUMNS
            ))
            .map_err(storage_err(
                "agent_auth_context_list_failed",
                "failed to list Agent authentication contexts",
            ))?;
        let rows = statement
            .query_map([], Self::read_row)
            .map_err(storage_err(
                "agent_auth_context_list_failed",
                "failed to list Agent authentication contexts",
            ))?;
        rows.map(|row| {
            row.map_err(storage_err(
                "agent_auth_context_list_failed",
                "failed to read an Agent authentication context",
            ))
            .and_then(Self::decode_row)
        })
        .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compare_and_set(
        conn: &Connection,
        auth_context_id: &AgentAuthContextId,
        expected_revision: i64,
        status: AgentAuthContextStatus,
        account_hint: Option<&str>,
        authenticated_via_method: Option<&str>,
        last_verified_at_ms: Option<i64>,
        increment_revision: bool,
    ) -> VibexResult<AgentAuthContext> {
        if expected_revision <= 0 {
            return Err(VibexError::validation(
                "agent_auth_context_revision_invalid",
                "Agent authentication context revision must be positive",
            ));
        }
        let next_revision = expected_revision + i64::from(increment_revision);
        let changed = conn
            .execute(
                "UPDATE agent_auth_contexts
                 SET status = ?3,
                     account_hint_redacted = ?4,
                     authenticated_via_method = ?5,
                     revision = ?6,
                     last_verified_at_ms = ?7,
                     updated_at_ms = ?8
                 WHERE auth_context_id = ?1 AND revision = ?2",
                params![
                    auth_context_id.as_str(),
                    expected_revision,
                    enum_to_db(&status)?,
                    account_hint,
                    authenticated_via_method,
                    next_revision,
                    last_verified_at_ms,
                    unix_timestamp_ms(),
                ],
            )
            .map_err(storage_err(
                "agent_auth_context_update_failed",
                "failed to update the Agent authentication context",
            ))?;
        if changed == 0 {
            return Err(VibexError::conflict(
                "agent_auth_context_revision_conflict",
                "Agent authentication context changed concurrently",
            ));
        }
        if increment_revision {
            AgentAuthModelCatalogRepository::delete_context(conn, auth_context_id)?;
        }
        Self::get_by_id(conn, auth_context_id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_auth_context_update_failed",
                "updated Agent authentication context could not be read",
            )
        })
    }

    pub fn delete_agent(conn: &Connection, agent_id: &AgentId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM agent_auth_contexts WHERE agent_id = ?1",
            params![agent_id.as_str()],
        )
        .map_err(storage_err(
            "agent_auth_context_delete_failed",
            "failed to delete the Agent authentication context",
        ))?;
        Ok(())
    }

    /// Returns every logical session whose desired/effective selection or
    /// historical binding references this account context. The query exposes
    /// only Vibex session ids; native ids and credential state stay private.
    pub fn referencing_session_ids(
        conn: &Connection,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<Vec<VibexSessionId>> {
        let mut statement = conn
            .prepare(
                "SELECT session_id FROM session_runtime_bindings
                 WHERE auth_source_kind = 'agent_account' AND auth_source_id = ?1
                 UNION
                 SELECT session_id FROM agent_sessions
                 WHERE (
                    json_extract(desired_runtime_selection_json, '$.authSource.kind') = 'agent_account'
                    AND json_extract(desired_runtime_selection_json, '$.authSource.authContextId') = ?1
                 ) OR (
                    json_extract(effective_runtime_selection_json, '$.authSource.kind') = 'agent_account'
                    AND json_extract(effective_runtime_selection_json, '$.authSource.authContextId') = ?1
                 )
                 ORDER BY session_id ASC",
            )
            .map_err(storage_err(
                "agent_auth_context_impact_query_failed",
                "failed to inspect sessions using the Agent account",
            ))?;
        let rows = statement
            .query_map(params![auth_context_id.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err(
                "agent_auth_context_impact_query_failed",
                "failed to inspect sessions using the Agent account",
            ))?;
        rows.map(|row| {
            row.map_err(storage_err(
                "agent_auth_context_impact_query_failed",
                "failed to read a session using the Agent account",
            ))
            .and_then(|session_id| parse_id(session_id, VibexSessionId::parse))
        })
        .collect()
    }

    const COLUMNS: &'static str = "
        auth_context_id, agent_id, status, account_hint_redacted,
        authenticated_via_method, revision, last_verified_at_ms,
        created_at_ms, updated_at_ms
    ";

    fn query_one(
        conn: &Connection,
        predicate: &str,
        value: &str,
    ) -> VibexResult<Option<AgentAuthContext>> {
        conn.query_row(
            &format!(
                "SELECT {} FROM agent_auth_contexts WHERE {predicate}",
                Self::COLUMNS
            ),
            params![value],
            Self::read_row,
        )
        .optional()
        .map_err(storage_err(
            "agent_auth_context_get_failed",
            "failed to read the Agent authentication context",
        ))?
        .map(Self::decode_row)
        .transpose()
    }

    fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAgentAuthContext> {
        Ok(RawAgentAuthContext {
            auth_context_id: row.get(0)?,
            agent_id: row.get(1)?,
            status: row.get(2)?,
            account_hint: row.get(3)?,
            authenticated_via_method: row.get(4)?,
            revision: row.get(5)?,
            last_verified_at_ms: row.get(6)?,
            created_at_ms: row.get(7)?,
            updated_at_ms: row.get(8)?,
        })
    }

    fn decode_row(raw: RawAgentAuthContext) -> VibexResult<AgentAuthContext> {
        Ok(AgentAuthContext {
            id: parse_id(raw.auth_context_id, AgentAuthContextId::parse)?,
            agent_id: parse_id(raw.agent_id, AgentId::parse)?,
            status: enum_from_db(raw.status)?,
            account_hint: raw.account_hint,
            authenticated_via_method: raw.authenticated_via_method,
            revision: raw.revision,
            last_verified_at_ms: raw.last_verified_at_ms,
            created_at_ms: raw.created_at_ms,
            updated_at_ms: raw.updated_at_ms,
        })
    }
}

struct RawAgentAuthContext {
    auth_context_id: String,
    agent_id: String,
    status: String,
    account_hint: Option<String>,
    authenticated_via_method: Option<String>,
    revision: i64,
    last_verified_at_ms: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

pub struct AgentAuthenticationOperationRepository;

impl AgentAuthenticationOperationRepository {
    pub fn get_active_for_context(
        conn: &Connection,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<Option<AgentAuthenticationOperation>> {
        let operation_id = conn
            .query_row(
                "SELECT operation_id
                 FROM agent_authentication_operations
                 WHERE auth_context_id = ?1 AND state IN (
                    'queued', 'discovering_methods', 'authenticating', 'awaiting_user',
                    'verifying', 'cancelling'
                 )
                 LIMIT 1",
                params![auth_context_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err(
                "agent_authentication_operation_get_failed",
                "failed to read the active Agent authentication operation",
            ))?
            .map(|value| parse_id(value, AgentAuthenticationOperationId::parse))
            .transpose()?;
        operation_id
            .as_ref()
            .map(|operation_id| Self::get(conn, operation_id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn insert(conn: &Connection, operation: &AgentAuthenticationOperation) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO agent_authentication_operations (
                operation_id, auth_context_id, expected_context_revision,
                method_id, state, error_code, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                operation.operation_id.as_str(),
                operation.auth_context_id.as_str(),
                operation.expected_context_revision,
                operation.method_id,
                enum_to_db(&operation.state)?,
                operation.error_code,
                operation.created_at_ms,
                operation.updated_at_ms,
            ],
        )
        .map_err(|error| {
            if error
                .to_string()
                .contains("idx_agent_authentication_operations_active")
            {
                VibexError::conflict(
                    "agent_authentication_operation_in_progress",
                    "another authentication operation is already active for this Agent",
                )
            } else {
                storage_err(
                    "agent_authentication_operation_insert_failed",
                    "failed to persist the Agent authentication operation",
                )(error)
            }
        })?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        operation_id: &AgentAuthenticationOperationId,
    ) -> VibexResult<Option<AgentAuthenticationOperation>> {
        conn.query_row(
            "SELECT auth_context_id, expected_context_revision, method_id, state,
                    error_code, created_at_ms, updated_at_ms
             FROM agent_authentication_operations WHERE operation_id = ?1",
            params![operation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(storage_err(
            "agent_authentication_operation_get_failed",
            "failed to read the Agent authentication operation",
        ))?
        .map(|raw| {
            Ok(AgentAuthenticationOperation {
                operation_id: operation_id.clone(),
                auth_context_id: parse_id(raw.0, AgentAuthContextId::parse)?,
                expected_context_revision: raw.1,
                method_id: raw.2,
                state: enum_from_db(raw.3)?,
                error_code: raw.4,
                created_at_ms: raw.5,
                updated_at_ms: raw.6,
            })
        })
        .transpose()
    }

    pub fn update_state(
        conn: &Connection,
        operation_id: &AgentAuthenticationOperationId,
        expected: AgentAuthenticationOperationState,
        next: AgentAuthenticationOperationState,
        error_code: Option<&str>,
    ) -> VibexResult<()> {
        let changed = conn
            .execute(
                "UPDATE agent_authentication_operations
                 SET state = ?3, error_code = ?4, updated_at_ms = ?5
                 WHERE operation_id = ?1 AND state = ?2",
                params![
                    operation_id.as_str(),
                    enum_to_db(&expected)?,
                    enum_to_db(&next)?,
                    error_code,
                    unix_timestamp_ms(),
                ],
            )
            .map_err(storage_err(
                "agent_authentication_operation_update_failed",
                "failed to update the Agent authentication operation",
            ))?;
        if changed == 0 {
            return Err(VibexError::conflict(
                "agent_authentication_operation_state_conflict",
                "Agent authentication operation state changed concurrently",
            ));
        }
        Ok(())
    }

    pub fn cancel_incomplete_on_startup(conn: &Connection) -> VibexResult<usize> {
        conn.execute(
            "UPDATE agent_authentication_operations
             SET state = 'cancelled', error_code = 'application_restarted', updated_at_ms = ?1
             WHERE state IN (
                'queued', 'discovering_methods', 'authenticating', 'awaiting_user',
                'verifying', 'cancelling'
             )",
            params![unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "agent_authentication_operation_reconcile_failed",
            "failed to reconcile Agent authentication operations",
        ))
    }
}

pub struct AgentAuthModelCatalogRepository;

impl AgentAuthModelCatalogRepository {
    pub fn upsert(conn: &Connection, snapshot: &AgentAuthModelCatalogSnapshot) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO agent_auth_model_catalog_snapshots (
                auth_context_id, auth_context_revision, runtime_fingerprint,
                discovery_source, status, catalog_json, last_success_at_ms,
                last_attempt_at_ms, last_error_code
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(auth_context_id, auth_context_revision, runtime_fingerprint)
             DO UPDATE SET
                discovery_source = excluded.discovery_source,
                status = excluded.status,
                catalog_json = excluded.catalog_json,
                last_success_at_ms = excluded.last_success_at_ms,
                last_attempt_at_ms = excluded.last_attempt_at_ms,
                last_error_code = excluded.last_error_code",
            params![
                snapshot.auth_context_id.as_str(),
                snapshot.auth_context_revision,
                snapshot.runtime_fingerprint,
                enum_to_db(&snapshot.discovery_source)?,
                enum_to_db(&snapshot.status)?,
                json_to_db(&snapshot.models)?,
                snapshot.last_success_at_ms,
                snapshot.last_attempt_at_ms,
                snapshot.last_error_code,
            ],
        )
        .map_err(storage_err(
            "agent_auth_model_catalog_upsert_failed",
            "failed to persist the Agent account model catalog",
        ))?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        auth_context_id: &AgentAuthContextId,
        auth_context_revision: i64,
        runtime_fingerprint: &str,
    ) -> VibexResult<Option<AgentAuthModelCatalogSnapshot>> {
        conn.query_row(
            "SELECT discovery_source, status, catalog_json, last_success_at_ms,
                    last_attempt_at_ms, last_error_code
             FROM agent_auth_model_catalog_snapshots
             WHERE auth_context_id = ?1 AND auth_context_revision = ?2
               AND runtime_fingerprint = ?3",
            params![
                auth_context_id.as_str(),
                auth_context_revision,
                runtime_fingerprint
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()
        .map_err(storage_err(
            "agent_auth_model_catalog_get_failed",
            "failed to read the Agent account model catalog",
        ))?
        .map(|raw| {
            Ok(AgentAuthModelCatalogSnapshot {
                auth_context_id: auth_context_id.clone(),
                auth_context_revision,
                runtime_fingerprint: runtime_fingerprint.to_string(),
                discovery_source: enum_from_db(raw.0)?,
                status: enum_from_db(raw.1)?,
                models: json_from_db(raw.2)?,
                last_success_at_ms: raw.3,
                last_attempt_at_ms: raw.4,
                last_error_code: raw.5,
            })
        })
        .transpose()
    }

    pub fn list_current(
        conn: &Connection,
        contexts: &[AgentAuthContext],
    ) -> VibexResult<Vec<AgentAuthModelCatalogSnapshot>> {
        let mut snapshots = Vec::new();
        for context in contexts {
            let mut statement = conn
                .prepare(
                    "SELECT runtime_fingerprint, discovery_source, status, catalog_json,
                            last_success_at_ms, last_attempt_at_ms, last_error_code
                     FROM agent_auth_model_catalog_snapshots
                     WHERE auth_context_id = ?1 AND auth_context_revision = ?2
                     ORDER BY last_attempt_at_ms DESC",
                )
                .map_err(storage_err(
                    "agent_auth_model_catalog_list_failed",
                    "failed to list Agent account model catalogs",
                ))?;
            let rows = statement
                .query_map(params![context.id.as_str(), context.revision], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                })
                .map_err(storage_err(
                    "agent_auth_model_catalog_list_failed",
                    "failed to list Agent account model catalogs",
                ))?;
            for row in rows {
                let raw = row.map_err(storage_err(
                    "agent_auth_model_catalog_list_failed",
                    "failed to read an Agent account model catalog",
                ))?;
                snapshots.push(AgentAuthModelCatalogSnapshot {
                    auth_context_id: context.id.clone(),
                    auth_context_revision: context.revision,
                    runtime_fingerprint: raw.0,
                    discovery_source: enum_from_db(raw.1)?,
                    status: enum_from_db(raw.2)?,
                    models: json_from_db(raw.3)?,
                    last_success_at_ms: raw.4,
                    last_attempt_at_ms: raw.5,
                    last_error_code: raw.6,
                });
            }
        }
        Ok(snapshots)
    }

    pub fn delete_context(
        conn: &Connection,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM agent_auth_model_catalog_snapshots WHERE auth_context_id = ?1",
            params![auth_context_id.as_str()],
        )
        .map_err(storage_err(
            "agent_auth_model_catalog_delete_failed",
            "failed to invalidate the Agent account model catalog",
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentAuthModelCatalogStatus, AgentModelDiscoverySource};

    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::apply_migrations(&mut connection).unwrap();
        connection
    }

    #[test]
    fn one_default_context_is_reused_and_the_database_rejects_a_second_account() {
        let connection = migrated_connection();
        let agent_id = AgentId::parse("codex").unwrap();

        let first = AgentAuthContextRepository::ensure_default(&connection, &agent_id).unwrap();
        let second = AgentAuthContextRepository::ensure_default(&connection, &agent_id).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(
            AgentAuthContextRepository::list(&connection).unwrap(),
            vec![first]
        );
        let duplicate = connection.execute(
            "INSERT INTO agent_auth_contexts (
                auth_context_id, agent_id, status, revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'unverified', 1, 1, 1)",
            params![AgentAuthContextId::new().as_str(), agent_id.as_str()],
        );
        assert!(
            duplicate.is_err(),
            "UNIQUE(agent_id) must reject a second account"
        );
    }

    #[test]
    fn revision_change_invalidates_models_and_stale_writers_fail_closed() {
        let connection = migrated_connection();
        let context = AgentAuthContextRepository::ensure_default(
            &connection,
            &AgentId::parse("codex").unwrap(),
        )
        .unwrap();
        AgentAuthModelCatalogRepository::upsert(
            &connection,
            &AgentAuthModelCatalogSnapshot {
                auth_context_id: context.id.clone(),
                auth_context_revision: context.revision,
                runtime_fingerprint: "runtime-v1".to_string(),
                discovery_source: AgentModelDiscoverySource::AgentDefault,
                status: AgentAuthModelCatalogStatus::AgentDefaultOnly,
                models: Vec::new(),
                last_success_at_ms: Some(1),
                last_attempt_at_ms: 1,
                last_error_code: None,
            },
        )
        .unwrap();

        let updated = AgentAuthContextRepository::compare_and_set(
            &connection,
            &context.id,
            context.revision,
            AgentAuthContextStatus::Authenticated,
            Some("work account"),
            Some("browser"),
            Some(2),
            true,
        )
        .unwrap();
        assert_eq!(updated.revision, context.revision + 1);
        assert!(
            AgentAuthModelCatalogRepository::get(
                &connection,
                &context.id,
                context.revision,
                "runtime-v1",
            )
            .unwrap()
            .is_none()
        );

        let stale = AgentAuthContextRepository::compare_and_set(
            &connection,
            &context.id,
            context.revision,
            AgentAuthContextStatus::Unavailable,
            None,
            None,
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(stale.code, "agent_auth_context_revision_conflict");
    }

    #[test]
    fn active_authentication_operation_query_tracks_only_incomplete_states() {
        let connection = migrated_connection();
        let context = AgentAuthContextRepository::ensure_default(
            &connection,
            &AgentId::parse("codex").unwrap(),
        )
        .unwrap();
        let operation = AgentAuthenticationOperation {
            operation_id: AgentAuthenticationOperationId::new(),
            auth_context_id: context.id.clone(),
            expected_context_revision: context.revision,
            method_id: "browser".to_string(),
            state: AgentAuthenticationOperationState::Queued,
            error_code: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        AgentAuthenticationOperationRepository::insert(&connection, &operation).unwrap();

        assert_eq!(
            AgentAuthenticationOperationRepository::get_active_for_context(
                &connection,
                &context.id,
            )
            .unwrap(),
            Some(operation.clone())
        );

        AgentAuthenticationOperationRepository::update_state(
            &connection,
            &operation.operation_id,
            AgentAuthenticationOperationState::Queued,
            AgentAuthenticationOperationState::Succeeded,
            None,
        )
        .unwrap();
        assert!(
            AgentAuthenticationOperationRepository::get_active_for_context(
                &connection,
                &context.id,
            )
            .unwrap()
            .is_none()
        );
    }
}

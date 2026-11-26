//! Persistence support for cross-instance session cloning.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use vibex_core::{
    AgentSession, ClonedTimelineEntry, CorrelationId, RuntimeBinding, SessionRuntimeSelection,
    TimelineItem, TurnExecutionAttribution, VibexError, VibexResult, VibexSessionId,
    unix_timestamp_ms,
};

use crate::{
    RuntimeBindingRepository, SessionRepository, append_timeline_in_transaction, enum_from_db_sql,
    json_from_db, json_from_db_sql, json_to_db, migration_applied, parse_optional_id_sql,
    storage_err,
};

/// Creates `cloned_session_runtime_hints` so newly imported sessions retain
/// their mapped target `SessionRuntimeSelection` without eagerly spawning an
/// ACP subprocess during bulk import.
pub(crate) fn apply_cloned_session_runtime_hints(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 91;
    const NAME: &str = "cloned_session_runtime_hints";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start cloned session runtime hints migration",
    ))?;
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS cloned_session_runtime_hints (
            session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
            runtime_selection_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        "#,
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to apply cloned session runtime hints migration",
    ))?;
    let already_recorded = migration_applied(&tx, VERSION)?;
    if !already_recorded {
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![VERSION, NAME, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "migration_record_failed",
            "failed to record cloned session runtime hints migration",
        ))?;
    }
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit cloned session runtime hints migration",
    ))?;
    if !already_recorded {
        applied.push(format!("{VERSION}:{NAME}"));
    }
    Ok(())
}

pub struct SessionCloneRepository;

impl SessionCloneRepository {
    /// Exports durable timeline entries for `session_id` in ascending sequence
    /// order, preserving full `TurnExecutionAttribution` metadata.
    pub fn export_timeline_entries(
        conn: &Connection,
        session_id: &VibexSessionId,
        max_items: usize,
    ) -> VibexResult<Vec<ClonedTimelineEntry>> {
        let mut stmt = conn
            .prepare_cached(
                "
                SELECT source, timestamp_ms, correlation_id, provider_correlation_id,
                    payload_json, redaction_state, execution_attribution_json
                FROM agent_timeline_items
                WHERE session_id = ?1
                ORDER BY sequence ASC
                ",
            )
            .map_err(storage_err(
                "session_clone_timeline_prepare_failed",
                "failed to prepare session clone timeline export query",
            ))?;
        let rows = stmt
            .query_map(params![session_id.as_str()], |row| {
                let execution_attribution = row
                    .get::<_, Option<String>>(6)?
                    .map(json_from_db_sql::<TurnExecutionAttribution>)
                    .transpose()?;
                Ok(ClonedTimelineEntry {
                    source: enum_from_db_sql(row.get(0)?)?,
                    created_at_ms: row.get(1)?,
                    correlation_id: parse_optional_id_sql(
                        row.get::<_, Option<String>>(2)?,
                        CorrelationId::parse,
                    )?,
                    provider_correlation_id: row.get(3)?,
                    payload: json_from_db_sql(row.get(4)?)?,
                    redaction_state: enum_from_db_sql(row.get(5)?)?,
                    execution_attribution,
                })
            })
            .map_err(storage_err(
                "session_clone_timeline_query_failed",
                "failed to query timeline items for session clone export",
            ))?;

        let mut entries = Vec::new();
        for row in rows {
            if entries.len() >= max_items {
                return Err(VibexError::validation(
                    "session_clone_timeline_too_large",
                    format!("session timeline exceeds maximum cloneable item count ({max_items})"),
                )
                .with_diagnostic("sessionId", session_id.as_str())
                .with_diagnostic("maxItems", max_items.to_string()));
            }
            entries.push(row.map_err(storage_err(
                "session_clone_timeline_decode_failed",
                "failed to decode timeline row for session clone export",
            ))?);
        }
        Ok(entries)
    }

    /// Atomically inserts a cloned session, locks its title without overwriting
    /// `updated_at_ms`, appends all timeline items (`1..=N`), records an
    /// optional preferred `SessionRuntimeSelection` hint, and optionally seeds
    /// an inactive `RuntimeBinding` for native resume on the first turn.
    pub fn insert_cloned_session(
        conn: &mut Connection,
        session: &AgentSession,
        timeline: &[ClonedTimelineEntry],
        mapped_selection: Option<&SessionRuntimeSelection>,
        seeded_binding: Option<&RuntimeBinding>,
    ) -> VibexResult<Vec<TimelineItem>> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_err(
                "session_clone_transaction_failed",
                "failed to start session clone import transaction",
            ))?;

        SessionRepository::insert(&tx, session)?;
        tx.execute(
            "UPDATE agent_sessions SET title_locked = 1 WHERE session_id = ?1",
            params![session.id.as_str()],
        )
        .map_err(storage_err(
            "session_clone_lock_title_failed",
            "failed to lock cloned session title",
        ))?;

        let mut inserted_items = Vec::with_capacity(timeline.len());
        for entry in timeline {
            inserted_items.push(append_timeline_in_transaction(
                &tx,
                &session.id,
                entry.source,
                entry.payload.clone(),
                Some(entry.created_at_ms),
                entry.correlation_id.as_ref(),
                entry.provider_correlation_id.as_deref(),
                entry.redaction_state,
                entry.execution_attribution.as_ref(),
            )?);
        }

        if let Some(selection) = mapped_selection {
            Self::upsert_runtime_hint(&tx, &session.id, selection)?;
        }

        if let Some(binding) = seeded_binding {
            RuntimeBindingRepository::insert(&tx, binding)?;
        }

        tx.commit().map_err(storage_err(
            "session_clone_commit_failed",
            "failed to commit session clone import transaction",
        ))?;

        Ok(inserted_items)
    }

    /// Stores or updates the preferred `SessionRuntimeSelection` hint for an
    /// imported session that has not yet spawned its live runtime binding.
    pub fn upsert_runtime_hint(
        conn: &Connection,
        session_id: &VibexSessionId,
        selection: &SessionRuntimeSelection,
    ) -> VibexResult<()> {
        let json = json_to_db(selection)?;
        conn.execute(
            "
            INSERT INTO cloned_session_runtime_hints (
                session_id, runtime_selection_json, created_at_ms
            )
            VALUES (?1, ?2, ?3)
            ON CONFLICT(session_id) DO UPDATE SET
                runtime_selection_json = excluded.runtime_selection_json,
                created_at_ms = excluded.created_at_ms
            ",
            params![session_id.as_str(), json, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "session_clone_hint_upsert_failed",
            "failed to persist cloned session runtime selection hint",
        ))?;
        Ok(())
    }

    /// Loads the preferred `SessionRuntimeSelection` hint for `session_id`, if
    /// one was persisted during import.
    pub fn get_runtime_hint(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<SessionRuntimeSelection>> {
        let raw = conn
            .query_row(
                "
                SELECT runtime_selection_json
                FROM cloned_session_runtime_hints
                WHERE session_id = ?1
                ",
                params![session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err(
                "session_clone_hint_lookup_failed",
                "failed to load cloned session runtime selection hint",
            ))?;
        raw.map(json_from_db).transpose()
    }

    /// Deletes the preferred `SessionRuntimeSelection` hint once the session's
    /// authoritative runtime selection has been initialized.
    pub fn delete_runtime_hint(conn: &Connection, session_id: &VibexSessionId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM cloned_session_runtime_hints WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .map_err(storage_err(
            "session_clone_hint_delete_failed",
            "failed to delete cloned session runtime selection hint",
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use rusqlite::params;
    use vibex_core::{
        AcpAdapterId, AgentId, AgentMessagePayload, AgentSession, AgentSessionSafety,
        AgentSessionState, BindingState, CLONED_SESSION_RESUME_IDENTITY, ClonedTimelineEntry,
        NativeStateHomeId, PermissionMode, ProviderProfileId, RuntimeAuthSource, RuntimeBinding,
        RuntimeBindingId, RuntimeModelSelection, SessionRuntimeConfigState,
        SessionRuntimeSelection, SessionToolSettings, SessionTransport, TimelinePayload,
        TimelineRedactionState, TimelineSource, TransportKind, UserMessagePayload, VibexSessionId,
        WorkspaceMode,
    };

    use super::SessionCloneRepository;
    use crate::{
        CURRENT_SCHEMA_VERSION, RuntimeBindingRepository, SessionRepository, TimelineRepository,
        WorkspaceRepository, apply_migrations, current_schema_version, open_database,
        tests_support::cleanup,
    };

    fn temp_db_path(label: &str) -> PathBuf {
        let base = std::env::temp_dir();
        let _ = std::fs::create_dir_all(&base);
        base.join(format!(
            "vibex-session-clone-db-{label}-{}.sqlite",
            VibexSessionId::new().as_str()
        ))
    }

    #[test]
    fn insert_and_export_cloned_session_roundtrips_timeline_hint_and_binding() {
        let db_path = temp_db_path("roundtrip");
        let mut conn = open_database(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("migrations");
        assert_eq!(
            current_schema_version(&conn).expect("schema version"),
            CURRENT_SCHEMA_VERSION
        );

        let workspace_dir =
            std::env::temp_dir().join(format!("vibex-clone-ws-{}", VibexSessionId::new().as_str()));
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_dir, WorkspaceMode::CurrentCheckout)
                .expect("ensure workspace");

        let session_id = VibexSessionId::new();
        let claude = AgentId::parse("claude").expect("agent id");
        let session = AgentSession {
            id: session_id.clone(),
            title: "Cloned cache-warm session".to_string(),
            project_id: project.id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: claude.clone(),
            transport: SessionTransport::Acp,
            terminal: None,
            tools: SessionToolSettings::default(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety {
                permission_mode: PermissionMode::WorkspaceWrite,
                ask_on_risk: true,
                bypass_all_permissions: false,
            },
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_050_000,
            last_message_at_ms: 1_700_000_020_000,
            archived_at_ms: None,
            deleted_at_ms: None,
        };

        let entries = vec![
            ClonedTimelineEntry {
                source: TimelineSource::User,
                created_at_ms: 1_700_000_010_000,
                correlation_id: None,
                provider_correlation_id: None,
                payload: TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Hello instance A".to_string(),
                    ..Default::default()
                }),
                redaction_state: TimelineRedactionState::None,
                execution_attribution: None,
            },
            ClonedTimelineEntry {
                source: TimelineSource::Agent,
                created_at_ms: 1_700_000_020_000,
                correlation_id: None,
                provider_correlation_id: Some("msg_1".to_string()),
                payload: TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "Cache-aligned reply".to_string(),
                    is_final: true,
                }),
                redaction_state: TimelineRedactionState::None,
                execution_attribution: None,
            },
        ];

        let profile_id = ProviderProfileId::parse("provider_claude_1").expect("profile id");
        let selection = SessionRuntimeSelection {
            agent_id: claude.clone(),
            auth_source: RuntimeAuthSource::provider_profile(profile_id),
            model: RuntimeModelSelection::explicit("claude-sonnet-4-5"),
            reasoning_effort: Some("high".to_string()),
            mode_id: None,
            config_values: BTreeMap::new(),
        };

        let binding = RuntimeBinding {
            binding_id: RuntimeBindingId::new(),
            session_id: session_id.clone(),
            agent_id: claude,
            transport_kind: TransportKind::Acp,
            adapter_id: AcpAdapterId::parse("claude").expect("adapter id"),
            adapter_version: "cloned".to_string(),
            adapter_compatibility_identity: "claude:acp".to_string(),
            auth_source: selection.auth_source.clone(),
            auth_source_revision: 1,
            native_session_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            native_state_home_id: NativeStateHomeId::new(),
            provider_resume_identity: Some(CLONED_SESSION_RESUME_IDENTITY.to_string()),
            process_spawn_fingerprint: "cloned".to_string(),
            session_runtime_config_state: SessionRuntimeConfigState::default(),
            capability_snapshot: None,
            restore_compatibility_key: None,
            last_context_sequence: 2,
            last_summary_sequence: 0,
            context_bridge_version: 1,
            activation_generation: 0,
            binding_state: BindingState::Inactive,
            created_by_switch_id: None,
            created_at_ms: 1_700_000_050_000,
            updated_at_ms: 1_700_000_050_000,
        };

        let inserted = SessionCloneRepository::insert_cloned_session(
            &mut conn,
            &session,
            &entries,
            Some(&selection),
            Some(&binding),
        )
        .expect("insert cloned session");
        assert_eq!(inserted.len(), 2);
        assert_eq!(inserted[0].sequence, 1);
        assert_eq!(inserted[1].sequence, 2);

        let loaded = SessionRepository::get(&conn, &session_id)
            .expect("get session")
            .expect("session exists");
        assert_eq!(loaded.updated_at_ms, 1_700_000_050_000);
        let title_locked: i64 = conn
            .query_row(
                "SELECT title_locked FROM agent_sessions WHERE session_id = ?1",
                params![session_id.as_str()],
                |row| row.get(0),
            )
            .expect("query title_locked");
        assert_eq!(title_locked, 1, "cloned session title must be locked");

        let exported = SessionCloneRepository::export_timeline_entries(&conn, &session_id, 100)
            .expect("export timeline");
        assert_eq!(exported, entries);
        assert_eq!(
            TimelineRepository::latest_sequence(&conn, &session_id).expect("latest seq"),
            2
        );

        let hint = SessionCloneRepository::get_runtime_hint(&conn, &session_id)
            .expect("get hint")
            .expect("hint exists");
        assert_eq!(hint, selection);

        let bindings =
            RuntimeBindingRepository::list_by_session(&conn, &session_id).expect("list bindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0].provider_resume_identity.as_deref(),
            Some(CLONED_SESSION_RESUME_IDENTITY)
        );
        assert_eq!(bindings[0].last_context_sequence, 2);

        SessionCloneRepository::delete_runtime_hint(&conn, &session_id).expect("delete hint");
        assert!(
            SessionCloneRepository::get_runtime_hint(&conn, &session_id)
                .expect("get hint after delete")
                .is_none()
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(workspace_dir);
        cleanup(db_path);
    }
}

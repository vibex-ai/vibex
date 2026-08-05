//! Durable, display-safe Agent runtime probe records.
//!
//! Probe execution is owned by the ACP runtime.  This repository only stores
//! the bounded state machine and redacted evidence, so a crash can be resumed
//! or cancelled without retaining process output, credentials, prompts, or
//! native session handles.

use rusqlite::{Connection, OptionalExtension, params};
use vibex_core::{
    AgentModelProviderBindingId, AgentRuntimeProbeId, AgentRuntimeProbeRecord,
    AgentRuntimeProbeStage, AgentRuntimeProbeStatus, AgentRuntimeProfileId, VibexError,
    VibexResult,
};

use crate::{enum_to_db, json_from_db_sql, json_to_db, storage_err};

pub struct AgentRuntimeProbeRepository;

impl AgentRuntimeProbeRepository {
    pub fn insert(conn: &Connection, record: &AgentRuntimeProbeRecord) -> VibexResult<()> {
        record.validate()?;
        conn.execute(
            "
            INSERT INTO agent_runtime_provider_probes (
                probe_id, agent_runtime_profile_id,
                agent_model_provider_binding_id, agent_id, adapter_id,
                descriptor_id, descriptor_version, status, stage, record_json,
                cancel_requested, revision, created_at_ms, updated_at_ms,
                finished_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ",
            params![
                record.id.as_str(),
                record.request.runtime_profile_id.as_str(),
                record
                    .request
                    .binding_id
                    .as_ref()
                    .map(AgentModelProviderBindingId::as_str),
                record.agent_id.as_str(),
                record.adapter_id.as_str(),
                record.descriptor_id.as_str(),
                record.descriptor_version,
                enum_to_db(&record.status)?,
                enum_to_db(&record.stage)?,
                json_to_db(record)?,
                bool_to_db(record.cancel_requested),
                record.revision,
                record.created_at_ms,
                record.updated_at_ms,
                record.finished_at_ms,
            ],
        )
        .map_err(storage_err(
            "agent_runtime_probe_insert_failed",
            "failed to persist Agent runtime probe",
        ))?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        id: &AgentRuntimeProbeId,
    ) -> VibexResult<Option<AgentRuntimeProbeRecord>> {
        conn.query_row(
            "SELECT record_json FROM agent_runtime_provider_probes WHERE probe_id = ?1",
            params![id.as_str()],
            |row| json_from_db_sql(row.get(0)?),
        )
        .optional()
        .map_err(storage_err(
            "agent_runtime_probe_lookup_failed",
            "failed to load Agent runtime probe",
        ))
    }

    pub fn list(
        conn: &Connection,
        runtime_profile_id: Option<&AgentRuntimeProfileId>,
        limit: usize,
    ) -> VibexResult<Vec<AgentRuntimeProbeRecord>> {
        let limit = i64::try_from(limit.clamp(1, 500)).unwrap_or(500);
        let rows = if let Some(runtime_profile_id) = runtime_profile_id {
            let mut statement = conn
                .prepare(
                    "SELECT record_json FROM agent_runtime_provider_probes
                     WHERE agent_runtime_profile_id = ?1
                     ORDER BY updated_at_ms DESC LIMIT ?2",
                )
                .map_err(storage_err(
                    "agent_runtime_probe_list_failed",
                    "failed to list Agent runtime probes",
                ))?;
            let mapped = statement
                .query_map(params![runtime_profile_id.as_str(), limit], |row| {
                    json_from_db_sql(row.get(0)?)
                })
                .map_err(storage_err(
                    "agent_runtime_probe_list_failed",
                    "failed to list Agent runtime probes",
                ))?;
            mapped.collect::<Result<Vec<_>, _>>().map_err(storage_err(
                "agent_runtime_probe_decode_failed",
                "failed to decode Agent runtime probe",
            ))?
        } else {
            let mut statement = conn
                .prepare(
                    "SELECT record_json FROM agent_runtime_provider_probes
                     ORDER BY updated_at_ms DESC LIMIT ?1",
                )
                .map_err(storage_err(
                    "agent_runtime_probe_list_failed",
                    "failed to list Agent runtime probes",
                ))?;
            let mapped = statement
                .query_map(params![limit], |row| json_from_db_sql(row.get(0)?))
                .map_err(storage_err(
                    "agent_runtime_probe_list_failed",
                    "failed to list Agent runtime probes",
                ))?;
            mapped.collect::<Result<Vec<_>, _>>().map_err(storage_err(
                "agent_runtime_probe_decode_failed",
                "failed to decode Agent runtime probe",
            ))?
        };
        Ok(rows)
    }

    pub fn latest_for_runtime(
        conn: &Connection,
        runtime_profile_id: &AgentRuntimeProfileId,
    ) -> VibexResult<Option<AgentRuntimeProbeRecord>> {
        Ok(Self::list(conn, Some(runtime_profile_id), 1)?
            .into_iter()
            .next())
    }

    pub fn latest_for_binding(
        conn: &Connection,
        binding_id: &AgentModelProviderBindingId,
    ) -> VibexResult<Option<AgentRuntimeProbeRecord>> {
        conn.query_row(
            "SELECT record_json FROM agent_runtime_provider_probes
             WHERE agent_model_provider_binding_id = ?1
             ORDER BY updated_at_ms DESC LIMIT 1",
            params![binding_id.as_str()],
            |row| json_from_db_sql(row.get(0)?),
        )
        .optional()
        .map_err(storage_err(
            "agent_runtime_probe_binding_lookup_failed",
            "failed to load the latest Agent runtime probe for a provider binding",
        ))
    }

    pub fn list_non_terminal(conn: &Connection) -> VibexResult<Vec<AgentRuntimeProbeRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT record_json FROM agent_runtime_provider_probes
                 WHERE status IN ('requested', 'running')
                 ORDER BY created_at_ms ASC, probe_id ASC",
            )
            .map_err(storage_err(
                "agent_runtime_probe_recovery_list_failed",
                "failed to list recoverable Agent runtime probes",
            ))?;
        let rows = statement
            .query_map([], |row| json_from_db_sql(row.get(0)?))
            .map_err(storage_err(
                "agent_runtime_probe_recovery_list_failed",
                "failed to list recoverable Agent runtime probes",
            ))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(storage_err(
            "agent_runtime_probe_recovery_decode_failed",
            "failed to decode recoverable Agent runtime probes",
        ))
    }

    /// Return an interrupted probe to its durable request boundary. Probe
    /// execution is isolated and has no resumable native identity, so startup
    /// recovery must clean the old root and replay from the first stage.
    pub fn reset_for_startup(
        conn: &Connection,
        id: &AgentRuntimeProbeId,
        expected_revision: i64,
        now_ms: i64,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        let mut record = Self::get(conn, id)?.ok_or_else(|| {
            VibexError::validation(
                "agent_runtime_probe_not_found",
                "Agent runtime probe was not found during startup recovery",
            )
        })?;
        if record.revision != expected_revision {
            return Err(VibexError::conflict(
                "agent_runtime_probe_revision_conflict",
                "Agent runtime probe changed during startup recovery",
            ));
        }
        if record.is_terminal() {
            return Ok(record);
        }

        record.facts.clear();
        record.evidence = None;
        record.updated_at_ms = now_ms.max(record.updated_at_ms);
        record.revision = record.revision.saturating_add(1).max(1);
        if record.cancel_requested {
            record.status = AgentRuntimeProbeStatus::Cancelled;
            record.stage = AgentRuntimeProbeStage::Completed;
            record.diagnostic_code = Some("probe_cancelled".to_string());
            record.finished_at_ms = Some(record.updated_at_ms);
        } else {
            record.status = AgentRuntimeProbeStatus::Requested;
            record.stage = AgentRuntimeProbeStage::Requested;
            record.diagnostic_code = Some("probe_recovered_after_restart".to_string());
            record.finished_at_ms = None;
        }
        Self::update(conn, &record, expected_revision)
    }

    pub fn update(
        conn: &Connection,
        record: &AgentRuntimeProbeRecord,
        expected_revision: i64,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        record.validate()?;
        let changed = conn
            .execute(
                "
                UPDATE agent_runtime_provider_probes
                SET status = ?2, stage = ?3, record_json = ?4,
                    cancel_requested = ?5, revision = ?6,
                    updated_at_ms = ?7, finished_at_ms = ?8
                WHERE probe_id = ?1 AND revision = ?9
                ",
                params![
                    record.id.as_str(),
                    enum_to_db(&record.status)?,
                    enum_to_db(&record.stage)?,
                    json_to_db(record)?,
                    bool_to_db(record.cancel_requested),
                    record.revision,
                    record.updated_at_ms,
                    record.finished_at_ms,
                    expected_revision,
                ],
            )
            .map_err(storage_err(
                "agent_runtime_probe_update_failed",
                "failed to update Agent runtime probe",
            ))?;
        if changed != 1 {
            return Err(VibexError::conflict(
                "agent_runtime_probe_revision_conflict",
                "Agent runtime probe changed since it was loaded",
            ));
        }
        Self::get(conn, &record.id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_runtime_probe_readback_failed",
                "Agent runtime probe disappeared after update",
            )
        })
    }

    pub fn claim_requested(
        conn: &Connection,
        id: &AgentRuntimeProbeId,
        now_ms: i64,
    ) -> VibexResult<Option<AgentRuntimeProbeRecord>> {
        let Some(mut record) = Self::get(conn, id)? else {
            return Ok(None);
        };
        if record.status != AgentRuntimeProbeStatus::Requested || record.cancel_requested {
            return Ok(Some(record));
        }
        let expected = record.revision;
        record.status = AgentRuntimeProbeStatus::Running;
        record.stage = AgentRuntimeProbeStage::ResolvingIdentity;
        record.updated_at_ms = now_ms.max(record.updated_at_ms);
        record.revision = record.revision.saturating_add(1).max(1);
        Ok(Some(Self::update(conn, &record, expected)?))
    }

    pub fn request_cancel(
        conn: &Connection,
        id: &AgentRuntimeProbeId,
        expected_revision: i64,
        now_ms: i64,
    ) -> VibexResult<AgentRuntimeProbeRecord> {
        let mut record = Self::get(conn, id)?.ok_or_else(|| {
            VibexError::validation(
                "agent_runtime_probe_not_found",
                "Agent runtime probe was not found",
            )
        })?;
        if record.revision != expected_revision {
            return Err(VibexError::conflict(
                "agent_runtime_probe_revision_conflict",
                "Agent runtime probe changed since it was loaded",
            ));
        }
        if record.is_terminal() {
            return Ok(record);
        }
        record.set_cancel_requested(now_ms);
        if record.status == AgentRuntimeProbeStatus::Requested {
            record.status = AgentRuntimeProbeStatus::Cancelled;
            record.stage = AgentRuntimeProbeStage::Completed;
            record.diagnostic_code = Some("probe_cancelled".to_string());
            record.finished_at_ms = Some(now_ms.max(record.updated_at_ms));
        }
        Self::update(conn, &record, expected_revision)
    }
}

fn bool_to_db(value: bool) -> i64 {
    i64::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use vibex_core::{
        AcpAdapterId, AgentId, AgentProviderProjectionDescriptorId, AgentRuntimeProbeRequest,
        AgentRuntimeProbeStage, AgentRuntimeProbeStatus, AgentRuntimeProfileId,
    };

    fn connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::apply_migrations(&mut connection).unwrap();
        connection
    }

    fn runtime_profile(conn: &Connection) -> AgentRuntimeProfileId {
        let id = AgentRuntimeProfileId::new();
        conn.execute(
            "INSERT INTO agent_runtime_profiles (
                agent_runtime_profile_id, legacy_provider_profile_id, agent_id, adapter_id,
                version_identity_json, command, args_json, safe_env_references_json,
                cwd_template, process_strategy, runtime_home_strategy,
                host_capabilities_json, resource_policy_json, revision,
                created_at_ms, updated_at_ms, deleted_at_ms
            ) VALUES (?1, NULL, ?2, ?3, ?4, 'fixture', '[]', '[]', NULL,
                'per_session', 'vibex_private', '{}', '{}', 1, 1, 1, NULL)",
            params![id.as_str(), "fixture", "fixture-acp", "{}"],
        )
        .unwrap();
        id
    }

    fn record(conn: &Connection) -> AgentRuntimeProbeRecord {
        let runtime_profile_id = runtime_profile(conn);
        AgentRuntimeProbeRecord::requested(
            AgentRuntimeProbeId::new(),
            AgentRuntimeProbeRequest {
                runtime_profile_id,
                binding_id: None,
                workspace_key: "probe-fixture".to_string(),
                timeout_ms: 5_000,
                minimal_prompt: false,
            },
            AgentId::parse("fixture").unwrap(),
            AcpAdapterId::parse("fixture-acp").unwrap(),
            AgentProviderProjectionDescriptorId::parse("projection_fixture_v1").unwrap(),
            "1".to_string(),
            10,
        )
        .unwrap()
    }

    #[test]
    fn probe_repository_round_trips_and_claims_once() {
        let conn = connection();
        let record = record(&conn);
        AgentRuntimeProbeRepository::insert(&conn, &record).unwrap();
        let loaded = AgentRuntimeProbeRepository::get(&conn, &record.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, record);
        let claimed = AgentRuntimeProbeRepository::claim_requested(&conn, &record.id, 20)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.status, AgentRuntimeProbeStatus::Running);
        assert_eq!(claimed.stage, AgentRuntimeProbeStage::ResolvingIdentity);
        let again = AgentRuntimeProbeRepository::claim_requested(&conn, &record.id, 30)
            .unwrap()
            .unwrap();
        assert_eq!(again.revision, claimed.revision);
    }

    #[test]
    fn probe_repository_rejects_stale_cancel_and_is_idempotent_after_terminal() {
        let conn = connection();
        let record = record(&conn);
        AgentRuntimeProbeRepository::insert(&conn, &record).unwrap();
        let error =
            AgentRuntimeProbeRepository::request_cancel(&conn, &record.id, 99, 20).unwrap_err();
        assert_eq!(error.code, "agent_runtime_probe_revision_conflict");
        let cancelled =
            AgentRuntimeProbeRepository::request_cancel(&conn, &record.id, 1, 20).unwrap();
        assert!(cancelled.cancel_requested);
        assert_eq!(cancelled.status, AgentRuntimeProbeStatus::Cancelled);
        assert_eq!(cancelled.stage, AgentRuntimeProbeStage::Completed);
        assert_eq!(cancelled.finished_at_ms, Some(20));
        let result =
            AgentRuntimeProbeRepository::request_cancel(&conn, &record.id, cancelled.revision, 40)
                .unwrap();
        assert_eq!(result.status, AgentRuntimeProbeStatus::Cancelled);
    }

    #[test]
    fn startup_recovery_requeues_running_probe_and_finishes_cancelled_request() {
        let conn = connection();
        let running = record(&conn);
        AgentRuntimeProbeRepository::insert(&conn, &running).unwrap();
        let running = AgentRuntimeProbeRepository::claim_requested(&conn, &running.id, 20)
            .unwrap()
            .unwrap();
        assert_eq!(
            AgentRuntimeProbeRepository::list_non_terminal(&conn)
                .unwrap()
                .len(),
            1
        );
        let recovered = AgentRuntimeProbeRepository::reset_for_startup(
            &conn,
            &running.id,
            running.revision,
            30,
        )
        .unwrap();
        assert_eq!(recovered.status, AgentRuntimeProbeStatus::Requested);
        assert_eq!(recovered.stage, AgentRuntimeProbeStage::Requested);
        assert_eq!(
            recovered.diagnostic_code.as_deref(),
            Some("probe_recovered_after_restart")
        );

        let cancelled = record(&conn);
        AgentRuntimeProbeRepository::insert(&conn, &cancelled).unwrap();
        let cancelled = AgentRuntimeProbeRepository::claim_requested(&conn, &cancelled.id, 31)
            .unwrap()
            .unwrap();
        let cancelled = AgentRuntimeProbeRepository::request_cancel(
            &conn,
            &cancelled.id,
            cancelled.revision,
            32,
        )
        .unwrap();
        assert_eq!(cancelled.status, AgentRuntimeProbeStatus::Running);
        assert!(cancelled.cancel_requested);
        let recovered = AgentRuntimeProbeRepository::reset_for_startup(
            &conn,
            &cancelled.id,
            cancelled.revision,
            40,
        )
        .unwrap();
        assert_eq!(recovered.status, AgentRuntimeProbeStatus::Cancelled);
        assert_eq!(recovered.finished_at_ms, Some(40));
    }
}

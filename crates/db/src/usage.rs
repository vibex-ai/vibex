use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use vibex_core::{
    AgentAuthContextId, AgentId, AgentTurnUsageFact, AgentUsageCounterOrigin, AgentUsageCoverage,
    AgentUsageExecution, AgentUsageExecutionStatus, AgentUsageExecutionStatusUpdate,
    AgentUsageObservation, AgentUsageReportedFields, AgentUsageTokenValues, MessageSubmissionId,
    ProjectId, ProviderProfileId, RuntimeAuthSource, RuntimeAuthSourceKind, RuntimeBindingId,
    UsageExecutionId, VibexError, VibexResult, VibexSessionId, WorkspaceId,
};

use crate::{enum_from_db, enum_to_db, storage_err};

pub const MAX_AGENT_USAGE_QUERY_ROWS: usize = 100_000;

const INPUT_REPORTED: i64 = 1 << 0;
const OUTPUT_REPORTED: i64 = 1 << 1;
const THOUGHT_REPORTED: i64 = 1 << 2;
const CACHED_READ_REPORTED: i64 = 1 << 3;
const CACHED_WRITE_REPORTED: i64 = 1 << 4;
const TOTAL_REPORTED: i64 = 1 << 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentUsageApplyOutcome {
    pub fact_changed: bool,
    pub checkpoint_changed: bool,
    pub reset_epoch_started: bool,
    pub ignored_stale_observation: bool,
}

impl AgentUsageApplyOutcome {
    pub fn changed(self) -> bool {
        self.fact_changed || self.checkpoint_changed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentUsageFactProjection {
    pub fact: AgentTurnUsageFact,
    pub agent_label: String,
    pub project_label: String,
    pub auth_source_label: String,
    pub session_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentUsageCheckpoint {
    usage_stream_id: String,
    session_id: VibexSessionId,
    binding_id: RuntimeBindingId,
    last_activation_generation: i64,
    agent_id: AgentId,
    auth_source: RuntimeAuthSource,
    auth_source_revision: i64,
    last_model_id: Option<String>,
    reset_epoch: i64,
    counter_origin: AgentUsageCounterOrigin,
    cumulative: AgentUsageTokenValues,
    last_usage_execution_id: Option<UsageExecutionId>,
    last_observation_sequence: u64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

pub struct AgentUsageRepository;

impl AgentUsageRepository {
    pub fn record_execution(
        conn: &Connection,
        execution: &AgentUsageExecution,
    ) -> VibexResult<bool> {
        validate_execution(execution)?;
        record_execution_on_conn(conn, execution)
    }

    pub fn record_execution_status(
        conn: &Connection,
        update: &AgentUsageExecutionStatusUpdate,
    ) -> VibexResult<bool> {
        validate_execution(&update.execution)?;
        if update.status == AgentUsageExecutionStatus::Dispatched
            || update.completed_at_ms < update.execution.dispatched_at_ms
        {
            return Err(VibexError::validation(
                "agent_usage_status_invalid",
                "Agent usage execution status update is invalid",
            ));
        }
        let tx = conn.unchecked_transaction().map_err(storage_err(
            "agent_usage_status_transaction_failed",
            "failed to start Agent usage status transaction",
        ))?;
        let inserted = record_execution_on_conn(&tx, &update.execution)?;
        let fact = get_fact(&tx, &update.execution.usage_execution_id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_usage_fact_missing",
                "Agent usage execution fact was not found after insertion",
            )
        })?;
        if fact.execution_status != AgentUsageExecutionStatus::Dispatched {
            tx.commit().map_err(storage_err(
                "agent_usage_status_transaction_commit_failed",
                "failed to commit Agent usage status transaction",
            ))?;
            return Ok(inserted);
        }
        let changed = tx
            .execute(
                "
                UPDATE agent_turn_usage_facts
                SET execution_status = ?2,
                    coverage = ?3,
                    completed_at_ms = ?4,
                    updated_at_ms = ?4
                WHERE usage_execution_id = ?1
                  AND execution_status = ?5
                ",
                params![
                    update.execution.usage_execution_id.as_str(),
                    enum_to_db(&update.status)?,
                    enum_to_db(&fact.coverage)?,
                    update.completed_at_ms,
                    enum_to_db(&AgentUsageExecutionStatus::Dispatched)?,
                ],
            )
            .map_err(storage_err(
                "agent_usage_status_update_failed",
                "failed to update Agent usage execution status",
            ))?;
        tx.commit().map_err(storage_err(
            "agent_usage_status_transaction_commit_failed",
            "failed to commit Agent usage status transaction",
        ))?;
        Ok(inserted || changed == 1)
    }

    pub fn apply_observation(
        conn: &mut Connection,
        observation: &AgentUsageObservation,
    ) -> VibexResult<AgentUsageApplyOutcome> {
        validate_observation(observation)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_err(
                "agent_usage_transaction_failed",
                "failed to start Agent usage transaction",
            ))?;

        let mut outcome = AgentUsageApplyOutcome::default();
        let mut fact = match observation.execution.as_ref() {
            Some(execution) => {
                outcome.fact_changed = record_execution_on_conn(&tx, execution)?;
                get_fact(&tx, &execution.usage_execution_id)?.ok_or_else(|| {
                    VibexError::storage(
                        "agent_usage_fact_missing",
                        "Agent usage execution fact was not found after insertion",
                    )
                })?
            }
            None => {
                // Context-only resume observations remain live-only. Advancing a
                // cumulative checkpoint without an exact execution could consume
                // a later turn's delta, so durable accounting fails closed.
                tx.commit().map_err(storage_err(
                    "agent_usage_transaction_commit_failed",
                    "failed to commit Agent usage transaction",
                ))?;
                return Ok(outcome);
            }
        };

        let stream_id = observation.stream.binding_id.as_str().to_string();
        let checkpoint = get_checkpoint(&tx, &stream_id)?;
        if let Some(current) = checkpoint.as_ref() {
            if observation.stream.activation_generation < current.last_activation_generation {
                outcome.ignored_stale_observation = true;
                tx.commit().map_err(storage_err(
                    "agent_usage_transaction_commit_failed",
                    "failed to commit Agent usage transaction",
                ))?;
                return Ok(outcome);
            }
            if observation.stream.activation_generation == current.last_activation_generation
                && observation.observation_sequence <= current.last_observation_sequence
            {
                outcome.ignored_stale_observation = true;
                tx.commit().map_err(storage_err(
                    "agent_usage_transaction_commit_failed",
                    "failed to commit Agent usage transaction",
                ))?;
                return Ok(outcome);
            }
            if let (Some(last_execution_id), Some(execution)) = (
                current.last_usage_execution_id.as_ref(),
                observation.execution.as_ref(),
            ) && last_execution_id != &execution.usage_execution_id
                && let Some(last_fact) = get_fact(&tx, last_execution_id)?
                && execution_precedes_fact(&tx, execution, &last_fact)?
            {
                outcome.ignored_stale_observation = true;
                tx.commit().map_err(storage_err(
                    "agent_usage_transaction_commit_failed",
                    "failed to commit Agent usage transaction",
                ))?;
                return Ok(outcome);
            }
        }

        let same_execution = checkpoint.as_ref().is_some_and(|current| {
            current.last_usage_execution_id.as_ref()
                == observation
                    .execution
                    .as_ref()
                    .map(|execution| &execution.usage_execution_id)
        });
        let has_prior_execution = match observation.execution.as_ref() {
            Some(execution) => has_prior_execution_on_stream(&tx, execution)?,
            None => false,
        };
        let regression = checkpoint
            .as_ref()
            .is_some_and(|current| tokens_regress(&observation.cumulative, &current.cumulative));
        let accepted_cumulative = if same_execution {
            checkpoint
                .as_ref()
                .map(|current| {
                    without_regressed_fields(&observation.cumulative, &current.cumulative)
                })
                .unwrap_or_else(|| observation.cumulative.clone())
        } else {
            observation.cumulative.clone()
        };
        let incoming_fields = AgentUsageReportedFields::from_tokens(&accepted_cumulative);
        let has_cumulative = accepted_cumulative.any_reported();
        let now = observation.observed_at_ms;
        let stream_counter_origin = checkpoint
            .as_ref()
            .map(|current| current.counter_origin)
            .unwrap_or(observation.counter_origin);
        let zero_baseline_claimed = match observation.execution.as_ref() {
            Some(execution) => binding_claimed_zero_baseline(&tx, execution)?,
            None => false,
        };
        let zero_baseline_available = stream_counter_origin == AgentUsageCounterOrigin::KnownZero
            && !has_prior_execution
            && zero_baseline_claimed;
        let reset_epoch_started = regression && !same_execution;
        let next_reset_epoch = checkpoint
            .as_ref()
            .map(|current| current.reset_epoch)
            .unwrap_or(0)
            .saturating_add(i64::from(reset_epoch_started));
        let previous = checkpoint
            .as_ref()
            .map(|current| current.cumulative.clone())
            .unwrap_or_default();
        let preserve_unknown_baseline = if same_execution {
            unknown_baseline_fields(&fact)
        } else {
            AgentUsageReportedFields::default()
        };
        let (next_cumulative, delta, baseline_only) = if !has_cumulative {
            (
                previous.clone(),
                AgentUsageTokenValues::default(),
                preserve_unknown_baseline.any(),
            )
        } else if reset_epoch_started {
            (
                observation.cumulative.clone(),
                AgentUsageTokenValues::default(),
                true,
            )
        } else {
            calculate_delta(
                &previous,
                &accepted_cumulative,
                zero_baseline_available,
                preserve_unknown_baseline,
            )?
        };

        fact.reset_epoch = next_reset_epoch;
        fact.delta = add_token_values(&fact.delta, &delta)?;
        fact.cumulative_after = merge_cumulative(&fact.cumulative_after, &accepted_cumulative);
        fact.reported_fields.merge(incoming_fields);
        fact.last_source = Some(observation.source);
        fact.last_observed_at_ms = Some(observation.observed_at_ms);
        fact.updated_at_ms = observation.observed_at_ms;
        if reset_epoch_started {
            fact.reset_reason = Some("counter_regression".to_string());
        }
        apply_context_observation(&mut fact, observation);
        fact.coverage = fact_coverage(&fact, baseline_only || reset_epoch_started);
        update_fact(&tx, &fact)?;
        outcome.fact_changed = true;
        outcome.reset_epoch_started = reset_epoch_started;

        if has_cumulative || checkpoint.is_some() {
            let created_at_ms = checkpoint
                .as_ref()
                .map(|current| current.created_at_ms)
                .unwrap_or(now);
            let last_usage_execution_id = if has_cumulative {
                observation
                    .execution
                    .as_ref()
                    .map(|execution| execution.usage_execution_id.clone())
            } else {
                checkpoint
                    .as_ref()
                    .and_then(|current| current.last_usage_execution_id.clone())
            };
            let next = AgentUsageCheckpoint {
                usage_stream_id: stream_id,
                session_id: observation.stream.session_id.clone(),
                binding_id: observation.stream.binding_id.clone(),
                last_activation_generation: observation.stream.activation_generation,
                agent_id: observation.stream.agent_id.clone(),
                auth_source: observation.stream.auth_source.clone(),
                auth_source_revision: observation.stream.auth_source_revision,
                last_model_id: observation.stream.model_id.clone(),
                reset_epoch: next_reset_epoch,
                counter_origin: if reset_epoch_started {
                    AgentUsageCounterOrigin::Unknown
                } else {
                    stream_counter_origin
                },
                cumulative: next_cumulative,
                last_usage_execution_id,
                last_observation_sequence: observation.observation_sequence,
                created_at_ms,
                updated_at_ms: now,
            };
            upsert_checkpoint(&tx, &next)?;
            outcome.checkpoint_changed = true;
        }

        tx.commit().map_err(storage_err(
            "agent_usage_transaction_commit_failed",
            "failed to commit Agent usage transaction",
        ))?;
        Ok(outcome)
    }

    pub fn get_fact(
        conn: &Connection,
        usage_execution_id: &UsageExecutionId,
    ) -> VibexResult<Option<AgentTurnUsageFact>> {
        get_fact(conn, usage_execution_id)
    }

    pub fn list_facts_in_range(
        conn: &Connection,
        start_at_ms: i64,
        end_at_ms: i64,
        limit: usize,
    ) -> VibexResult<Vec<AgentUsageFactProjection>> {
        if start_at_ms < 0 || end_at_ms <= start_at_ms || limit == 0 {
            return Err(VibexError::validation(
                "agent_usage_query_range_invalid",
                "Agent usage query range must be positive and ordered",
            ));
        }
        let limit = limit.min(MAX_AGENT_USAGE_QUERY_ROWS.saturating_add(1));
        let mut statement = conn
            .prepare(&format!(
                "
                SELECT {},
                       COALESCE(NULLIF(a.label_override, ''), f.agent_id),
                       COALESCE(NULLIF(p.name, ''), f.project_id),
                       CASE f.auth_source_kind
                           WHEN 'provider_profile' THEN
                               COALESCE(NULLIF(pp.display_name, ''), f.auth_source_id)
                           WHEN 'agent_account' THEN
                               COALESCE(
                                   NULLIF(ac.account_hint_redacted, ''),
                                   COALESCE(NULLIF(a.label_override, ''), f.agent_id)
                               )
                           ELSE f.auth_source_id
                       END,
                       COALESCE(NULLIF(s.title, ''), f.session_id)
                FROM agent_turn_usage_facts f
                LEFT JOIN agent_configs a ON a.agent_id = f.agent_id
                LEFT JOIN projects p ON p.project_id = f.project_id
                LEFT JOIN provider_profiles pp
                    ON pp.provider_profile_id = f.provider_profile_id
                LEFT JOIN agent_auth_contexts ac
                    ON ac.auth_context_id = f.auth_source_id
                   AND f.auth_source_kind = 'agent_account'
                LEFT JOIN agent_sessions s ON s.session_id = f.session_id
                WHERE f.dispatched_at_ms >= ?1 AND f.dispatched_at_ms < ?2
                ORDER BY f.dispatched_at_ms ASC, f.usage_execution_id ASC
                LIMIT ?3
                ",
                fact_columns("f")
            ))
            .map_err(storage_err(
                "agent_usage_query_prepare_failed",
                "failed to prepare Agent usage query",
            ))?;
        let rows = statement
            .query_map(params![start_at_ms, end_at_ms, limit as i64], |row| {
                Ok((
                    read_raw_fact(row)?,
                    row.get::<_, String>(38)?,
                    row.get::<_, String>(39)?,
                    row.get::<_, String>(40)?,
                    row.get::<_, String>(41)?,
                ))
            })
            .map_err(storage_err(
                "agent_usage_query_failed",
                "failed to query Agent usage facts",
            ))?;
        rows.map(|row| {
            let (raw, agent_label, project_label, auth_source_label, session_label) =
                row.map_err(storage_err(
                    "agent_usage_query_row_failed",
                    "failed to read Agent usage fact row",
                ))?;
            Ok(AgentUsageFactProjection {
                fact: decode_raw_fact(raw)?,
                agent_label,
                project_label,
                auth_source_label,
                session_label,
            })
        })
        .collect()
    }

    pub fn count_facts(conn: &Connection) -> VibexResult<u64> {
        let count = conn
            .query_row("SELECT COUNT(*) FROM agent_turn_usage_facts", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(storage_err(
                "agent_usage_count_failed",
                "failed to count Agent usage facts",
            ))?;
        u64::try_from(count).map_err(|_| {
            VibexError::storage(
                "agent_usage_count_invalid",
                "Agent usage fact count was invalid",
            )
        })
    }
}

fn decode_usage_auth_source(
    kind: String,
    source_id: String,
    legacy_provider_profile_id: Option<String>,
) -> VibexResult<RuntimeAuthSource> {
    match enum_from_db::<RuntimeAuthSourceKind>(kind)? {
        RuntimeAuthSourceKind::ProviderProfile => {
            if legacy_provider_profile_id.as_deref() != Some(source_id.as_str()) {
                return Err(VibexError::storage(
                    "agent_usage_auth_source_legacy_mismatch",
                    "Provider usage attribution does not match its compatibility column",
                ));
            }
            Ok(RuntimeAuthSource::provider_profile(
                ProviderProfileId::parse(source_id)?,
            ))
        }
        RuntimeAuthSourceKind::AgentAccount => {
            if legacy_provider_profile_id.is_some() {
                return Err(VibexError::storage(
                    "agent_usage_auth_source_legacy_mismatch",
                    "Agent account usage attribution must not contain a Provider Profile",
                ));
            }
            Ok(RuntimeAuthSource::agent_account(AgentAuthContextId::parse(
                source_id,
            )?))
        }
    }
}

fn validate_execution(execution: &AgentUsageExecution) -> VibexResult<()> {
    if execution.stream.activation_generation < 0
        || execution.stream.auth_source_revision < 0
        || execution.dispatched_at_ms < 0
        || execution.stream.model_id.as_ref().is_some_and(|model_id| {
            model_id.trim().is_empty()
                || model_id.len() > 512
                || model_id.chars().any(char::is_control)
        })
        || (execution.stream.auth_source.provider_profile_id().is_some()
            && execution.stream.model_id.is_none())
    {
        return Err(VibexError::validation(
            "agent_usage_execution_invalid",
            "Agent usage execution attribution is invalid",
        ));
    }
    Ok(())
}

fn validate_observation(observation: &AgentUsageObservation) -> VibexResult<()> {
    if !observation.validate() || observation.observation_sequence == 0 {
        return Err(VibexError::validation(
            "agent_usage_observation_invalid",
            "Agent usage observation is invalid",
        ));
    }
    Ok(())
}

fn record_execution_on_conn(
    conn: &Connection,
    execution: &AgentUsageExecution,
) -> VibexResult<bool> {
    validate_execution(execution)?;
    if let Some(existing) = get_fact(conn, &execution.usage_execution_id)? {
        if !execution_matches_fact(execution, &existing) {
            return Err(VibexError::conflict(
                "agent_usage_execution_conflict",
                "Agent usage execution id belongs to different attribution",
            ));
        }
        return Ok(false);
    }
    conn.execute(
        "
        INSERT INTO agent_turn_usage_facts (
            usage_execution_id, message_submission_id, session_id, project_id, workspace_id,
            binding_id, activation_generation, reset_epoch, agent_id, provider_profile_id,
            auth_source_kind, auth_source_id, auth_source_revision, model_id, execution_status,
            input_delta, output_delta, thought_delta,
            cached_read_delta, cached_write_delta, total_delta, cumulative_input_after,
            cumulative_output_after, cumulative_thought_after, cumulative_cached_read_after,
            cumulative_cached_write_after, cumulative_total_after, context_window_used_tokens,
            context_window_size_tokens, reported_fields, coverage, last_source, reset_reason,
            dispatched_at_ms, completed_at_ms, last_observed_at_ms, created_at_ms, updated_at_ms
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, NULL, 0, ?15, NULL, NULL, ?16, NULL, NULL, ?16, ?16
        )
        ",
        params![
            execution.usage_execution_id.as_str(),
            execution
                .message_submission_id
                .as_ref()
                .map(MessageSubmissionId::as_str),
            execution.stream.session_id.as_str(),
            execution.project_id.as_str(),
            execution.workspace_id.as_str(),
            execution.stream.binding_id.as_str(),
            execution.stream.activation_generation,
            execution.stream.agent_id.as_str(),
            execution
                .stream
                .auth_source
                .provider_profile_id()
                .map(ProviderProfileId::as_str),
            enum_to_db(&execution.stream.auth_source.kind())?,
            execution.stream.auth_source.id(),
            execution.stream.auth_source_revision,
            execution.stream.model_id,
            enum_to_db(&AgentUsageExecutionStatus::Dispatched)?,
            enum_to_db(&AgentUsageCoverage::Unreported)?,
            execution.dispatched_at_ms,
        ],
    )
    .map_err(storage_err(
        "agent_usage_execution_insert_failed",
        "failed to insert Agent usage execution fact",
    ))?;
    Ok(true)
}

fn execution_matches_fact(execution: &AgentUsageExecution, fact: &AgentTurnUsageFact) -> bool {
    execution.usage_execution_id == fact.usage_execution_id
        && execution.message_submission_id == fact.message_submission_id
        && execution.stream.session_id == fact.session_id
        && execution.project_id == fact.project_id
        && execution.workspace_id == fact.workspace_id
        && execution.stream.binding_id == fact.binding_id
        && execution.stream.activation_generation == fact.activation_generation
        && execution.stream.agent_id == fact.agent_id
        && execution.stream.auth_source == fact.auth_source
        && execution.stream.auth_source_revision == fact.auth_source_revision
        && execution.stream.model_id == fact.model_id
}

fn execution_precedes_fact(
    conn: &Connection,
    execution: &AgentUsageExecution,
    other: &AgentTurnUsageFact,
) -> VibexResult<bool> {
    if execution.dispatched_at_ms != other.dispatched_at_ms {
        return Ok(execution.dispatched_at_ms < other.dispatched_at_ms);
    }
    Ok(fact_rowid(conn, &execution.usage_execution_id)?
        < fact_rowid(conn, &other.usage_execution_id)?)
}

fn fact_rowid(conn: &Connection, usage_execution_id: &UsageExecutionId) -> VibexResult<i64> {
    conn.query_row(
        "SELECT rowid FROM agent_turn_usage_facts WHERE usage_execution_id = ?1",
        params![usage_execution_id.as_str()],
        |row| row.get(0),
    )
    .map_err(storage_err(
        "agent_usage_fact_order_query_failed",
        "failed to inspect Agent usage execution order",
    ))
}

fn has_prior_execution_on_stream(
    conn: &Connection,
    execution: &AgentUsageExecution,
) -> VibexResult<bool> {
    conn.query_row(
        "
        SELECT EXISTS (
            SELECT 1
            FROM agent_turn_usage_facts
            WHERE binding_id = ?1
              AND usage_execution_id <> ?2
              AND dispatched_at_ms <= ?3
        )
        ",
        params![
            execution.stream.binding_id.as_str(),
            execution.usage_execution_id.as_str(),
            execution.dispatched_at_ms,
        ],
        |row| row.get::<_, bool>(0),
    )
    .map_err(storage_err(
        "agent_usage_prior_execution_query_failed",
        "failed to inspect prior Agent usage executions",
    ))
}

fn binding_claimed_zero_baseline(
    conn: &Connection,
    execution: &AgentUsageExecution,
) -> VibexResult<bool> {
    conn.query_row(
        "
            SELECT EXISTS (
                SELECT 1
                FROM session_runtime_bindings binding
                WHERE binding.binding_id = ?1
                  AND binding.usage_zero_baseline_state = 'claimed'
                  AND binding.usage_zero_baseline_execution_id = ?2
                  AND binding.usage_zero_baseline_activation_generation = ?3
            )
        ",
        params![
            execution.stream.binding_id.as_str(),
            execution.usage_execution_id.as_str(),
            execution.stream.activation_generation,
        ],
        |row| row.get::<_, bool>(0),
    )
    .map_err(storage_err(
        "agent_usage_binding_origin_query_failed",
        "failed to verify the Agent usage zero-baseline claim",
    ))
}

fn apply_context_observation(fact: &mut AgentTurnUsageFact, observation: &AgentUsageObservation) {
    if observation.context_window_used_tokens.is_some() {
        fact.context_window_used_tokens = observation.context_window_used_tokens;
    }
    if observation.context_window_size_tokens.is_some() {
        fact.context_window_size_tokens = observation.context_window_size_tokens;
    }
    fact.last_source = Some(observation.source);
    fact.last_observed_at_ms = Some(observation.observed_at_ms);
    fact.updated_at_ms = observation.observed_at_ms;
}

fn fact_coverage(fact: &AgentTurnUsageFact, baseline_only: bool) -> AgentUsageCoverage {
    if baseline_only && !fact.delta.any_reported() {
        AgentUsageCoverage::BaselineOnly
    } else if fact.reported_fields.all()
        && fact.delta.values().into_iter().all(|value| value.is_some())
    {
        AgentUsageCoverage::Complete
    } else if fact.reported_fields.any() || fact.delta.any_reported() {
        AgentUsageCoverage::Partial
    } else {
        fact.coverage
    }
}

fn unknown_baseline_fields(fact: &AgentTurnUsageFact) -> AgentUsageReportedFields {
    AgentUsageReportedFields {
        input_tokens: fact.cumulative_after.input_tokens.is_some()
            && fact.delta.input_tokens.is_none(),
        output_tokens: fact.cumulative_after.output_tokens.is_some()
            && fact.delta.output_tokens.is_none(),
        thought_tokens: fact.cumulative_after.thought_tokens.is_some()
            && fact.delta.thought_tokens.is_none(),
        cached_read_tokens: fact.cumulative_after.cached_read_tokens.is_some()
            && fact.delta.cached_read_tokens.is_none(),
        cached_write_tokens: fact.cumulative_after.cached_write_tokens.is_some()
            && fact.delta.cached_write_tokens.is_none(),
        total_tokens: fact.cumulative_after.total_tokens.is_some()
            && fact.delta.total_tokens.is_none(),
    }
}

fn tokens_regress(current: &AgentUsageTokenValues, previous: &AgentUsageTokenValues) -> bool {
    [
        (current.input_tokens, previous.input_tokens),
        (current.output_tokens, previous.output_tokens),
        (current.thought_tokens, previous.thought_tokens),
        (current.cached_read_tokens, previous.cached_read_tokens),
        (current.cached_write_tokens, previous.cached_write_tokens),
        (current.total_tokens, previous.total_tokens),
    ]
    .into_iter()
    .any(|(current, previous)| matches!((current, previous), (Some(a), Some(b)) if a < b))
}

fn without_regressed_fields(
    current: &AgentUsageTokenValues,
    previous: &AgentUsageTokenValues,
) -> AgentUsageTokenValues {
    let accepted = |current, previous| match (current, previous) {
        (Some(current), Some(previous)) if current < previous => None,
        (current, _) => current,
    };
    AgentUsageTokenValues {
        input_tokens: accepted(current.input_tokens, previous.input_tokens),
        output_tokens: accepted(current.output_tokens, previous.output_tokens),
        thought_tokens: accepted(current.thought_tokens, previous.thought_tokens),
        cached_read_tokens: accepted(current.cached_read_tokens, previous.cached_read_tokens),
        cached_write_tokens: accepted(current.cached_write_tokens, previous.cached_write_tokens),
        total_tokens: accepted(current.total_tokens, previous.total_tokens),
    }
}

fn merge_cumulative(
    previous: &AgentUsageTokenValues,
    current: &AgentUsageTokenValues,
) -> AgentUsageTokenValues {
    AgentUsageTokenValues {
        input_tokens: current.input_tokens.or(previous.input_tokens),
        output_tokens: current.output_tokens.or(previous.output_tokens),
        thought_tokens: current.thought_tokens.or(previous.thought_tokens),
        cached_read_tokens: current.cached_read_tokens.or(previous.cached_read_tokens),
        cached_write_tokens: current.cached_write_tokens.or(previous.cached_write_tokens),
        total_tokens: current.total_tokens.or(previous.total_tokens),
    }
}

fn calculate_delta(
    previous: &AgentUsageTokenValues,
    current: &AgentUsageTokenValues,
    stream_started_at_zero: bool,
    preserve_unknown_baseline: AgentUsageReportedFields,
) -> VibexResult<(AgentUsageTokenValues, AgentUsageTokenValues, bool)> {
    let (input, input_delta, input_baseline) = calculate_field(
        previous.input_tokens,
        current.input_tokens,
        stream_started_at_zero,
        preserve_unknown_baseline.input_tokens,
    )?;
    let (output, output_delta, output_baseline) = calculate_field(
        previous.output_tokens,
        current.output_tokens,
        stream_started_at_zero,
        preserve_unknown_baseline.output_tokens,
    )?;
    let (thought, thought_delta, thought_baseline) = calculate_field(
        previous.thought_tokens,
        current.thought_tokens,
        stream_started_at_zero,
        preserve_unknown_baseline.thought_tokens,
    )?;
    let (cached_read, cached_read_delta, cached_read_baseline) = calculate_field(
        previous.cached_read_tokens,
        current.cached_read_tokens,
        stream_started_at_zero,
        preserve_unknown_baseline.cached_read_tokens,
    )?;
    let (cached_write, cached_write_delta, cached_write_baseline) = calculate_field(
        previous.cached_write_tokens,
        current.cached_write_tokens,
        stream_started_at_zero,
        preserve_unknown_baseline.cached_write_tokens,
    )?;
    let (total, total_delta, total_baseline) = calculate_field(
        previous.total_tokens,
        current.total_tokens,
        stream_started_at_zero,
        preserve_unknown_baseline.total_tokens,
    )?;
    Ok((
        AgentUsageTokenValues {
            input_tokens: input,
            output_tokens: output,
            thought_tokens: thought,
            cached_read_tokens: cached_read,
            cached_write_tokens: cached_write,
            total_tokens: total,
        },
        AgentUsageTokenValues {
            input_tokens: input_delta,
            output_tokens: output_delta,
            thought_tokens: thought_delta,
            cached_read_tokens: cached_read_delta,
            cached_write_tokens: cached_write_delta,
            total_tokens: total_delta,
        },
        input_baseline
            || output_baseline
            || thought_baseline
            || cached_read_baseline
            || cached_write_baseline
            || total_baseline,
    ))
}

fn calculate_field(
    previous: Option<u64>,
    current: Option<u64>,
    stream_started_at_zero: bool,
    preserve_unknown_baseline: bool,
) -> VibexResult<(Option<u64>, Option<u64>, bool)> {
    match (previous, current) {
        (previous, None) => Ok((
            previous,
            None,
            preserve_unknown_baseline && previous.is_some(),
        )),
        (None, Some(current)) if stream_started_at_zero => {
            Ok((Some(current), Some(current), false))
        }
        (None, Some(current)) => Ok((Some(current), None, true)),
        (Some(previous), Some(current)) if preserve_unknown_baseline && current == previous => {
            Ok((Some(current), None, true))
        }
        (Some(previous), Some(current)) => Ok((
            Some(current),
            Some(current.checked_sub(previous).ok_or_else(|| {
                VibexError::storage(
                    "agent_usage_counter_regressed",
                    "Agent usage counter regressed during delta calculation",
                )
            })?),
            false,
        )),
    }
}

fn add_token_values(
    existing: &AgentUsageTokenValues,
    increment: &AgentUsageTokenValues,
) -> VibexResult<AgentUsageTokenValues> {
    Ok(AgentUsageTokenValues {
        input_tokens: add_optional(existing.input_tokens, increment.input_tokens)?,
        output_tokens: add_optional(existing.output_tokens, increment.output_tokens)?,
        thought_tokens: add_optional(existing.thought_tokens, increment.thought_tokens)?,
        cached_read_tokens: add_optional(
            existing.cached_read_tokens,
            increment.cached_read_tokens,
        )?,
        cached_write_tokens: add_optional(
            existing.cached_write_tokens,
            increment.cached_write_tokens,
        )?,
        total_tokens: add_optional(existing.total_tokens, increment.total_tokens)?,
    })
}

fn add_optional(existing: Option<u64>, increment: Option<u64>) -> VibexResult<Option<u64>> {
    match (existing, increment) {
        (existing, None) => Ok(existing),
        (None, Some(increment)) => Ok(Some(increment)),
        (Some(existing), Some(increment)) => {
            existing.checked_add(increment).map(Some).ok_or_else(|| {
                VibexError::storage(
                    "agent_usage_delta_overflow",
                    "Agent usage delta exceeded the supported range",
                )
            })
        }
    }
}

fn upsert_checkpoint(tx: &Transaction<'_>, checkpoint: &AgentUsageCheckpoint) -> VibexResult<()> {
    tx.execute(
        "
        INSERT INTO agent_usage_checkpoints (
            usage_stream_id, session_id, binding_id, last_activation_generation, agent_id,
            provider_profile_id, auth_source_kind, auth_source_id, auth_source_revision,
            last_model_id, reset_epoch, counter_origin,
            cumulative_input_tokens, cumulative_output_tokens, cumulative_thought_tokens,
            cumulative_cached_read_tokens, cumulative_cached_write_tokens,
            cumulative_total_tokens, last_usage_execution_id, last_observation_sequence,
            created_at_ms, updated_at_ms
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22
        )
        ON CONFLICT(usage_stream_id) DO UPDATE SET
            session_id = excluded.session_id,
            binding_id = excluded.binding_id,
            last_activation_generation = excluded.last_activation_generation,
            agent_id = excluded.agent_id,
            provider_profile_id = excluded.provider_profile_id,
            auth_source_kind = excluded.auth_source_kind,
            auth_source_id = excluded.auth_source_id,
            auth_source_revision = excluded.auth_source_revision,
            last_model_id = excluded.last_model_id,
            reset_epoch = excluded.reset_epoch,
            counter_origin = excluded.counter_origin,
            cumulative_input_tokens = excluded.cumulative_input_tokens,
            cumulative_output_tokens = excluded.cumulative_output_tokens,
            cumulative_thought_tokens = excluded.cumulative_thought_tokens,
            cumulative_cached_read_tokens = excluded.cumulative_cached_read_tokens,
            cumulative_cached_write_tokens = excluded.cumulative_cached_write_tokens,
            cumulative_total_tokens = excluded.cumulative_total_tokens,
            last_usage_execution_id = excluded.last_usage_execution_id,
            last_observation_sequence = excluded.last_observation_sequence,
            updated_at_ms = excluded.updated_at_ms
        ",
        params![
            checkpoint.usage_stream_id,
            checkpoint.session_id.as_str(),
            checkpoint.binding_id.as_str(),
            checkpoint.last_activation_generation,
            checkpoint.agent_id.as_str(),
            checkpoint
                .auth_source
                .provider_profile_id()
                .map(ProviderProfileId::as_str),
            enum_to_db(&checkpoint.auth_source.kind())?,
            checkpoint.auth_source.id(),
            checkpoint.auth_source_revision,
            checkpoint.last_model_id,
            checkpoint.reset_epoch,
            enum_to_db(&checkpoint.counter_origin)?,
            token_to_db(checkpoint.cumulative.input_tokens)?,
            token_to_db(checkpoint.cumulative.output_tokens)?,
            token_to_db(checkpoint.cumulative.thought_tokens)?,
            token_to_db(checkpoint.cumulative.cached_read_tokens)?,
            token_to_db(checkpoint.cumulative.cached_write_tokens)?,
            token_to_db(checkpoint.cumulative.total_tokens)?,
            checkpoint
                .last_usage_execution_id
                .as_ref()
                .map(UsageExecutionId::as_str),
            i64::try_from(checkpoint.last_observation_sequence).map_err(|_| {
                VibexError::validation(
                    "agent_usage_observation_sequence_invalid",
                    "Agent usage observation sequence exceeded the supported range",
                )
            })?,
            checkpoint.created_at_ms,
            checkpoint.updated_at_ms,
        ],
    )
    .map_err(storage_err(
        "agent_usage_checkpoint_upsert_failed",
        "failed to update Agent usage checkpoint",
    ))?;
    Ok(())
}

fn get_checkpoint(
    conn: &Connection,
    usage_stream_id: &str,
) -> VibexResult<Option<AgentUsageCheckpoint>> {
    let raw = conn
        .query_row(
            "
            SELECT usage_stream_id, session_id, binding_id, last_activation_generation,
                   agent_id, provider_profile_id, auth_source_kind, auth_source_id,
                   auth_source_revision, last_model_id, reset_epoch, counter_origin,
                   cumulative_input_tokens, cumulative_output_tokens,
                   cumulative_thought_tokens, cumulative_cached_read_tokens,
                   cumulative_cached_write_tokens, cumulative_total_tokens,
                   last_usage_execution_id, last_observation_sequence,
                   created_at_ms, updated_at_ms
            FROM agent_usage_checkpoints
            WHERE usage_stream_id = ?1
            ",
            params![usage_stream_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, Option<i64>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, Option<i64>>(16)?,
                    row.get::<_, Option<i64>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, i64>(20)?,
                    row.get::<_, i64>(21)?,
                ))
            },
        )
        .optional()
        .map_err(storage_err(
            "agent_usage_checkpoint_get_failed",
            "failed to load Agent usage checkpoint",
        ))?;
    raw.map(|raw| {
        Ok(AgentUsageCheckpoint {
            usage_stream_id: raw.0,
            session_id: VibexSessionId::parse(raw.1)?,
            binding_id: RuntimeBindingId::parse(raw.2)?,
            last_activation_generation: raw.3,
            agent_id: AgentId::parse(raw.4)?,
            auth_source: decode_usage_auth_source(raw.6, raw.7, raw.5)?,
            auth_source_revision: raw.8,
            last_model_id: raw.9,
            reset_epoch: raw.10,
            counter_origin: enum_from_db(raw.11)?,
            cumulative: AgentUsageTokenValues {
                input_tokens: token_from_db(raw.12)?,
                output_tokens: token_from_db(raw.13)?,
                thought_tokens: token_from_db(raw.14)?,
                cached_read_tokens: token_from_db(raw.15)?,
                cached_write_tokens: token_from_db(raw.16)?,
                total_tokens: token_from_db(raw.17)?,
            },
            last_usage_execution_id: raw.18.map(UsageExecutionId::parse).transpose()?,
            last_observation_sequence: u64::try_from(raw.19).map_err(|_| {
                VibexError::storage(
                    "agent_usage_checkpoint_sequence_invalid",
                    "stored Agent usage checkpoint sequence was invalid",
                )
            })?,
            created_at_ms: raw.20,
            updated_at_ms: raw.21,
        })
    })
    .transpose()
}

fn update_fact(conn: &Connection, fact: &AgentTurnUsageFact) -> VibexResult<()> {
    conn.execute(
        "
        UPDATE agent_turn_usage_facts SET
            reset_epoch = ?2,
            execution_status = ?3,
            input_delta = ?4,
            output_delta = ?5,
            thought_delta = ?6,
            cached_read_delta = ?7,
            cached_write_delta = ?8,
            total_delta = ?9,
            cumulative_input_after = ?10,
            cumulative_output_after = ?11,
            cumulative_thought_after = ?12,
            cumulative_cached_read_after = ?13,
            cumulative_cached_write_after = ?14,
            cumulative_total_after = ?15,
            context_window_used_tokens = ?16,
            context_window_size_tokens = ?17,
            reported_fields = ?18,
            coverage = ?19,
            last_source = ?20,
            reset_reason = ?21,
            completed_at_ms = ?22,
            last_observed_at_ms = ?23,
            updated_at_ms = ?24
        WHERE usage_execution_id = ?1
        ",
        params![
            fact.usage_execution_id.as_str(),
            fact.reset_epoch,
            enum_to_db(&fact.execution_status)?,
            token_to_db(fact.delta.input_tokens)?,
            token_to_db(fact.delta.output_tokens)?,
            token_to_db(fact.delta.thought_tokens)?,
            token_to_db(fact.delta.cached_read_tokens)?,
            token_to_db(fact.delta.cached_write_tokens)?,
            token_to_db(fact.delta.total_tokens)?,
            token_to_db(fact.cumulative_after.input_tokens)?,
            token_to_db(fact.cumulative_after.output_tokens)?,
            token_to_db(fact.cumulative_after.thought_tokens)?,
            token_to_db(fact.cumulative_after.cached_read_tokens)?,
            token_to_db(fact.cumulative_after.cached_write_tokens)?,
            token_to_db(fact.cumulative_after.total_tokens)?,
            token_to_db(fact.context_window_used_tokens)?,
            token_to_db(fact.context_window_size_tokens)?,
            reported_fields_to_mask(fact.reported_fields),
            enum_to_db(&fact.coverage)?,
            fact.last_source.as_ref().map(enum_to_db).transpose()?,
            fact.reset_reason,
            fact.completed_at_ms,
            fact.last_observed_at_ms,
            fact.updated_at_ms,
        ],
    )
    .map_err(storage_err(
        "agent_usage_fact_update_failed",
        "failed to update Agent usage execution fact",
    ))?;
    Ok(())
}

fn get_fact(
    conn: &Connection,
    usage_execution_id: &UsageExecutionId,
) -> VibexResult<Option<AgentTurnUsageFact>> {
    let raw = conn
        .query_row(
            &format!(
                "SELECT {} FROM agent_turn_usage_facts f WHERE f.usage_execution_id = ?1",
                fact_columns("f")
            ),
            params![usage_execution_id.as_str()],
            read_raw_fact,
        )
        .optional()
        .map_err(storage_err(
            "agent_usage_fact_get_failed",
            "failed to load Agent usage execution fact",
        ))?;
    raw.map(decode_raw_fact).transpose()
}

fn fact_columns(alias: &str) -> String {
    [
        "usage_execution_id",
        "message_submission_id",
        "session_id",
        "project_id",
        "workspace_id",
        "binding_id",
        "activation_generation",
        "reset_epoch",
        "agent_id",
        "provider_profile_id",
        "auth_source_kind",
        "auth_source_id",
        "auth_source_revision",
        "model_id",
        "execution_status",
        "input_delta",
        "output_delta",
        "thought_delta",
        "cached_read_delta",
        "cached_write_delta",
        "total_delta",
        "cumulative_input_after",
        "cumulative_output_after",
        "cumulative_thought_after",
        "cumulative_cached_read_after",
        "cumulative_cached_write_after",
        "cumulative_total_after",
        "context_window_used_tokens",
        "context_window_size_tokens",
        "reported_fields",
        "coverage",
        "last_source",
        "reset_reason",
        "dispatched_at_ms",
        "completed_at_ms",
        "last_observed_at_ms",
        "created_at_ms",
        "updated_at_ms",
    ]
    .into_iter()
    .map(|column| format!("{alias}.{column}"))
    .collect::<Vec<_>>()
    .join(", ")
}

#[derive(Debug)]
struct RawUsageFact {
    usage_execution_id: String,
    message_submission_id: Option<String>,
    session_id: String,
    project_id: String,
    workspace_id: String,
    binding_id: String,
    activation_generation: i64,
    reset_epoch: i64,
    agent_id: String,
    provider_profile_id: Option<String>,
    auth_source_kind: String,
    auth_source_id: String,
    auth_source_revision: i64,
    model_id: Option<String>,
    execution_status: String,
    delta: [Option<i64>; 6],
    cumulative_after: [Option<i64>; 6],
    context_window_used_tokens: Option<i64>,
    context_window_size_tokens: Option<i64>,
    reported_fields: i64,
    coverage: String,
    last_source: Option<String>,
    reset_reason: Option<String>,
    dispatched_at_ms: i64,
    completed_at_ms: Option<i64>,
    last_observed_at_ms: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

fn read_raw_fact(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawUsageFact> {
    Ok(RawUsageFact {
        usage_execution_id: row.get(0)?,
        message_submission_id: row.get(1)?,
        session_id: row.get(2)?,
        project_id: row.get(3)?,
        workspace_id: row.get(4)?,
        binding_id: row.get(5)?,
        activation_generation: row.get(6)?,
        reset_epoch: row.get(7)?,
        agent_id: row.get(8)?,
        provider_profile_id: row.get(9)?,
        auth_source_kind: row.get(10)?,
        auth_source_id: row.get(11)?,
        auth_source_revision: row.get(12)?,
        model_id: row.get(13)?,
        execution_status: row.get(14)?,
        delta: [
            row.get(15)?,
            row.get(16)?,
            row.get(17)?,
            row.get(18)?,
            row.get(19)?,
            row.get(20)?,
        ],
        cumulative_after: [
            row.get(21)?,
            row.get(22)?,
            row.get(23)?,
            row.get(24)?,
            row.get(25)?,
            row.get(26)?,
        ],
        context_window_used_tokens: row.get(27)?,
        context_window_size_tokens: row.get(28)?,
        reported_fields: row.get(29)?,
        coverage: row.get(30)?,
        last_source: row.get(31)?,
        reset_reason: row.get(32)?,
        dispatched_at_ms: row.get(33)?,
        completed_at_ms: row.get(34)?,
        last_observed_at_ms: row.get(35)?,
        created_at_ms: row.get(36)?,
        updated_at_ms: row.get(37)?,
    })
}

fn decode_raw_fact(raw: RawUsageFact) -> VibexResult<AgentTurnUsageFact> {
    Ok(AgentTurnUsageFact {
        usage_execution_id: UsageExecutionId::parse(raw.usage_execution_id)?,
        message_submission_id: raw
            .message_submission_id
            .map(MessageSubmissionId::parse)
            .transpose()?,
        session_id: VibexSessionId::parse(raw.session_id)?,
        project_id: ProjectId::parse(raw.project_id)?,
        workspace_id: WorkspaceId::parse(raw.workspace_id)?,
        binding_id: RuntimeBindingId::parse(raw.binding_id)?,
        activation_generation: raw.activation_generation,
        reset_epoch: raw.reset_epoch,
        agent_id: AgentId::parse(raw.agent_id)?,
        auth_source: decode_usage_auth_source(
            raw.auth_source_kind,
            raw.auth_source_id,
            raw.provider_profile_id,
        )?,
        auth_source_revision: raw.auth_source_revision,
        model_id: raw.model_id,
        execution_status: enum_from_db(raw.execution_status)?,
        delta: AgentUsageTokenValues {
            input_tokens: token_from_db(raw.delta[0])?,
            output_tokens: token_from_db(raw.delta[1])?,
            thought_tokens: token_from_db(raw.delta[2])?,
            cached_read_tokens: token_from_db(raw.delta[3])?,
            cached_write_tokens: token_from_db(raw.delta[4])?,
            total_tokens: token_from_db(raw.delta[5])?,
        },
        cumulative_after: AgentUsageTokenValues {
            input_tokens: token_from_db(raw.cumulative_after[0])?,
            output_tokens: token_from_db(raw.cumulative_after[1])?,
            thought_tokens: token_from_db(raw.cumulative_after[2])?,
            cached_read_tokens: token_from_db(raw.cumulative_after[3])?,
            cached_write_tokens: token_from_db(raw.cumulative_after[4])?,
            total_tokens: token_from_db(raw.cumulative_after[5])?,
        },
        context_window_used_tokens: token_from_db(raw.context_window_used_tokens)?,
        context_window_size_tokens: token_from_db(raw.context_window_size_tokens)?,
        reported_fields: reported_fields_from_mask(raw.reported_fields),
        coverage: enum_from_db(raw.coverage)?,
        last_source: raw.last_source.map(enum_from_db).transpose()?,
        reset_reason: raw.reset_reason,
        dispatched_at_ms: raw.dispatched_at_ms,
        completed_at_ms: raw.completed_at_ms,
        last_observed_at_ms: raw.last_observed_at_ms,
        created_at_ms: raw.created_at_ms,
        updated_at_ms: raw.updated_at_ms,
    })
}

fn token_to_db(value: Option<u64>) -> VibexResult<Option<i64>> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                VibexError::validation(
                    "agent_usage_token_out_of_range",
                    "Agent usage token value exceeded the supported range",
                )
            })
        })
        .transpose()
}

fn token_from_db(value: Option<i64>) -> VibexResult<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                VibexError::storage(
                    "agent_usage_token_invalid",
                    "stored Agent usage token value was invalid",
                )
            })
        })
        .transpose()
}

fn reported_fields_to_mask(fields: AgentUsageReportedFields) -> i64 {
    (i64::from(fields.input_tokens) * INPUT_REPORTED)
        | (i64::from(fields.output_tokens) * OUTPUT_REPORTED)
        | (i64::from(fields.thought_tokens) * THOUGHT_REPORTED)
        | (i64::from(fields.cached_read_tokens) * CACHED_READ_REPORTED)
        | (i64::from(fields.cached_write_tokens) * CACHED_WRITE_REPORTED)
        | (i64::from(fields.total_tokens) * TOTAL_REPORTED)
}

fn reported_fields_from_mask(mask: i64) -> AgentUsageReportedFields {
    AgentUsageReportedFields {
        input_tokens: mask & INPUT_REPORTED != 0,
        output_tokens: mask & OUTPUT_REPORTED != 0,
        thought_tokens: mask & THOUGHT_REPORTED != 0,
        cached_read_tokens: mask & CACHED_READ_REPORTED != 0,
        cached_write_tokens: mask & CACHED_WRITE_REPORTED != 0,
        total_tokens: mask & TOTAL_REPORTED != 0,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use vibex_core::{
        AgentSession, AgentSessionSafety, AgentSessionState, AgentUsageExecutionContext,
        AgentUsageObservationSource, AgentUsageStreamAttribution, WorkspaceMode, unix_timestamp_ms,
    };

    use super::*;
    use crate::{
        CURRENT_SCHEMA_VERSION, RuntimeBindingRepository, SessionRepository, WorkspaceRepository,
        apply_migrations, current_schema_version, open_database,
    };

    const AGENT_USAGE_MIGRATION_VERSION: i64 = 31;

    fn temp_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-agent-usage-{name}-{}-{}.db",
            std::process::id(),
            unix_timestamp_ms()
        ))
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn seeded(conn: &Connection, name: &str) -> AgentUsageExecutionContext {
        let root = std::env::temp_dir().join(format!("vibex-usage-workspace-{name}"));
        std::fs::create_dir_all(&root).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(conn, &root, WorkspaceMode::CurrentCheckout).unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: format!("Usage {name}"),
            project_id: project.id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("opencode").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            last_message_at_ms: 1_000,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(conn, &session).unwrap();
        let provider_profile_id = ProviderProfileId::new();
        let context = AgentUsageExecutionContext {
            usage_execution_id: UsageExecutionId::new(),
            message_submission_id: None,
            project_id: project.id,
            workspace_id: workspace.id,
            stream: AgentUsageStreamAttribution {
                session_id: session.id,
                binding_id: RuntimeBindingId::new(),
                activation_generation: 1,
                agent_id: AgentId::parse("opencode").unwrap(),
                auth_source: RuntimeAuthSource::provider_profile(provider_profile_id),
                auth_source_revision: 1,
                model_id: Some("test-model".to_string()),
            },
        };
        let usage_migration_applied_at_ms = conn
            .query_row(
                "SELECT applied_at_ms FROM schema_migrations WHERE version = ?1",
                params![AGENT_USAGE_MIGRATION_VERSION],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        insert_usage_binding(
            conn,
            &context,
            usage_migration_applied_at_ms.saturating_add(1),
        );
        context
    }

    fn insert_usage_binding(
        conn: &Connection,
        context: &AgentUsageExecutionContext,
        binding_created_at_ms: i64,
    ) {
        conn.execute(
            "
            INSERT INTO session_runtime_bindings (
                binding_id, session_id, agent_id, transport_kind, adapter_id, adapter_version,
                adapter_compatibility_identity, provider_profile_id, profile_revision,
                auth_source_kind, auth_source_id, auth_source_revision, native_state_home_id,
                process_spawn_fingerprint, session_runtime_config_state_json, binding_state,
                activation_generation, created_at_ms, updated_at_ms, usage_zero_baseline_state,
                usage_zero_baseline_execution_id, usage_zero_baseline_activation_generation
            ) VALUES (
                ?1, ?2, ?3, 'acp', 'usage-test-adapter', '1.0.0',
                'usage-test-compatibility', ?4, ?5, ?6, ?7, ?8, 'usage-test-home',
                'usage-test-fingerprint', '{}', 'current', ?11, ?9, ?9, 'claimed', ?10, ?11
            )
            ",
            params![
                context.stream.binding_id.as_str(),
                context.stream.session_id.as_str(),
                context.stream.agent_id.as_str(),
                context
                    .stream
                    .auth_source
                    .provider_profile_id()
                    .unwrap()
                    .as_str(),
                context.stream.auth_source_revision,
                enum_to_db(&context.stream.auth_source.kind()).unwrap(),
                context.stream.auth_source.id(),
                context.stream.auth_source_revision,
                binding_created_at_ms,
                context.usage_execution_id.as_str(),
                context.stream.activation_generation,
            ],
        )
        .unwrap();
    }

    fn observation(
        execution: AgentUsageExecution,
        sequence: u64,
        total: u64,
        origin: AgentUsageCounterOrigin,
    ) -> AgentUsageObservation {
        let observed_at_ms = execution.dispatched_at_ms + 10;
        AgentUsageObservation {
            stream: execution.stream.clone(),
            execution: Some(execution),
            counter_origin: origin,
            observation_sequence: sequence,
            cumulative: AgentUsageTokenValues {
                input_tokens: Some(total),
                output_tokens: Some(total / 2),
                cached_read_tokens: Some(total / 4),
                total_tokens: Some(total),
                ..AgentUsageTokenValues::default()
            },
            context_window_used_tokens: Some(total),
            context_window_size_tokens: Some(200_000),
            source: AgentUsageObservationSource::PromptResponse,
            observed_at_ms,
        }
    }

    #[test]
    fn migration_creates_usage_tables_and_indexes() {
        let path = temp_db("migration");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        assert_eq!(
            current_schema_version(&conn).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        for table in ["agent_usage_checkpoints", "agent_turn_usage_facts"] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing {table}");
        }
        cleanup(&path);
    }

    #[test]
    fn terminal_status_recovers_missing_facts_without_claiming_usage_support() {
        let path = temp_db("status-recovery");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "status-recovery");

        for (index, status) in [
            AgentUsageExecutionStatus::Completed,
            AgentUsageExecutionStatus::Failed,
            AgentUsageExecutionStatus::Interrupted,
        ]
        .into_iter()
        .enumerate()
        {
            let mut next = context.clone();
            next.usage_execution_id = UsageExecutionId::new();
            let execution = next.dispatched_at(2_000 + index as i64 * 100);
            let update = AgentUsageExecutionStatusUpdate {
                execution: execution.clone(),
                status,
                completed_at_ms: execution.dispatched_at_ms + 50,
            };
            assert!(AgentUsageRepository::record_execution_status(&conn, &update).unwrap());
            assert!(!AgentUsageRepository::record_execution_status(&conn, &update).unwrap());

            let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
                .unwrap()
                .unwrap();
            assert_eq!(fact.execution_status, status);
            assert_eq!(fact.coverage, AgentUsageCoverage::Unreported);
            assert_eq!(fact.last_observed_at_ms, None);
        }
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 3);
        cleanup(&path);
    }

    #[test]
    fn cumulative_sequence_becomes_exact_execution_deltas() {
        let path = temp_db("deltas");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "deltas");

        let mut executions = Vec::new();
        for (index, cumulative) in [1_000, 1_800, 2_500].into_iter().enumerate() {
            let mut next = context.clone();
            if index > 0 {
                next.usage_execution_id = UsageExecutionId::new();
            }
            let execution = next.dispatched_at(2_000 + index as i64 * 100);
            AgentUsageRepository::apply_observation(
                &mut conn,
                &observation(
                    execution.clone(),
                    index as u64 + 1,
                    cumulative,
                    AgentUsageCounterOrigin::KnownZero,
                ),
            )
            .unwrap();
            executions.push(execution.usage_execution_id);
        }

        let deltas = executions
            .iter()
            .map(|id| {
                AgentUsageRepository::get_fact(&conn, id)
                    .unwrap()
                    .unwrap()
                    .delta
                    .total_tokens
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(deltas, [1_000, 800, 700]);
        cleanup(&path);
    }

    #[test]
    fn binding_without_a_persisted_claim_cannot_use_a_zero_baseline() {
        let path = temp_db("unclaimed-binding");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "unclaimed-binding");
        conn.execute(
            "UPDATE session_runtime_bindings
             SET usage_zero_baseline_state = 'unavailable',
                 usage_zero_baseline_execution_id = NULL,
                 usage_zero_baseline_activation_generation = NULL
             WHERE binding_id = ?1",
            params![context.stream.binding_id.as_str()],
        )
        .unwrap();

        let execution = context.dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(
                execution.clone(),
                1,
                1_800,
                AgentUsageCounterOrigin::KnownZero,
            ),
        )
        .unwrap();

        let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.total_tokens, None);
        assert_eq!(fact.coverage, AgentUsageCoverage::BaselineOnly);
        cleanup(&path);
    }

    #[test]
    fn claimed_zero_baseline_does_not_depend_on_timestamp_ordering() {
        let path = temp_db("zero-claim-same-millisecond");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "zero-claim-same-millisecond");
        let migration_applied_at_ms = conn
            .query_row(
                "SELECT applied_at_ms FROM schema_migrations WHERE version = ?1",
                params![AGENT_USAGE_MIGRATION_VERSION],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE session_runtime_bindings
             SET created_at_ms = ?2, updated_at_ms = ?2
             WHERE binding_id = ?1",
            params![context.stream.binding_id.as_str(), migration_applied_at_ms],
        )
        .unwrap();

        let execution = context.dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(
                execution.clone(),
                1,
                1_800,
                AgentUsageCounterOrigin::KnownZero,
            ),
        )
        .unwrap();

        let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.total_tokens, Some(1_800));
        assert_ne!(fact.coverage, AgentUsageCoverage::BaselineOnly);
        cleanup(&path);
    }

    #[test]
    fn same_binding_generation_advance_preserves_the_cumulative_checkpoint() {
        let path = temp_db("generation-lineage");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "generation-lineage");

        let first = context.clone().dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(first, 7, 1_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let mut next = context;
        next.usage_execution_id = UsageExecutionId::new();
        next.stream.activation_generation = 2;
        let second = next.dispatched_at(3_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(
                second.clone(),
                1,
                1_600,
                AgentUsageCounterOrigin::RestoredCheckpoint,
            ),
        )
        .unwrap();

        let fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.activation_generation, 2);
        assert_eq!(fact.delta.total_tokens, Some(600));
        assert_eq!(fact.reset_epoch, 0);
        assert_ne!(fact.coverage, AgentUsageCoverage::BaselineOnly);
        cleanup(&path);
    }

    #[test]
    fn a_new_binding_uses_an_independent_counter_checkpoint() {
        let path = temp_db("binding-isolation");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "binding-isolation");

        let first = context.clone().dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(first, 1, 1_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let mut next = context;
        next.usage_execution_id = UsageExecutionId::new();
        next.stream.binding_id = RuntimeBindingId::new();
        next.stream.activation_generation = 2;
        let usage_migration_applied_at_ms = conn
            .query_row(
                "SELECT applied_at_ms FROM schema_migrations WHERE version = ?1",
                params![AGENT_USAGE_MIGRATION_VERSION],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        insert_usage_binding(
            &conn,
            &next,
            usage_migration_applied_at_ms.saturating_add(2),
        );
        let second = next.dispatched_at(3_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(second.clone(), 1, 1_200, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.total_tokens, Some(1_200));
        assert_eq!(fact.reset_epoch, 0);
        let checkpoint_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_usage_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(checkpoint_count, 2);
        cleanup(&path);
    }

    #[test]
    fn model_switch_on_the_same_lineage_attributes_the_delta_to_the_new_model() {
        let path = temp_db("model-switch-lineage");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "model-switch-lineage");

        let first = context.clone().dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(first, 1, 1_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let mut next = context;
        next.usage_execution_id = UsageExecutionId::new();
        next.stream.model_id = Some("switched-model".to_string());
        let second = next.dispatched_at(3_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(
                second.clone(),
                2,
                1_500,
                AgentUsageCounterOrigin::RestoredCheckpoint,
            ),
        )
        .unwrap();

        let fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.model_id.as_deref(), Some("switched-model"));
        assert_eq!(fact.delta.total_tokens, Some(500));
        let checkpoint_model: Option<String> = conn
            .query_row(
                "SELECT last_model_id FROM agent_usage_checkpoints WHERE binding_id = ?1",
                params![second.stream.binding_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpoint_model.as_deref(), Some("switched-model"));
        cleanup(&path);
    }

    #[test]
    fn known_zero_stream_baselines_fields_first_reported_after_an_earlier_execution() {
        let path = temp_db("known-zero-partial");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "known-zero-partial");

        let first = context.clone().dispatched_at(2_000);
        let mut first_observation =
            observation(first.clone(), 1, 100, AgentUsageCounterOrigin::KnownZero);
        first_observation.cumulative = AgentUsageTokenValues {
            input_tokens: Some(100),
            ..AgentUsageTokenValues::default()
        };
        AgentUsageRepository::apply_observation(&mut conn, &first_observation).unwrap();

        let mut second_context = context;
        second_context.usage_execution_id = UsageExecutionId::new();
        let second = second_context.dispatched_at(3_000);
        let mut second_observation = observation(
            second.clone(),
            2,
            180,
            AgentUsageCounterOrigin::RestoredCheckpoint,
        );
        second_observation.cumulative = AgentUsageTokenValues {
            input_tokens: Some(180),
            output_tokens: Some(40),
            ..AgentUsageTokenValues::default()
        };
        AgentUsageRepository::apply_observation(&mut conn, &second_observation).unwrap();

        let mut tail = second_observation;
        tail.observation_sequence = 3;
        tail.observed_at_ms += 1;
        tail.cumulative.output_tokens = Some(55);
        tail.cumulative.cached_read_tokens = Some(20);
        tail.source = AgentUsageObservationSource::SessionUsageUpdate;
        AgentUsageRepository::apply_observation(&mut conn, &tail).unwrap();
        tail.observation_sequence = 4;
        tail.observed_at_ms += 1;
        tail.cumulative.cached_read_tokens = Some(27);
        AgentUsageRepository::apply_observation(&mut conn, &tail).unwrap();

        let first_fact = AgentUsageRepository::get_fact(&conn, &first.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(first_fact.delta.input_tokens, Some(100));
        assert_eq!(first_fact.delta.output_tokens, None);
        let second_fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(second_fact.delta.input_tokens, Some(80));
        assert_eq!(second_fact.delta.output_tokens, Some(15));
        assert_eq!(second_fact.delta.cached_read_tokens, Some(7));
        cleanup(&path);
    }

    #[test]
    fn known_zero_stream_with_an_unreported_prior_execution_establishes_a_baseline() {
        let path = temp_db("known-zero-gap");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "known-zero-gap");

        let first = context.clone().dispatched_at(2_000);
        AgentUsageRepository::record_execution(&conn, &first).unwrap();

        let mut second_context = context;
        second_context.usage_execution_id = UsageExecutionId::new();
        let second = second_context.dispatched_at(3_000);
        AgentUsageRepository::record_execution(&conn, &second).unwrap();
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(second.clone(), 1, 1_800, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let baseline = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(baseline.delta.total_tokens, None);
        assert_eq!(baseline.coverage, AgentUsageCoverage::BaselineOnly);

        let mut later = observation(
            second.clone(),
            2,
            2_000,
            AgentUsageCounterOrigin::RestoredCheckpoint,
        );
        later.observed_at_ms += 1;
        AgentUsageRepository::apply_observation(&mut conn, &later).unwrap();
        let recovered = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.delta.total_tokens, Some(200));
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 2);
        cleanup(&path);
    }

    #[test]
    fn persisted_zero_claim_prevents_overcount_when_all_first_execution_usage_writes_are_lost() {
        let path = temp_db("zero-claim-restart-gap");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let first_context = seeded(&conn, "zero-claim-restart-gap");

        assert!(
            RuntimeBindingRepository::claim_usage_zero_baseline(
                &conn,
                &first_context.stream.binding_id,
                first_context.stream.activation_generation,
                &first_context.usage_execution_id,
            )
            .unwrap()
        );
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 0);

        let mut second_context = first_context;
        second_context.usage_execution_id = UsageExecutionId::new();
        assert!(
            !RuntimeBindingRepository::claim_usage_zero_baseline(
                &conn,
                &second_context.stream.binding_id,
                second_context.stream.activation_generation,
                &second_context.usage_execution_id,
            )
            .unwrap()
        );
        let second = second_context.dispatched_at(3_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(second.clone(), 1, 1_800, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let baseline = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(baseline.delta.total_tokens, None);
        assert_eq!(baseline.coverage, AgentUsageCoverage::BaselineOnly);

        let mut tail = observation(
            second.clone(),
            2,
            2_000,
            AgentUsageCounterOrigin::RestoredCheckpoint,
        );
        tail.observed_at_ms += 1;
        AgentUsageRepository::apply_observation(&mut conn, &tail).unwrap();
        let recovered = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.delta.total_tokens, Some(200));
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 1);
        cleanup(&path);
    }

    #[test]
    fn resumed_first_sample_and_duplicate_remain_baseline_only() {
        let path = temp_db("baseline");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let execution = seeded(&conn, "baseline").dispatched_at(2_000);
        let first = observation(
            execution.clone(),
            1,
            9_000,
            AgentUsageCounterOrigin::Resumed,
        );
        AgentUsageRepository::apply_observation(&mut conn, &first).unwrap();
        let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.total_tokens, None);
        assert_eq!(fact.coverage, AgentUsageCoverage::BaselineOnly);

        let mut context_only = first.clone();
        context_only.observation_sequence = 2;
        context_only.observed_at_ms += 1;
        context_only.cumulative = AgentUsageTokenValues::default();
        context_only.source = AgentUsageObservationSource::SessionUsageUpdate;
        AgentUsageRepository::apply_observation(&mut conn, &context_only).unwrap();

        let mut duplicate = first;
        duplicate.observation_sequence = 3;
        duplicate.observed_at_ms += 2;
        AgentUsageRepository::apply_observation(&mut conn, &duplicate).unwrap();
        let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.total_tokens, None);
        assert_eq!(fact.coverage, AgentUsageCoverage::BaselineOnly);
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 1);
        cleanup(&path);
    }

    #[test]
    fn reset_clears_unreported_field_checkpoints_for_the_new_epoch() {
        let path = temp_db("reset-partial");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let context = seeded(&conn, "reset-partial");
        let first = context.clone().dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(first, 1, 5_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let mut second_context = context.clone();
        second_context.usage_execution_id = UsageExecutionId::new();
        let second = second_context.dispatched_at(3_000);
        let mut reset = observation(second, 2, 300, AgentUsageCounterOrigin::KnownZero);
        reset.cumulative = AgentUsageTokenValues {
            input_tokens: Some(300),
            total_tokens: Some(300),
            ..AgentUsageTokenValues::default()
        };
        let outcome = AgentUsageRepository::apply_observation(&mut conn, &reset).unwrap();
        assert!(outcome.reset_epoch_started);

        let mut third_context = context;
        third_context.usage_execution_id = UsageExecutionId::new();
        let third = third_context.dispatched_at(4_000);
        let mut after_reset =
            observation(third.clone(), 3, 400, AgentUsageCounterOrigin::KnownZero);
        after_reset.cumulative = AgentUsageTokenValues {
            input_tokens: Some(400),
            output_tokens: Some(100),
            total_tokens: Some(400),
            ..AgentUsageTokenValues::default()
        };
        let outcome = AgentUsageRepository::apply_observation(&mut conn, &after_reset).unwrap();
        assert!(!outcome.reset_epoch_started);
        let fact = AgentUsageRepository::get_fact(&conn, &third.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.reset_epoch, 1);
        assert_eq!(fact.delta.input_tokens, Some(100));
        assert_eq!(fact.delta.output_tokens, None);
        assert_eq!(fact.delta.total_tokens, Some(100));
        cleanup(&path);
    }

    #[test]
    fn resumed_tail_increase_records_only_the_post_baseline_delta() {
        let path = temp_db("baseline-tail");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let execution = seeded(&conn, "baseline-tail").dispatched_at(2_000);
        let first = observation(
            execution.clone(),
            1,
            9_000,
            AgentUsageCounterOrigin::Resumed,
        );
        AgentUsageRepository::apply_observation(&mut conn, &first).unwrap();

        let mut tail = first;
        tail.observation_sequence = 2;
        tail.observed_at_ms += 1;
        tail.cumulative.input_tokens = Some(9_200);
        tail.cumulative.output_tokens = Some(4_600);
        tail.cumulative.cached_read_tokens = Some(2_300);
        tail.cumulative.total_tokens = Some(9_200);
        AgentUsageRepository::apply_observation(&mut conn, &tail).unwrap();
        let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.input_tokens, Some(200));
        assert_eq!(fact.delta.output_tokens, Some(100));
        assert_eq!(fact.delta.cached_read_tokens, Some(50));
        assert_eq!(fact.delta.total_tokens, Some(200));
        assert_eq!(fact.coverage, AgentUsageCoverage::Partial);
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 1);
        cleanup(&path);
    }

    #[test]
    fn same_execution_regression_discards_only_the_regressed_fields() {
        let path = temp_db("same-execution-partial-regression");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let execution = seeded(&conn, "same-execution-partial-regression").dispatched_at(2_000);
        let mut first = observation(
            execution.clone(),
            1,
            100,
            AgentUsageCounterOrigin::KnownZero,
        );
        first.cumulative = AgentUsageTokenValues {
            input_tokens: Some(100),
            ..AgentUsageTokenValues::default()
        };
        AgentUsageRepository::apply_observation(&mut conn, &first).unwrap();

        let mut tail = first;
        tail.observation_sequence = 2;
        tail.observed_at_ms += 1;
        tail.cumulative = AgentUsageTokenValues {
            input_tokens: Some(90),
            output_tokens: Some(50),
            ..AgentUsageTokenValues::default()
        };
        let outcome = AgentUsageRepository::apply_observation(&mut conn, &tail).unwrap();
        assert!(!outcome.reset_epoch_started);

        let fact = AgentUsageRepository::get_fact(&conn, &execution.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.input_tokens, Some(100));
        assert_eq!(fact.delta.output_tokens, Some(50));
        assert_eq!(fact.cumulative_after.input_tokens, Some(100));
        assert_eq!(fact.cumulative_after.output_tokens, Some(50));
        assert_eq!(fact.reset_epoch, 0);
        cleanup(&path);
    }

    #[test]
    fn regression_starts_epoch_without_negative_delta_and_old_execution_is_ignored() {
        let path = temp_db("reset");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let base = seeded(&conn, "reset");
        let first = base.clone().dispatched_at(2_000);
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(first.clone(), 1, 5_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();

        let mut second_context = base;
        second_context.usage_execution_id = UsageExecutionId::new();
        let second = second_context.dispatched_at(3_000);
        let outcome = AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(second.clone(), 2, 300, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();
        assert!(outcome.reset_epoch_started);
        let fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.delta.total_tokens, None);
        assert_eq!(fact.reset_epoch, 1);

        let mut late = observation(first.clone(), 3, 4_900, AgentUsageCounterOrigin::KnownZero);
        late.observed_at_ms = 3_100;
        let outcome = AgentUsageRepository::apply_observation(&mut conn, &late).unwrap();
        assert!(outcome.ignored_stale_observation);
        let fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.reset_epoch, 1);
        cleanup(&path);
    }

    #[test]
    fn same_millisecond_late_execution_uses_persisted_dispatch_order() {
        let path = temp_db("same-millisecond-late-execution");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let base = seeded(&conn, "same-millisecond-late-execution");
        let first = base.clone().dispatched_at(2_000);
        let mut second_context = base;
        second_context.usage_execution_id = UsageExecutionId::new();
        let second = second_context.dispatched_at(2_000);
        AgentUsageRepository::record_execution(&conn, &first).unwrap();
        AgentUsageRepository::record_execution(&conn, &second).unwrap();

        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(first.clone(), 1, 5_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();
        let reset = AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(second.clone(), 2, 300, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();
        assert!(reset.reset_epoch_started);

        let mut late = observation(first, 3, 4_900, AgentUsageCounterOrigin::KnownZero);
        late.observed_at_ms = 3_100;
        let outcome = AgentUsageRepository::apply_observation(&mut conn, &late).unwrap();
        assert!(outcome.ignored_stale_observation);

        let checkpoint = get_checkpoint(&conn, second.stream.binding_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(
            checkpoint.last_usage_execution_id,
            Some(second.usage_execution_id.clone())
        );
        assert_eq!(checkpoint.cumulative.total_tokens, Some(300));
        assert_eq!(checkpoint.reset_epoch, 1);
        let fact = AgentUsageRepository::get_fact(&conn, &second.usage_execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(fact.reset_epoch, 1);
        assert_eq!(fact.delta.total_tokens, None);
        cleanup(&path);
    }

    #[test]
    fn fact_and_checkpoint_roll_back_together() {
        let path = temp_db("rollback");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let execution = seeded(&conn, "rollback").dispatched_at(2_000);
        conn.execute_batch(
            "
            CREATE TRIGGER fail_usage_checkpoint
            BEFORE INSERT ON agent_usage_checkpoints
            BEGIN
                SELECT RAISE(ABORT, 'usage checkpoint failure');
            END;
            ",
        )
        .unwrap();
        let result = AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(execution, 1, 1_000, AgentUsageCounterOrigin::KnownZero),
        );
        assert!(result.is_err());
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 0);
        cleanup(&path);
    }

    #[test]
    fn deleting_session_cascades_facts_and_checkpoints() {
        let path = temp_db("cascade");
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let execution = seeded(&conn, "cascade").dispatched_at(2_000);
        let session_id = execution.stream.session_id.clone();
        AgentUsageRepository::apply_observation(
            &mut conn,
            &observation(execution, 1, 1_000, AgentUsageCounterOrigin::KnownZero),
        )
        .unwrap();
        conn.execute(
            "DELETE FROM agent_sessions WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .unwrap();
        assert_eq!(AgentUsageRepository::count_facts(&conn).unwrap(), 0);
        let checkpoints: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_usage_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(checkpoints, 0);
        cleanup(&path);
    }
}

//! Persistence for the local model gateway's request-level state.
//!
//! Everything here is routing state: which upstreams are configured, how each
//! Key is behaving, how many tokens one Provider carried in a window, which
//! conversation is pinned where, and a metadata-only request ledger. The
//! durable Provider configuration itself stays in `model_provider_profiles`
//! and `agent_model_provider_bindings_v2`, so a gateway can be added or removed
//! without touching a single upstream record.
//!
//! The ledger never stores prompt text, reply text, image bytes, or Key
//! material. `model_gateway_request_log` is pruned to a rolling week.

use rusqlite::{Connection, OptionalExtension, params};

use vibex_core::{
    GATEWAY_REQUEST_PAGE_MAX, GatewayConversationPin, GatewayFailureKind, GatewayRequestCursor,
    GatewayRequestOutcome, GatewayRequestQuery, GatewayRouteTrace, GatewayTokenWindow,
    GatewayTokenWindowUsage, ModelGatewayRequestLog, ModelGatewaySettings,
    ModelProviderAccountState, ModelProviderProfileId, RequestId, VibexError, VibexResult,
    unix_timestamp_ms,
};

use crate::{DbConnection, storage_err};

/// Fixed key for the one gateway this build runs.
const SETTINGS_ID: i64 = 1;

pub struct ModelGatewaySettingsRepository;

impl ModelGatewaySettingsRepository {
    /// Reads the gateway settings, creating defaults on first access.
    pub fn get_or_create(conn: &Connection) -> VibexResult<ModelGatewaySettings> {
        if let Some(settings) = Self::get(conn)? {
            return Ok(settings);
        }
        let settings = ModelGatewaySettings {
            updated_at_ms: unix_timestamp_ms().max(1),
            ..ModelGatewaySettings::default()
        };
        conn.execute(
            "
            INSERT INTO model_gateway_settings (
                settings_id, display_name, port, upstreams_json, policy_json,
                revision, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(settings_id) DO NOTHING
            ",
            params![
                SETTINGS_ID,
                settings.display_name,
                i64::from(settings.port),
                // Retained for schema compatibility only: the gateway owns no
                // upstream list any more, and this column is always written
                // empty. Routes are its only input.
                "[]",
                encode_json("model_gateway_policy_encode_failed", &settings.policy)?,
                settings.revision,
                settings.updated_at_ms,
                settings.updated_at_ms,
            ],
        )
        .map_err(storage_err(
            "model_gateway_settings_insert_failed",
            "failed to create gateway settings",
        ))?;
        Self::get(conn)?.ok_or_else(|| {
            VibexError::storage(
                "model_gateway_settings_readback_failed",
                "failed to read gateway settings after create",
            )
        })
    }

    pub fn get(conn: &Connection) -> VibexResult<Option<ModelGatewaySettings>> {
        let row = conn
            .query_row(
                "
                SELECT display_name, port, policy_json,
                       revision, updated_at_ms
                FROM model_gateway_settings
                WHERE settings_id = ?1
                ",
                params![SETTINGS_ID],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err(
                "model_gateway_settings_read_failed",
                "failed to read gateway settings",
            ))?;
        let Some((display_name, port, policy, revision, updated_at_ms)) = row else {
            return Ok(None);
        };
        let settings = ModelGatewaySettings {
            display_name,
            port: u16::try_from(port).map_err(|_| {
                VibexError::storage(
                    "model_gateway_settings_invalid",
                    "gateway settings hold an out-of-range port",
                )
            })?,
            policy: decode_json("model_gateway_settings_invalid", &policy)?,
            revision,
            updated_at_ms,
        };
        settings.validate().map_err(|error| {
            VibexError::storage(
                "model_gateway_settings_invalid",
                format!("gateway settings are invalid: {}", error.message),
            )
        })?;
        Ok(Some(settings))
    }

    /// Replaces the settings, advancing the revision exactly once.
    pub fn update(
        conn: &Connection,
        settings: &ModelGatewaySettings,
        expected_revision: i64,
    ) -> VibexResult<ModelGatewaySettings> {
        settings.validate()?;
        if settings.revision != expected_revision.saturating_add(1) {
            return Err(VibexError::validation(
                "model_gateway_settings_revision_invalid",
                "gateway settings must advance exactly one revision from the observed one",
            ));
        }
        let updated_at_ms = unix_timestamp_ms().max(settings.updated_at_ms.saturating_add(1));
        let changed = conn
            .execute(
                "
                UPDATE model_gateway_settings
                SET display_name = ?1,
                    port = ?2,
                    upstreams_json = '[]',
                    policy_json = ?3,
                    revision = ?4,
                    updated_at_ms = ?5
                WHERE settings_id = ?6 AND revision = ?7
                ",
                params![
                    settings.display_name,
                    i64::from(settings.port),
                    encode_json("model_gateway_policy_encode_failed", &settings.policy)?,
                    settings.revision,
                    updated_at_ms,
                    SETTINGS_ID,
                    expected_revision,
                ],
            )
            .map_err(storage_err(
                "model_gateway_settings_update_failed",
                "failed to update gateway settings",
            ))?;
        if changed == 0 {
            return Err(VibexError::conflict(
                "model_gateway_settings_revision_conflict",
                "gateway settings changed since they were read",
            ));
        }
        let mut stored = settings.clone();
        stored.updated_at_ms = updated_at_ms;
        Ok(stored)
    }

    /// Persists only the port the listener actually bound.
    ///
    /// This is not a user edit and must not advance the user-visible revision,
    /// because a revision bump would invalidate every editor holding the row.
    pub fn store_bound_port(conn: &Connection, port: u16) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE model_gateway_settings
            SET port = ?1, updated_at_ms = ?2
            WHERE settings_id = ?3
            ",
            params![i64::from(port), unix_timestamp_ms(), SETTINGS_ID],
        )
        .map_err(storage_err(
            "model_gateway_port_store_failed",
            "failed to persist the gateway port",
        ))?;
        Ok(())
    }
}

pub struct ModelProviderAccountStateRepository;

impl ModelProviderAccountStateRepository {
    pub fn list(
        conn: &Connection,
        profile: &ModelProviderProfileId,
    ) -> VibexResult<Vec<ModelProviderAccountState>> {
        let mut statement = conn
            .prepare(
                "
                SELECT credential_id, enabled, priority, cooldown_until_ms,
                       cooldown_reason, consecutive_failures, updated_at_ms
                FROM model_provider_account_state
                WHERE model_provider_profile_id = ?1
                ORDER BY priority ASC, credential_id ASC
                ",
            )
            .map_err(storage_err(
                "model_gateway_account_list_failed",
                "failed to read gateway account state",
            ))?;
        let rows = statement
            .query_map(params![profile.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(storage_err(
                "model_gateway_account_list_failed",
                "failed to read gateway account state",
            ))?;
        let mut states = Vec::new();
        for row in rows {
            let (
                credential_id,
                enabled,
                priority,
                cooldown_until_ms,
                cooldown_reason,
                consecutive_failures,
                updated_at_ms,
            ) = row.map_err(storage_err(
                "model_gateway_account_list_failed",
                "failed to read gateway account state",
            ))?;
            states.push(ModelProviderAccountState {
                model_provider_profile_id: profile.clone(),
                credential_id: RequestId::parse(credential_id)?,
                enabled: enabled != 0,
                priority,
                cooldown_until_ms,
                cooldown_reason: cooldown_reason
                    .as_deref()
                    .and_then(GatewayFailureKind::from_id),
                consecutive_failures,
                updated_at_ms,
            });
        }
        Ok(states)
    }

    pub fn upsert(conn: &Connection, state: &ModelProviderAccountState) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO model_provider_account_state (
                model_provider_profile_id, credential_id, enabled, priority,
                cooldown_until_ms, cooldown_reason, consecutive_failures, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(model_provider_profile_id, credential_id) DO UPDATE SET
                enabled = excluded.enabled,
                priority = excluded.priority,
                cooldown_until_ms = excluded.cooldown_until_ms,
                cooldown_reason = excluded.cooldown_reason,
                consecutive_failures = excluded.consecutive_failures,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                state.model_provider_profile_id.as_str(),
                state.credential_id.as_str(),
                i64::from(state.enabled),
                state.priority,
                state.cooldown_until_ms,
                state.cooldown_reason.map(GatewayFailureKind::id),
                state.consecutive_failures,
                state.updated_at_ms,
            ],
        )
        .map_err(storage_err(
            "model_gateway_account_write_failed",
            "failed to write gateway account state",
        ))?;
        Ok(())
    }

    /// Marks one Key as working again after a successful reply.
    pub fn clear_cooldown(
        conn: &Connection,
        profile: &ModelProviderProfileId,
        credential: &RequestId,
    ) -> VibexResult<()> {
        conn.execute(
            "
            UPDATE model_provider_account_state
            SET cooldown_until_ms = NULL,
                cooldown_reason = NULL,
                consecutive_failures = 0,
                updated_at_ms = ?1
            WHERE model_provider_profile_id = ?2 AND credential_id = ?3
            ",
            params![unix_timestamp_ms(), profile.as_str(), credential.as_str(),],
        )
        .map_err(storage_err(
            "model_gateway_account_write_failed",
            "failed to clear gateway account cooldown",
        ))?;
        Ok(())
    }

    /// Applies one user setting to a Key, creating its state row if this is the
    /// first thing that ever touched it.
    ///
    /// Re-enabling clears the cooldown: the user just asked for this Key to be
    /// tried, and an authentication failure otherwise never expires, which
    /// would make the switch look broken after the account was repaired.
    pub fn apply_update(
        conn: &Connection,
        update: &vibex_core::GatewayAccountUpdate,
        now_ms: i64,
    ) -> VibexResult<ModelProviderAccountState> {
        let mut state = Self::list(conn, &update.model_provider_profile_id)?
            .into_iter()
            .find(|state| state.credential_id == update.credential_id)
            .unwrap_or_else(|| {
                ModelProviderAccountState::ready(
                    update.model_provider_profile_id.clone(),
                    update.credential_id.clone(),
                )
            });
        if let Some(enabled) = update.enabled {
            state.enabled = enabled;
            if enabled {
                state.cooldown_until_ms = None;
                state.cooldown_reason = None;
                state.consecutive_failures = 0;
            }
        }
        state.updated_at_ms = now_ms;
        Self::upsert(conn, &state)?;
        Ok(state)
    }
}

pub struct GatewayTokenWindowRepository;

impl GatewayTokenWindowRepository {
    /// Adds tokens observed for one Provider in the window containing `now_ms`.
    pub fn record(
        conn: &Connection,
        profile: &ModelProviderProfileId,
        window: GatewayTokenWindow,
        tokens: u64,
        now_ms: i64,
    ) -> VibexResult<GatewayTokenWindowUsage> {
        let start = window_start(window, now_ms);
        let end = start.saturating_add(window.duration_ms());
        let window_id = format!("{}:{}:{}", profile.as_str(), window.id(), start);
        conn.execute(
            "
            INSERT INTO model_provider_token_window (
                window_id, model_provider_profile_id, window_kind,
                window_start_ms, window_end_ms, used_tokens
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(window_id) DO UPDATE SET
                used_tokens = used_tokens + excluded.used_tokens
            ",
            params![
                window_id,
                profile.as_str(),
                window.id(),
                start,
                end,
                i64::try_from(tokens).unwrap_or(i64::MAX),
            ],
        )
        .map_err(storage_err(
            "model_gateway_token_window_write_failed",
            "failed to record gateway token usage",
        ))?;
        Self::current(conn, profile, window, now_ms)?.ok_or_else(|| {
            VibexError::storage(
                "model_gateway_token_window_readback_failed",
                "failed to read the token window that was just written",
            )
        })
    }

    pub fn current(
        conn: &Connection,
        profile: &ModelProviderProfileId,
        window: GatewayTokenWindow,
        now_ms: i64,
    ) -> VibexResult<Option<GatewayTokenWindowUsage>> {
        let start = window_start(window, now_ms);
        let used = conn
            .query_row(
                "
                SELECT used_tokens, window_end_ms
                FROM model_provider_token_window
                WHERE model_provider_profile_id = ?1
                  AND window_kind = ?2
                  AND window_start_ms = ?3
                ",
                params![profile.as_str(), window.id(), start],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(storage_err(
                "model_gateway_token_window_read_failed",
                "failed to read gateway token usage",
            ))?;
        Ok(
            used.map(|(used_tokens, window_end_ms)| GatewayTokenWindowUsage {
                model_provider_profile_id: profile.clone(),
                window,
                window_start_ms: start,
                window_end_ms,
                used_tokens: u64::try_from(used_tokens).unwrap_or_default(),
            }),
        )
    }
}

/// The start of the window containing `now_ms`, aligned to local midnight.
fn window_start(window: GatewayTokenWindow, now_ms: i64) -> i64 {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let days = now_ms.div_euclid(DAY_MS);
    match window {
        GatewayTokenWindow::Day => days * DAY_MS,
        // A week starts on the day the epoch did, a month on a fixed 30-day
        // boundary: the ceiling is a local guardrail, not a vendor calendar,
        // and a fixed-length window is what makes it explainable.
        GatewayTokenWindow::Week => days.div_euclid(7) * 7 * DAY_MS,
        GatewayTokenWindow::Month => days.div_euclid(30) * 30 * DAY_MS,
    }
}

pub struct GatewayConversationPinRepository;

impl GatewayConversationPinRepository {
    pub fn get(conn: &Connection, key: &str) -> VibexResult<Option<GatewayConversationPin>> {
        conn.query_row(
            "
            SELECT model_provider_profile_id, credential_id, chosen_at_ms, expires_at_ms
            FROM model_gateway_conversation_pin
            WHERE conversation_key = ?1
            ",
            params![key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(storage_err(
            "model_gateway_pin_read_failed",
            "failed to read the gateway conversation pin",
        ))?
        .map(|(profile, credential, chosen_at_ms, expires_at_ms)| {
            Ok(GatewayConversationPin {
                conversation_key: key.to_string(),
                model_provider_profile_id: ModelProviderProfileId::parse(profile)?,
                credential_id: RequestId::parse(credential)?,
                chosen_at_ms,
                expires_at_ms,
            })
        })
        .transpose()
    }

    pub fn upsert(conn: &Connection, pin: &GatewayConversationPin) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO model_gateway_conversation_pin (
                conversation_key, model_provider_profile_id, credential_id,
                chosen_at_ms, expires_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(conversation_key) DO UPDATE SET
                model_provider_profile_id = excluded.model_provider_profile_id,
                credential_id = excluded.credential_id,
                chosen_at_ms = excluded.chosen_at_ms,
                expires_at_ms = excluded.expires_at_ms
            ",
            params![
                pin.conversation_key,
                pin.model_provider_profile_id.as_str(),
                pin.credential_id.as_str(),
                pin.chosen_at_ms,
                pin.expires_at_ms,
            ],
        )
        .map_err(storage_err(
            "model_gateway_pin_write_failed",
            "failed to write the gateway conversation pin",
        ))?;
        Ok(())
    }

    pub fn delete(conn: &Connection, key: &str) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM model_gateway_conversation_pin WHERE conversation_key = ?1",
            params![key],
        )
        .map_err(storage_err(
            "model_gateway_pin_delete_failed",
            "failed to delete the gateway conversation pin",
        ))?;
        Ok(())
    }

    /// Drops expired pins, and the oldest ones above `keep`.
    pub fn prune(conn: &Connection, now_ms: i64, keep: usize) -> VibexResult<usize> {
        let mut removed = conn
            .execute(
                "DELETE FROM model_gateway_conversation_pin WHERE expires_at_ms <= ?1",
                params![now_ms],
            )
            .map_err(storage_err(
                "model_gateway_pin_prune_failed",
                "failed to prune the gateway conversation pins",
            ))?;
        removed += conn
            .execute(
                "
                DELETE FROM model_gateway_conversation_pin
                WHERE conversation_key IN (
                    SELECT conversation_key
                    FROM model_gateway_conversation_pin
                    ORDER BY chosen_at_ms DESC
                    LIMIT -1 OFFSET ?1
                )
                ",
                params![i64::try_from(keep).unwrap_or(i64::MAX)],
            )
            .map_err(storage_err(
                "model_gateway_pin_prune_failed",
                "failed to prune the gateway conversation pins",
            ))?;
        Ok(removed)
    }
}

pub struct ModelGatewayRequestLogRepository;

impl ModelGatewayRequestLogRepository {
    pub fn insert(conn: &Connection, log: &ModelGatewayRequestLog) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO model_gateway_request_log (
                request_id, agent_id, model_provider_profile_id, credential_id,
                model_requested, model_served, wire_in, wire_out, status,
                input_tokens, output_tokens, cached_read_tokens, cached_write_tokens,
                reasoning_tokens, latency_ms, outcome, route_trace, created_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
            ",
            params![
                log.request_id.as_str(),
                log.agent_id,
                log.model_provider_profile_id.as_str(),
                log.credential_id.as_ref().map(RequestId::as_str),
                log.model_requested,
                log.model_served,
                log.wire_in,
                log.wire_out,
                log.status.map(i64::from),
                i64::try_from(log.input_tokens).unwrap_or(i64::MAX),
                i64::try_from(log.output_tokens).unwrap_or(i64::MAX),
                i64::try_from(log.cached_read_tokens).unwrap_or(i64::MAX),
                i64::try_from(log.cached_write_tokens).unwrap_or(i64::MAX),
                i64::try_from(log.reasoning_tokens).unwrap_or(i64::MAX),
                log.latency_ms,
                log.outcome.id(),
                log.route
                    .as_ref()
                    .map(|trace| encode_json("model_gateway_route_encode_failed", trace))
                    .transpose()?,
                log.created_at_ms,
            ],
        )
        .map_err(storage_err(
            "model_gateway_request_log_write_failed",
            "failed to write the gateway request log",
        ))?;
        Ok(())
    }

    /// The newest requests, for the Config Center's request list.
    pub fn recent(conn: &Connection, limit: usize) -> VibexResult<Vec<ModelGatewayRequestLog>> {
        Self::query(conn, None, None, None, limit)
    }

    /// One bounded page of ledger rows, newest first.
    pub fn page(
        conn: &Connection,
        query: &GatewayRequestQuery,
    ) -> VibexResult<Vec<ModelGatewayRequestLog>> {
        Self::query(
            conn,
            query.since_ms,
            query.until_ms,
            query.before.as_ref(),
            query.limit.clamp(1, GATEWAY_REQUEST_PAGE_MAX),
        )
    }

    fn query(
        conn: &Connection,
        since_ms: Option<i64>,
        until_ms: Option<i64>,
        before: Option<&GatewayRequestCursor>,
        limit: usize,
    ) -> VibexResult<Vec<ModelGatewayRequestLog>> {
        let mut statement = conn
            .prepare(
                "
                SELECT request_id, agent_id, model_provider_profile_id, credential_id,
                       model_requested, model_served, wire_in, wire_out, status,
                       input_tokens, output_tokens, cached_read_tokens, cached_write_tokens,
                       reasoning_tokens, latency_ms, outcome, route_trace, created_at_ms
                FROM model_gateway_request_log
                WHERE (?1 IS NULL OR created_at_ms >= ?1)
                  AND (?2 IS NULL OR created_at_ms < ?2)
                  AND (
                      ?3 IS NULL
                      OR created_at_ms < ?3
                      OR (created_at_ms = ?3 AND request_id > ?4)
                  )
                ORDER BY created_at_ms DESC, request_id ASC
                LIMIT ?5
                ",
            )
            .map_err(storage_err(
                "model_gateway_request_log_read_failed",
                "failed to read the gateway request log",
            ))?;
        let rows = statement
            .query_map(
                params![
                    since_ms,
                    until_ms,
                    before.map(|cursor| cursor.created_at_ms),
                    before.map(|cursor| cursor.request_id.as_str()),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                        row.get::<_, i64>(13)?,
                        row.get::<_, Option<i64>>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, Option<String>>(16)?,
                        row.get::<_, i64>(17)?,
                    ))
                },
            )
            .map_err(storage_err(
                "model_gateway_request_log_read_failed",
                "failed to read the gateway request log",
            ))?;
        let mut logs = Vec::new();
        for row in rows {
            let row = row.map_err(storage_err(
                "model_gateway_request_log_read_failed",
                "failed to read the gateway request log",
            ))?;
            logs.push(ModelGatewayRequestLog {
                request_id: RequestId::parse(row.0)?,
                agent_id: row.1,
                model_provider_profile_id: ModelProviderProfileId::parse(row.2)?,
                credential_id: row.3.map(RequestId::parse).transpose()?,
                model_requested: row.4,
                model_served: row.5,
                wire_in: row.6,
                wire_out: row.7,
                status: row.8.and_then(|status| u16::try_from(status).ok()),
                input_tokens: u64::try_from(row.9).unwrap_or_default(),
                output_tokens: u64::try_from(row.10).unwrap_or_default(),
                cached_read_tokens: u64::try_from(row.11).unwrap_or_default(),
                cached_write_tokens: u64::try_from(row.12).unwrap_or_default(),
                reasoning_tokens: u64::try_from(row.13).unwrap_or_default(),
                latency_ms: row.14,
                outcome: GatewayRequestOutcome::from_id(&row.15)
                    .unwrap_or(GatewayRequestOutcome::Failed),
                route: row.16.as_deref().and_then(read_route_trace),
                created_at_ms: row.17,
            });
        }
        Ok(logs)
    }

    /// Drops ledger rows older than the retention window.
    pub fn prune(conn: &Connection, now_ms: i64) -> VibexResult<usize> {
        let cutoff = now_ms.saturating_sub(ModelGatewayRequestLog::RETENTION_MS);
        conn.execute(
            "DELETE FROM model_gateway_request_log WHERE created_at_ms < ?1",
            params![cutoff],
        )
        .map_err(storage_err(
            "model_gateway_request_log_prune_failed",
            "failed to prune the gateway request log",
        ))
    }

    /// Deletes every ledger row, as the settings action asks.
    pub fn clear(conn: &Connection) -> VibexResult<usize> {
        conn.execute("DELETE FROM model_gateway_request_log", [])
            .map_err(storage_err(
                "model_gateway_request_log_clear_failed",
                "failed to clear the gateway request log",
            ))
    }
}

/// Creates the gateway tables.
pub(crate) fn migrate(conn: &mut DbConnection, applied: &mut Vec<String>) -> VibexResult<()> {
    const VERSION: i64 = 64;
    const NAME: &str = "model_gateway";
    if crate::migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start the model gateway migration transaction",
    ))?;
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS model_gateway_settings (
            settings_id INTEGER PRIMARY KEY CHECK(settings_id = 1),
            display_name TEXT NOT NULL,
            port INTEGER NOT NULL CHECK(port >= 0 AND port <= 65535),
            upstreams_json TEXT NOT NULL DEFAULT '[]',
            policy_json TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision > 0),
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS model_provider_account_state (
            model_provider_profile_id TEXT NOT NULL
                REFERENCES model_provider_profiles(model_provider_profile_id)
                ON DELETE CASCADE,
            credential_id TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            priority INTEGER NOT NULL DEFAULT 0,
            cooldown_until_ms INTEGER NULL,
            cooldown_reason TEXT NULL,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (model_provider_profile_id, credential_id)
        );
        CREATE INDEX IF NOT EXISTS idx_model_provider_account_state_ready
            ON model_provider_account_state(
                model_provider_profile_id, enabled, cooldown_until_ms, priority
            );

        CREATE TABLE IF NOT EXISTS model_provider_token_window (
            window_id TEXT PRIMARY KEY,
            model_provider_profile_id TEXT NOT NULL
                REFERENCES model_provider_profiles(model_provider_profile_id)
                ON DELETE CASCADE,
            window_kind TEXT NOT NULL,
            window_start_ms INTEGER NOT NULL,
            window_end_ms INTEGER NOT NULL,
            limit_tokens INTEGER NULL CHECK(limit_tokens IS NULL OR limit_tokens > 0),
            used_tokens INTEGER NOT NULL DEFAULT 0 CHECK(used_tokens >= 0)
        );
        CREATE INDEX IF NOT EXISTS idx_model_provider_token_window_current
            ON model_provider_token_window(
                model_provider_profile_id, window_kind, window_start_ms
            );

        CREATE TABLE IF NOT EXISTS model_gateway_conversation_pin (
            conversation_key TEXT PRIMARY KEY,
            model_provider_profile_id TEXT NOT NULL
                REFERENCES model_provider_profiles(model_provider_profile_id)
                ON DELETE CASCADE,
            credential_id TEXT NOT NULL,
            chosen_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_model_gateway_conversation_pin_expiry
            ON model_gateway_conversation_pin(expires_at_ms, chosen_at_ms);

        CREATE TABLE IF NOT EXISTS model_gateway_request_log (
            request_id TEXT PRIMARY KEY,
            agent_id TEXT NULL,
            model_provider_profile_id TEXT NOT NULL,
            credential_id TEXT NULL,
            model_requested TEXT NOT NULL,
            model_served TEXT NULL,
            wire_in TEXT NOT NULL,
            wire_out TEXT NOT NULL,
            status INTEGER NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cached_read_tokens INTEGER NOT NULL DEFAULT 0,
            cached_write_tokens INTEGER NOT NULL DEFAULT 0,
            reasoning_tokens INTEGER NOT NULL DEFAULT 0,
            latency_ms INTEGER NULL,
            outcome TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_model_gateway_request_log_recent
            ON model_gateway_request_log(created_at_ms DESC);
        ",
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to create the model gateway tables",
    ))?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record the model gateway migration",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit the model gateway migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Adds the route trace to the request ledger.
///
/// The trace answers the only question a failover leaves behind: which Key was
/// set aside, and why the request moved on. It is additive and nullable, so a
/// database written before it keeps every existing row and simply has no trace
/// for them. `ALTER TABLE ... ADD COLUMN` is not idempotent, so the version
/// check is the guard.
pub(crate) fn migrate_route_trace(
    conn: &mut DbConnection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 65;
    const NAME: &str = "model_gateway_route_trace";
    if crate::migration_applied(conn, VERSION)? {
        return Ok(());
    }
    conn.execute_batch("ALTER TABLE model_gateway_request_log ADD COLUMN route_trace TEXT NULL;")
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to add the gateway request route trace",
        ))?;
    conn.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record the gateway route trace migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Records the Agents that have been switched to the gateway.
///
/// The list used to be derivable from the hidden Profiles alone, which made
/// turning the gateway off lose the user's choice. It is now durable, so a
/// listener that is down can remove the Profiles (Agents fall back to direct)
/// without forgetting which Agents should walk through the gateway once it is
/// back.
pub(crate) fn migrate_gateway_agents(
    conn: &mut DbConnection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 66;
    const NAME: &str = "model_gateway_agents";
    if crate::migration_applied(conn, VERSION)? {
        return Ok(());
    }
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS model_gateway_agents (
            agent_id TEXT PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 1,
            updated_at_ms INTEGER NOT NULL
        );
        ",
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to create the gateway Agent table",
    ))?;
    // Adopt the hidden Profiles that already exist, so an upgrade keeps every
    // Agent the user had pointed at the gateway.
    conn.execute(
        "
        INSERT OR IGNORE INTO model_gateway_agents (agent_id, enabled, updated_at_ms)
        SELECT json_extract(entry.value, '$.value'), 1, ?1
        FROM provider_profiles profile, json_each(profile.provider_options_json, '$.entries') entry
        WHERE profile.deleted_at_ms IS NULL
          AND json_extract(entry.value, '$.key') = 'vibex.internal.profileRole'
          AND json_extract(entry.value, '$.value') = 'model_gateway'
        ",
        params![unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to adopt the existing gateway Agents",
    ))?;
    conn.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms()],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record the gateway Agent migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Retires the two inputs the gateway used to configure itself with.
///
/// The gateway is now a transport for route groups: it holds no Provider list
/// and no per-Agent switch. Both had to go, not merely stop being read —
/// leaving the rows behind would let a later reader believe they still mean
/// something.
///
/// - `model_gateway_settings.upstreams_json` is cleared. The column stays for
///   schema compatibility with older binaries, but nothing writes it again.
/// - Every `kind = 'gateway'` route is soft-deleted along with its bindings.
///   That shape existed only to express the global upstream list as one
///   callable route; with the list gone it has no members to describe and no
///   way to be re-created.
/// - `model_gateway_agents` is dropped. An Agent reaches the gateway by binding
///   a route group, which is the one switch and lives with the route.
pub(crate) fn migrate_retire_gateway_inputs(
    conn: &mut DbConnection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 77;
    const NAME: &str = "retire_gateway_inputs";
    if crate::migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start the gateway inputs retirement transaction",
    ))?;
    let now = unix_timestamp_ms().max(1);
    tx.execute(
        "UPDATE model_gateway_settings SET upstreams_json = '[]'",
        [],
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to clear the gateway upstream list",
    ))?;
    // Soft-delete the plain gateway routes first, so their bindings can be
    // retired by route id rather than by a join the next statement would have
    // to repeat.
    let retired: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT model_route_id FROM model_routes WHERE kind = 'gateway'")
            .map_err(storage_err(
                "migration_apply_failed",
                "failed to read the retired gateway routes",
            ))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err(
                "migration_apply_failed",
                "failed to read the retired gateway routes",
            ))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(storage_err(
                "migration_apply_failed",
                "failed to read a retired gateway route",
            ))?);
        }
        ids
    };
    tx.execute(
        "UPDATE model_routes SET deleted_at_ms = COALESCE(deleted_at_ms, ?1), updated_at_ms = ?1 \
         WHERE kind = 'gateway'",
        params![now],
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to retire the gateway routes",
    ))?;
    for route_id in &retired {
        tx.execute(
            "UPDATE agent_model_route_bindings \
             SET deleted_at_ms = COALESCE(deleted_at_ms, ?2), updated_at_ms = ?2, is_default = 0 \
             WHERE model_route_id = ?1",
            params![route_id, now],
        )
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to retire a gateway route binding",
        ))?;
    }
    tx.execute_batch("DROP TABLE IF EXISTS model_gateway_agents;")
        .map_err(storage_err(
            "migration_apply_failed",
            "failed to drop the gateway Agent table",
        ))?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, now],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record the gateway inputs retirement",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_transaction_failed",
        "failed to commit the gateway inputs retirement",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Which wire protocol each untyped endpoint was observed to answer.
///
/// An endpoint that declares no protocol is an address whose API is unknown, so
/// the gateway tries one and keeps the answer here. It is a cache of observed
/// behaviour, never of configuration: deleting a row only costs one probe.
pub struct GatewayEndpointProtocolRepository;

impl GatewayEndpointProtocolRepository {
    pub fn get(
        conn: &Connection,
        profile: &ModelProviderProfileId,
        endpoint_id: &str,
    ) -> VibexResult<Option<String>> {
        conn.query_row(
            "SELECT wire_protocol_id FROM model_gateway_endpoint_protocol \
             WHERE model_provider_profile_id = ?1 AND endpoint_id = ?2",
            params![profile.as_str(), endpoint_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_err(
            "model_gateway_endpoint_protocol_read_failed",
            "failed to read an endpoint's learned protocol",
        ))
    }

    pub fn remember(
        conn: &Connection,
        profile: &ModelProviderProfileId,
        endpoint_id: &str,
        wire_protocol_id: &str,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO model_gateway_endpoint_protocol (
                model_provider_profile_id, endpoint_id, wire_protocol_id, learned_at_ms
             )
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(model_provider_profile_id, endpoint_id)
             DO UPDATE SET wire_protocol_id = excluded.wire_protocol_id,
                           learned_at_ms = excluded.learned_at_ms",
            params![profile.as_str(), endpoint_id, wire_protocol_id, now_ms],
        )
        .map_err(storage_err(
            "model_gateway_endpoint_protocol_write_failed",
            "failed to remember an endpoint's protocol",
        ))?;
        Ok(())
    }

    /// Forgets a verdict, so a corrected address is probed again.
    pub fn forget(
        conn: &Connection,
        profile: &ModelProviderProfileId,
        endpoint_id: &str,
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM model_gateway_endpoint_protocol \
             WHERE model_provider_profile_id = ?1 AND endpoint_id = ?2",
            params![profile.as_str(), endpoint_id],
        )
        .map_err(storage_err(
            "model_gateway_endpoint_protocol_delete_failed",
            "failed to forget an endpoint's protocol",
        ))?;
        Ok(())
    }
}

/// Creates the table that remembers what an undeclared endpoint speaks.
pub(crate) fn migrate_endpoint_protocol(
    conn: &mut DbConnection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 79;
    const NAME: &str = "model_gateway_endpoint_protocol";
    if crate::migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err(
        "migration_transaction_failed",
        "failed to start the endpoint protocol migration transaction",
    ))?;
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS model_gateway_endpoint_protocol (
            model_provider_profile_id TEXT NOT NULL
                REFERENCES model_provider_profiles(model_provider_profile_id)
                ON DELETE CASCADE,
            endpoint_id TEXT NOT NULL,
            wire_protocol_id TEXT NOT NULL,
            learned_at_ms INTEGER NOT NULL,
            PRIMARY KEY (model_provider_profile_id, endpoint_id)
        );
        ",
    )
    .map_err(storage_err(
        "migration_apply_failed",
        "failed to create the endpoint protocol table",
    ))?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
        params![VERSION, NAME, unix_timestamp_ms().max(1)],
    )
    .map_err(storage_err(
        "migration_record_failed",
        "failed to record the endpoint protocol migration",
    ))?;
    tx.commit().map_err(storage_err(
        "migration_commit_failed",
        "failed to commit the endpoint protocol migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

fn encode_json<T: serde::Serialize>(code: &'static str, value: &T) -> VibexResult<String> {
    serde_json::to_string(value)
        .map_err(|_| VibexError::storage(code, "failed to encode gateway settings"))
}

fn decode_json<T: serde::de::DeserializeOwned>(
    code: &'static str,
    encoded: &str,
) -> VibexResult<T> {
    serde_json::from_str(encoded)
        .map_err(|_| VibexError::storage(code, "gateway settings hold an invalid policy"))
}

/// Reads a stored route trace.
///
/// The trace annotates a diagnostics row; nothing is routed from it. An
/// unreadable payload — a blob written by a newer build, or one that has been
/// tampered with — therefore reads as "no trace" rather than failing the whole
/// request list, and the row's own decision columns stay authoritative.
fn read_route_trace(encoded: &str) -> Option<GatewayRouteTrace> {
    serde_json::from_str::<GatewayRouteTrace>(encoded)
        .ok()
        .filter(GatewayRouteTrace::is_current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_migrations;
    use crate::tests_support::cleanup;

    fn database() -> (std::path::PathBuf, DbConnection) {
        let path = std::env::temp_dir().join(format!(
            "vibex-model-gateway-{}.db",
            RequestId::new().as_str()
        ));
        let mut conn = crate::open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        (path, conn)
    }

    /// A Provider to point the gateway at.
    ///
    /// It creates one rather than taking the first row: nothing is seeded any more,
    /// because a per-kind placeholder with no address was standing in for Providers
    /// the reader had not added.
    fn profile(conn: &Connection) -> ModelProviderProfileId {
        let profile = vibex_core::ModelProviderProfile {
            id: vibex_core::ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "fixture".to_string(),
            vendor_hint: None,
            endpoints: vec![vibex_core::ModelProviderEndpoint {
                id: "api".to_string(),
                kind: vibex_core::ModelProviderEndpointKind::Api,
                url: "https://api.example.invalid/v1".to_string(),
                wire_protocol_id: None,
            }],
            proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: Vec::new(),
            default_model_id: None,
            headers: Vec::new(),
            metadata: Default::default(),
            status: vibex_core::ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        };
        crate::ModelProviderProfileRepository::insert(conn, &profile).unwrap();
        profile.id
    }

    /// The retirement migration clears the two inputs the gateway used to
    /// configure itself with.
    ///
    /// It has to do more than stop reading them: a leftover upstream list or a
    /// per-Agent row would let a later reader believe they still mean
    /// something.
    #[test]
    fn migration_77_retires_the_gateway_inputs() {
        let (path, mut conn) = database();
        assert!(crate::migration_applied(&conn, 77).unwrap());
        // A fresh home has no gateway row until something reads it.
        ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();

        // The column survives for compatibility, but holds nothing.
        let stored: String = conn
            .query_row(
                "SELECT upstreams_json FROM model_gateway_settings WHERE settings_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "[]", "the upstream list must be cleared");

        // The per-Agent switch table is gone, not merely unused.
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'model_gateway_agents'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0, "the per-Agent gateway table must be dropped");

        // Re-applying changes nothing.
        let applied = apply_migrations(&mut conn).unwrap();
        assert!(
            !applied.iter().any(|entry| entry.starts_with("77:")),
            "the retirement migration must not run twice"
        );
        cleanup(path);
    }

    #[test]
    fn migration_64_creates_gateway_tables_and_is_idempotent() {
        let (path, mut conn) = database();
        assert_eq!(
            crate::current_schema_version(&conn).unwrap(),
            crate::CURRENT_SCHEMA_VERSION
        );
        assert!(crate::migration_applied(&conn, 64).unwrap());
        assert!(crate::migration_applied(&conn, 65).unwrap());
        assert!(crate::migration_applied(&conn, 66).unwrap());
        let applied = apply_migrations(&mut conn).unwrap();
        assert!(
            !applied.iter().any(|entry| entry.starts_with("64:")
                || entry.starts_with("65:")
                || entry.starts_with("66:")),
            "re-applying migrations must not re-run the gateway migrations"
        );
        let settings = ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();
        assert_eq!(settings.port, 0);
        cleanup(path);
    }

    #[test]
    fn settings_round_trip_and_reject_stale_revisions() {
        let (path, conn) = database();
        let settings = ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();
        let mut next = settings.clone();
        next.revision = settings.revision + 1;
        next.port = 3425;
        let stored =
            ModelGatewaySettingsRepository::update(&conn, &next, settings.revision).unwrap();
        assert_eq!(stored.port, 3425);

        // A caller that read the row before the update tries to advance from
        // the revision it observed. Its replacement has the right shape, so
        // only the compare-and-swap can reject it.
        let mut stale = settings.clone();
        stale.revision = settings.revision + 1;
        stale.port = 4000;
        let error =
            ModelGatewaySettingsRepository::update(&conn, &stale, settings.revision).unwrap_err();
        assert_eq!(error.code, "model_gateway_settings_revision_conflict");

        // A replacement that skips a revision is refused before any write.
        let mut skip = stored.clone();
        skip.revision = stored.revision + 2;
        let error =
            ModelGatewaySettingsRepository::update(&conn, &skip, stored.revision).unwrap_err();
        assert_eq!(error.code, "model_gateway_settings_revision_invalid");
        cleanup(path);
    }

    #[test]
    fn account_state_round_trips_and_clears() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let credential = RequestId::new();
        let mut state = ModelProviderAccountState::ready(upstream.clone(), credential.clone());
        state.cooldown_reason = Some(GatewayFailureKind::Rate);
        state.cooldown_until_ms = Some(unix_timestamp_ms() + 30_000);
        state.consecutive_failures = 2;
        ModelProviderAccountStateRepository::upsert(&conn, &state).unwrap();

        let states = ModelProviderAccountStateRepository::list(&conn, &upstream).unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].cooldown_reason, Some(GatewayFailureKind::Rate));
        assert!(!states[0].is_ready_at(unix_timestamp_ms()));

        ModelProviderAccountStateRepository::clear_cooldown(&conn, &upstream, &credential).unwrap();
        let states = ModelProviderAccountStateRepository::list(&conn, &upstream).unwrap();
        assert!(states[0].is_ready_at(unix_timestamp_ms()));
        assert_eq!(states[0].consecutive_failures, 0);
        cleanup(path);
    }

    #[test]
    fn a_key_can_be_turned_off_before_it_ever_fails() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let credential = RequestId::new();
        let update = |enabled| vibex_core::GatewayAccountUpdate {
            model_provider_profile_id: upstream.clone(),
            credential_id: credential.clone(),
            enabled: Some(enabled),
            preferred: None,
        };

        // No state row exists yet: the first thing that touched this Key is the
        // user turning it off, which must work rather than being a no-op.
        ModelProviderAccountStateRepository::apply_update(
            &conn,
            &update(false),
            unix_timestamp_ms(),
        )
        .unwrap();
        let states = ModelProviderAccountStateRepository::list(&conn, &upstream).unwrap();
        assert_eq!(states.len(), 1);
        assert!(!states[0].enabled);
        assert_eq!(
            states[0].visibility_at(unix_timestamp_ms()),
            vibex_core::GatewayKeyState::Disabled
        );

        ModelProviderAccountStateRepository::apply_update(
            &conn,
            &update(true),
            unix_timestamp_ms(),
        )
        .unwrap();
        let states = ModelProviderAccountStateRepository::list(&conn, &upstream).unwrap();
        assert!(states[0].enabled);
        assert_eq!(
            states[0].visibility_at(unix_timestamp_ms()),
            vibex_core::GatewayKeyState::Ready
        );
        cleanup(path);
    }

    /// Saying "use this Key again" is how an auth-rejected Key comes back: the
    /// failure itself never expires, so without clearing it the switch would
    /// look broken even after the account was repaired elsewhere.
    #[test]
    fn re_enabling_a_key_clears_an_authentication_failure() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let credential = RequestId::new();
        let mut state = ModelProviderAccountState::ready(upstream.clone(), credential.clone());
        state.cooldown_reason = Some(GatewayFailureKind::Auth);
        state.consecutive_failures = 1;
        ModelProviderAccountStateRepository::upsert(&conn, &state).unwrap();

        let update = vibex_core::GatewayAccountUpdate {
            model_provider_profile_id: upstream.clone(),
            credential_id: credential.clone(),
            enabled: Some(true),
            preferred: None,
        };
        ModelProviderAccountStateRepository::apply_update(&conn, &update, unix_timestamp_ms())
            .unwrap();
        let states = ModelProviderAccountStateRepository::list(&conn, &upstream).unwrap();
        assert!(states[0].is_ready_at(unix_timestamp_ms()));
        assert_eq!(states[0].consecutive_failures, 0);
        cleanup(path);
    }

    #[test]
    fn token_windows_accumulate_in_the_same_bucket_only() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let now = 1_700_000_000_000_i64;
        let first = GatewayTokenWindowRepository::record(
            &conn,
            &upstream,
            GatewayTokenWindow::Day,
            100,
            now,
        )
        .unwrap();
        let second = GatewayTokenWindowRepository::record(
            &conn,
            &upstream,
            GatewayTokenWindow::Day,
            50,
            now + 60_000,
        )
        .unwrap();
        assert_eq!(first.used_tokens, 100);
        assert_eq!(second.used_tokens, 150);
        assert_eq!(second.window_start_ms, first.window_start_ms);

        let next_day = GatewayTokenWindowRepository::record(
            &conn,
            &upstream,
            GatewayTokenWindow::Day,
            10,
            now + 24 * 60 * 60 * 1000,
        )
        .unwrap();
        assert_eq!(next_day.used_tokens, 10);
        assert_ne!(next_day.window_start_ms, first.window_start_ms);
        cleanup(path);
    }

    #[test]
    fn conversation_pins_expire_and_prune() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let now = unix_timestamp_ms();
        let live = GatewayConversationPin {
            conversation_key: "session-a".to_string(),
            model_provider_profile_id: upstream.clone(),
            credential_id: RequestId::new(),
            chosen_at_ms: now,
            expires_at_ms: now + GatewayConversationPin::TTL_MS,
        };
        let dead = GatewayConversationPin {
            conversation_key: "session-b".to_string(),
            chosen_at_ms: now,
            expires_at_ms: now - 1,
            ..live.clone()
        };
        GatewayConversationPinRepository::upsert(&conn, &live).unwrap();
        GatewayConversationPinRepository::upsert(&conn, &dead).unwrap();

        assert!(
            GatewayConversationPinRepository::get(&conn, "session-a")
                .unwrap()
                .unwrap()
                .is_live_at(now)
        );
        let removed = GatewayConversationPinRepository::prune(&conn, now, 512).unwrap();
        assert_eq!(removed, 1);
        assert!(
            GatewayConversationPinRepository::get(&conn, "session-b")
                .unwrap()
                .is_none()
        );
        cleanup(path);
    }

    #[test]
    fn request_log_holds_no_prompt_or_secret_and_prunes_after_a_week() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let now = unix_timestamp_ms();
        let mut log = ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: Some("codex".to_string()),
            model_provider_profile_id: upstream.clone(),
            credential_id: Some(RequestId::new()),
            model_requested: "deepseek/deepseek-chat".to_string(),
            model_served: Some("deepseek-chat".to_string()),
            wire_in: vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
            wire_out: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
            status: Some(200),
            input_tokens: 1024,
            output_tokens: 180,
            cached_read_tokens: 800,
            cached_write_tokens: 0,
            reasoning_tokens: 0,
            latency_ms: Some(42),
            outcome: GatewayRequestOutcome::Ok,
            route: Some(GatewayRouteTrace {
                schema_version: vibex_core::GATEWAY_ROUTE_TRACE_SCHEMA_VERSION,
                skipped_cooling: 1,
                skipped_over_limit: 0,
                failed_attempts: vec![vibex_core::GatewayRouteAttempt {
                    credential_id: RequestId::new(),
                    failure: GatewayFailureKind::Quota,
                }],
                skipped_members: Vec::new(),
                error_code: None,
                error_message: None,
                decision: Some(vibex_core::GatewayRouteDecisionTrace {
                    rule_index: Some(1),
                    rule_id: Some("rule-2".to_string()),
                    reason: Some("rule".to_string()),
                    effort: Some("high".to_string()),
                    ..Default::default()
                }),
            }),
            created_at_ms: now,
        };
        ModelGatewayRequestLogRepository::insert(&conn, &log).unwrap();
        log.request_id = RequestId::new();
        log.created_at_ms = now - ModelGatewayRequestLog::RETENTION_MS - 1;
        ModelGatewayRequestLogRepository::insert(&conn, &log).unwrap();

        let recent = ModelGatewayRequestLogRepository::recent(&conn, 10).unwrap();
        assert_eq!(recent.len(), 2);
        // The trace survives the round trip, and a reader that meets an
        // unreadable or newer payload reports the row as having no trace
        // instead of failing the list around it.
        let stored = recent[0].route.as_ref().expect("the trace round-trips");
        assert_eq!(stored.skipped_cooling, 1);
        assert_eq!(stored.failed_attempts.len(), 1);
        assert_eq!(stored.failed_attempts[0].failure, GatewayFailureKind::Quota);
        conn.execute(
            "UPDATE model_gateway_request_log SET route_trace = ?1 WHERE request_id = ?2",
            params![
                "{\"schemaVersion\":99,\"skippedCooling\":3}",
                log.request_id.as_str()
            ],
        )
        .unwrap();
        conn.execute(
            "UPDATE model_gateway_request_log SET route_trace = ?1 WHERE request_id != ?2",
            params!["not json at all", log.request_id.as_str()],
        )
        .unwrap();
        let recent = ModelGatewayRequestLogRepository::recent(&conn, 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().all(|row| row.route.is_none()));
        let encoded = serde_json::to_string(&recent).unwrap();
        for forbidden in ["sk-", "prompt", "reply", "base64", "cost", "price"] {
            assert!(
                !encoded.to_lowercase().contains(forbidden),
                "request log leaked `{forbidden}`"
            );
        }

        let removed = ModelGatewayRequestLogRepository::prune(&conn, now).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            ModelGatewayRequestLogRepository::recent(&conn, 10)
                .unwrap()
                .len(),
            1
        );
        ModelGatewayRequestLogRepository::clear(&conn).unwrap();
        assert!(
            ModelGatewayRequestLogRepository::recent(&conn, 10)
                .unwrap()
                .is_empty()
        );
        cleanup(path);
    }

    /// The ledger is the one table that grows without a user action: one row per
    /// gateway request. Ten thousand rows is a heavy weak of use, and the
    /// request list must still page from the index instead of loading them all.
    #[test]
    fn ten_thousand_request_rows_stay_pageable_in_order() {
        let (path, mut conn) = database();
        let upstream = profile(&conn);
        let now = unix_timestamp_ms();
        let transaction = conn.transaction().unwrap();
        for index in 0..10_000i64 {
            ModelGatewayRequestLogRepository::insert(
                &transaction,
                &ModelGatewayRequestLog {
                    request_id: RequestId::new(),
                    agent_id: Some("codex".to_string()),
                    model_provider_profile_id: upstream.clone(),
                    credential_id: None,
                    model_requested: "deepseek/deepseek-chat".to_string(),
                    model_served: Some("deepseek-chat".to_string()),
                    wire_in: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    wire_out: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    status: Some(200),
                    input_tokens: 1,
                    output_tokens: 1,
                    cached_read_tokens: 0,
                    cached_write_tokens: 0,
                    reasoning_tokens: 0,
                    latency_ms: Some(10),
                    outcome: GatewayRequestOutcome::Ok,
                    route: None,
                    created_at_ms: now - index,
                },
            )
            .unwrap();
        }
        transaction.commit().unwrap();

        let page = ModelGatewayRequestLogRepository::recent(&conn, 100).unwrap();
        assert_eq!(page.len(), 100);
        assert_eq!(page[0].created_at_ms, now);
        assert!(
            page.windows(2)
                .all(|pair| pair[0].created_at_ms > pair[1].created_at_ms),
            "the newest page must be ordered by recency"
        );
        assert!(
            ModelGatewayRequestLogRepository::recent(&conn, 10_001)
                .unwrap()
                .len()
                == 10_000
        );
        assert_eq!(
            ModelGatewayRequestLogRepository::clear(&conn).unwrap(),
            10_000
        );
        cleanup(path);
    }

    #[test]
    fn request_pages_respect_a_half_open_window_and_equal_timestamps() {
        let (path, conn) = database();
        let upstream = profile(&conn);
        let insert = |request_id: &str, created_at_ms: i64| {
            ModelGatewayRequestLogRepository::insert(
                &conn,
                &ModelGatewayRequestLog {
                    request_id: RequestId::parse(format!("request_page_{request_id}"))
                        .expect("a valid request id"),
                    agent_id: Some("codex".to_string()),
                    model_provider_profile_id: upstream.clone(),
                    credential_id: None,
                    model_requested: "deepseek/deepseek-chat".to_string(),
                    model_served: Some("deepseek-chat".to_string()),
                    wire_in: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    wire_out: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    status: Some(200),
                    input_tokens: 1,
                    output_tokens: 1,
                    cached_read_tokens: 0,
                    cached_write_tokens: 0,
                    reasoning_tokens: 0,
                    latency_ms: Some(10),
                    outcome: GatewayRequestOutcome::Ok,
                    route: None,
                    created_at_ms,
                },
            )
            .unwrap();
        };
        insert("outside_newer", 110);
        insert("equal_b", 100);
        insert("equal_a", 100);
        insert("middle", 90);
        insert("oldest", 80);
        insert("outside_older", 70);

        let first = ModelGatewayRequestLogRepository::page(
            &conn,
            &GatewayRequestQuery::in_window(80, 101).with_limit(2),
        )
        .unwrap();
        assert_eq!(
            first
                .iter()
                .map(|row| row.request_id.as_str())
                .collect::<Vec<_>>(),
            ["request_page_equal_a", "request_page_equal_b"]
        );

        let cursor = GatewayRequestCursor::new(first[1].created_at_ms, first[1].request_id.clone());
        let second = ModelGatewayRequestLogRepository::page(
            &conn,
            &GatewayRequestQuery::in_window(80, 101)
                .after(cursor)
                .with_limit(2),
        )
        .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|row| row.request_id.as_str())
                .collect::<Vec<_>>(),
            ["request_page_middle", "request_page_oldest"]
        );
        let cursor =
            GatewayRequestCursor::new(second[1].created_at_ms, second[1].request_id.clone());
        assert!(
            ModelGatewayRequestLogRepository::page(
                &conn,
                &GatewayRequestQuery::in_window(80, 101)
                    .after(cursor)
                    .with_limit(2),
            )
            .unwrap()
            .is_empty()
        );

        cleanup(path);
    }

    #[test]
    fn gateway_state_cannot_outlive_its_upstream_profile() {
        let (path, conn) = database();
        // Every gateway row that names an upstream points at a real profile,
        // so a pruned profile cannot leave routing state behind.
        let orphan = ModelProviderProfileId::new();
        let error = GatewayTokenWindowRepository::record(
            &conn,
            &orphan,
            GatewayTokenWindow::Day,
            5,
            unix_timestamp_ms(),
        )
        .unwrap_err();
        assert_eq!(error.code, "model_gateway_token_window_write_failed");

        let upstream = profile(&conn);
        GatewayTokenWindowRepository::record(
            &conn,
            &upstream,
            GatewayTokenWindow::Day,
            5,
            unix_timestamp_ms(),
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_provider_token_window WHERE model_provider_profile_id = ?1",
                params![upstream.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        cleanup(path);
    }
}

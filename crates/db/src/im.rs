//! Persistence for IM chat channels.
//!
//! These repositories keep the channel configuration, the paired external
//! identities, the chat-to-session bindings, and the one-shot approval action
//! records. Secret values never live here: a row only names the backend and the
//! lookup key the secret store resolves.

use rusqlite::{Connection, OptionalExtension, params};
use vibex_core::{
    ImActionId, ImBindingId, ImBindingStatus, ImChannelId, ImChannelKind, ImPairingId,
    ImPrincipalId, ImPrincipalRole, ImPrincipalStatus, VibexError, VibexResult, VibexSessionId,
};

use super::parse_id_sql;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImChannelRecord {
    pub channel_id: ImChannelId,
    pub display_name: String,
    pub kind: ImChannelKind,
    pub enabled: bool,
    pub config_json: String,
    pub event_filter_json: Option<String>,
    pub locale: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImChannelSecretRecord {
    pub channel_id: ImChannelId,
    pub secret_kind: String,
    pub backend: String,
    pub lookup_key: String,
    pub setup_state: String,
    pub redacted_hint: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImPrincipalRecord {
    pub principal_id: ImPrincipalId,
    pub channel_id: ImChannelId,
    pub external_user_id: String,
    pub display_name: String,
    pub role: ImPrincipalRole,
    pub status: ImPrincipalStatus,
    pub paired_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImPairingRecord {
    pub pairing_id: ImPairingId,
    pub channel_id: ImChannelId,
    pub code_hash: String,
    pub role: ImPrincipalRole,
    pub scope: String,
    pub target_chat_id: Option<String>,
    pub created_by_principal_id: Option<ImPrincipalId>,
    pub expires_at_ms: i64,
    pub claimed_principal_id: Option<ImPrincipalId>,
    pub created_at_ms: i64,
    pub claimed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImBindingRecord {
    pub binding_id: ImBindingId,
    pub channel_id: ImChannelId,
    pub external_chat_id: String,
    pub thread_key: String,
    pub thread_kind: Option<String>,
    pub session_id: VibexSessionId,
    pub created_by_principal_id: Option<ImPrincipalId>,
    pub status: ImBindingStatus,
    pub last_delivered_sequence: i64,
    pub title: Option<String>,
    pub title_sync_enabled: bool,
    pub provider_payload_json: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImApprovalCardRecord {
    pub card_id: String,
    pub binding_id: ImBindingId,
    pub session_id: VibexSessionId,
    pub request_id: String,
    pub external_message_id: Option<String>,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImActionRecord {
    pub action_id: ImActionId,
    pub short_code: String,
    pub card_id: Option<String>,
    pub channel_id: ImChannelId,
    pub binding_id: ImBindingId,
    pub kind: String,
    pub payload_json: String,
    pub expires_at_ms: i64,
    pub consumed_at_ms: Option<i64>,
    pub consumed_by_principal_id: Option<ImPrincipalId>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImMessageRecord {
    pub message_id: String,
    pub channel_id: ImChannelId,
    pub direction: String,
    pub provider_update_id: Option<String>,
    pub external_message_id: Option<String>,
    pub binding_id: Option<ImBindingId>,
    pub principal_id: Option<ImPrincipalId>,
    pub kind: String,
    pub content_preview: Option<String>,
    pub content_sha256: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImAuditRecord {
    pub audit_id: String,
    pub channel_id: ImChannelId,
    pub principal_id: Option<ImPrincipalId>,
    pub action: String,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub outcome: String,
    pub detail_json: Option<String>,
    pub created_at_ms: i64,
}

pub struct ImChannelRepository;

impl ImChannelRepository {
    pub fn list(conn: &Connection) -> VibexResult<Vec<ImChannelRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT channel_id, display_name, kind, enabled, config_json, event_filter_json,
                        locale, created_at_ms, updated_at_ms
                 FROM im_channels ORDER BY created_at_ms ASC",
            )
            .map_err(storage_error("im_channel_list_failed"))?;
        let rows = statement
            .query_map([], map_channel)
            .map_err(storage_error("im_channel_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_channel_list_failed"))
    }

    pub fn get(
        conn: &Connection,
        channel_id: &ImChannelId,
    ) -> VibexResult<Option<ImChannelRecord>> {
        conn.query_row(
            "SELECT channel_id, display_name, kind, enabled, config_json, event_filter_json,
                    locale, created_at_ms, updated_at_ms
             FROM im_channels WHERE channel_id = ?1",
            params![channel_id.as_str()],
            map_channel,
        )
        .optional()
        .map_err(storage_error("im_channel_lookup_failed"))
    }

    pub fn insert(conn: &Connection, record: &ImChannelRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_channels (
                channel_id, display_name, kind, enabled, config_json, event_filter_json,
                locale, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.channel_id.as_str(),
                record.display_name,
                record.kind.as_str(),
                record.enabled,
                record.config_json,
                record.event_filter_json,
                record.locale,
                record.created_at_ms,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_error("im_channel_insert_failed"))?;
        Ok(())
    }

    pub fn update(conn: &Connection, record: &ImChannelRecord) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_channels
             SET display_name = ?2, kind = ?3, config_json = ?4, event_filter_json = ?5,
                 locale = ?6, updated_at_ms = ?7
             WHERE channel_id = ?1",
            params![
                record.channel_id.as_str(),
                record.display_name,
                record.kind.as_str(),
                record.config_json,
                record.event_filter_json,
                record.locale,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_error("im_channel_update_failed"))?;
        Ok(())
    }

    pub fn set_enabled(
        conn: &Connection,
        channel_id: &ImChannelId,
        enabled: bool,
        updated_at_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_channels SET enabled = ?2, updated_at_ms = ?3 WHERE channel_id = ?1",
            params![channel_id.as_str(), enabled, updated_at_ms],
        )
        .map_err(storage_error("im_channel_enable_failed"))?;
        Ok(())
    }

    pub fn delete(conn: &Connection, channel_id: &ImChannelId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM im_channels WHERE channel_id = ?1",
            params![channel_id.as_str()],
        )
        .map_err(storage_error("im_channel_delete_failed"))?;
        Ok(())
    }
}

pub struct ImChannelSecretRepository;

impl ImChannelSecretRepository {
    pub fn upsert(conn: &Connection, record: &ImChannelSecretRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_channel_secrets (
                channel_id, secret_kind, backend, lookup_key, setup_state, redacted_hint,
                updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(channel_id, secret_kind) DO UPDATE SET
                backend = excluded.backend,
                lookup_key = excluded.lookup_key,
                setup_state = excluded.setup_state,
                redacted_hint = excluded.redacted_hint,
                updated_at_ms = excluded.updated_at_ms",
            params![
                record.channel_id.as_str(),
                record.secret_kind,
                record.backend,
                record.lookup_key,
                record.setup_state,
                record.redacted_hint,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_error("im_secret_upsert_failed"))?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        channel_id: &ImChannelId,
        secret_kind: &str,
    ) -> VibexResult<Option<ImChannelSecretRecord>> {
        conn.query_row(
            "SELECT channel_id, secret_kind, backend, lookup_key, setup_state, redacted_hint,
                    updated_at_ms
             FROM im_channel_secrets WHERE channel_id = ?1 AND secret_kind = ?2",
            params![channel_id.as_str(), secret_kind],
            map_secret,
        )
        .optional()
        .map_err(storage_error("im_secret_lookup_failed"))
    }

    pub fn list_for_channel(
        conn: &Connection,
        channel_id: &ImChannelId,
    ) -> VibexResult<Vec<ImChannelSecretRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT channel_id, secret_kind, backend, lookup_key, setup_state, redacted_hint,
                        updated_at_ms
                 FROM im_channel_secrets WHERE channel_id = ?1 ORDER BY secret_kind ASC",
            )
            .map_err(storage_error("im_secret_list_failed"))?;
        let rows = statement
            .query_map(params![channel_id.as_str()], map_secret)
            .map_err(storage_error("im_secret_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_secret_list_failed"))
    }

    pub fn delete_for_channel(conn: &Connection, channel_id: &ImChannelId) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM im_channel_secrets WHERE channel_id = ?1",
            params![channel_id.as_str()],
        )
        .map_err(storage_error("im_secret_delete_failed"))?;
        Ok(())
    }
}

pub struct ImPrincipalRepository;

impl ImPrincipalRepository {
    pub fn get(
        conn: &Connection,
        principal_id: &ImPrincipalId,
    ) -> VibexResult<Option<ImPrincipalRecord>> {
        conn.query_row(
            "SELECT principal_id, channel_id, external_user_id, display_name, role, status,
                    paired_at_ms, last_seen_at_ms, revoked_at_ms, created_at_ms, updated_at_ms
             FROM im_principals WHERE principal_id = ?1",
            params![principal_id.as_str()],
            map_principal,
        )
        .optional()
        .map_err(storage_error("im_principal_lookup_failed"))
    }

    pub fn get_by_external(
        conn: &Connection,
        channel_id: &ImChannelId,
        external_user_id: &str,
    ) -> VibexResult<Option<ImPrincipalRecord>> {
        conn.query_row(
            "SELECT principal_id, channel_id, external_user_id, display_name, role, status,
                    paired_at_ms, last_seen_at_ms, revoked_at_ms, created_at_ms, updated_at_ms
             FROM im_principals WHERE channel_id = ?1 AND external_user_id = ?2",
            params![channel_id.as_str(), external_user_id],
            map_principal,
        )
        .optional()
        .map_err(storage_error("im_principal_lookup_failed"))
    }

    pub fn upsert_active(conn: &Connection, record: &ImPrincipalRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_principals (
                principal_id, channel_id, external_user_id, display_name, role, status,
                paired_at_ms, last_seen_at_ms, revoked_at_ms, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(channel_id, external_user_id) DO UPDATE SET
                display_name = excluded.display_name,
                role = excluded.role,
                status = 'active',
                paired_at_ms = excluded.paired_at_ms,
                last_seen_at_ms = excluded.last_seen_at_ms,
                revoked_at_ms = NULL,
                updated_at_ms = excluded.updated_at_ms",
            params![
                record.principal_id.as_str(),
                record.channel_id.as_str(),
                record.external_user_id,
                record.display_name,
                record.role.as_str(),
                record.status.as_str(),
                record.paired_at_ms,
                record.last_seen_at_ms,
                record.revoked_at_ms,
                record.created_at_ms,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_error("im_principal_upsert_failed"))?;
        Ok(())
    }

    pub fn list_for_channel(
        conn: &Connection,
        channel_id: &ImChannelId,
    ) -> VibexResult<Vec<ImPrincipalRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT principal_id, channel_id, external_user_id, display_name, role, status,
                        paired_at_ms, last_seen_at_ms, revoked_at_ms, created_at_ms, updated_at_ms
                 FROM im_principals WHERE channel_id = ?1 ORDER BY created_at_ms ASC",
            )
            .map_err(storage_error("im_principal_list_failed"))?;
        let rows = statement
            .query_map(params![channel_id.as_str()], map_principal)
            .map_err(storage_error("im_principal_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_principal_list_failed"))
    }

    pub fn revoke(conn: &Connection, principal_id: &ImPrincipalId, now_ms: i64) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_principals
             SET status = 'revoked', revoked_at_ms = ?2, updated_at_ms = ?2
             WHERE principal_id = ?1",
            params![principal_id.as_str(), now_ms],
        )
        .map_err(storage_error("im_principal_revoke_failed"))?;
        Ok(())
    }

    pub fn touch(conn: &Connection, principal_id: &ImPrincipalId, now_ms: i64) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_principals SET last_seen_at_ms = ?2, updated_at_ms = ?2
             WHERE principal_id = ?1",
            params![principal_id.as_str(), now_ms],
        )
        .map_err(storage_error("im_principal_touch_failed"))?;
        Ok(())
    }
}

pub struct ImPairingRepository;

impl ImPairingRepository {
    pub fn insert(conn: &Connection, record: &ImPairingRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_pairing_codes (
                pairing_id, channel_id, code_hash, role, scope, target_chat_id,
                created_by_principal_id, expires_at_ms, claimed_principal_id, created_at_ms,
                claimed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.pairing_id.as_str(),
                record.channel_id.as_str(),
                record.code_hash,
                record.role.as_str(),
                record.scope,
                record.target_chat_id,
                record
                    .created_by_principal_id
                    .as_ref()
                    .map(ImPrincipalId::as_str),
                record.expires_at_ms,
                record
                    .claimed_principal_id
                    .as_ref()
                    .map(ImPrincipalId::as_str),
                record.created_at_ms,
                record.claimed_at_ms,
            ],
        )
        .map_err(storage_error("im_pairing_insert_failed"))?;
        Ok(())
    }

    /// Atomically claims one pairing code. Returns the row when this call won.
    pub fn set_claimed_principal(
        conn: &Connection,
        pairing_id: &ImPairingId,
        principal_id: &ImPrincipalId,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_pairing_codes SET claimed_principal_id = ?2 WHERE pairing_id = ?1",
            params![pairing_id.as_str(), principal_id.as_str()],
        )
        .map_err(storage_error("im_pairing_claim_principal_failed"))?;
        Ok(())
    }

    pub fn claim(
        conn: &Connection,
        code_hash: &str,
        now_ms: i64,
    ) -> VibexResult<Option<ImPairingRecord>> {
        let changed = conn
            .execute(
                "UPDATE im_pairing_codes
                 SET claimed_at_ms = ?2
                 WHERE code_hash = ?1 AND claimed_at_ms IS NULL AND expires_at_ms > ?2",
                params![code_hash, now_ms],
            )
            .map_err(storage_error("im_pairing_claim_failed"))?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            "SELECT pairing_id, channel_id, code_hash, role, scope, target_chat_id,
                    created_by_principal_id, expires_at_ms, claimed_principal_id, created_at_ms,
                    claimed_at_ms
             FROM im_pairing_codes WHERE code_hash = ?1",
            params![code_hash],
            map_pairing,
        )
        .optional()
        .map_err(storage_error("im_pairing_claim_lookup_failed"))
    }
}

pub struct ImBindingRepository;

impl ImBindingRepository {
    pub fn get(
        conn: &Connection,
        binding_id: &ImBindingId,
    ) -> VibexResult<Option<ImBindingRecord>> {
        conn.query_row(
            "SELECT binding_id, channel_id, external_chat_id, thread_key, thread_kind, session_id,
                    created_by_principal_id, status, last_delivered_sequence, title,
                    title_sync_enabled, provider_payload_json, created_at_ms, updated_at_ms
             FROM im_bindings WHERE binding_id = ?1",
            params![binding_id.as_str()],
            map_binding,
        )
        .optional()
        .map_err(storage_error("im_binding_lookup_failed"))
    }

    pub fn get_by_target(
        conn: &Connection,
        channel_id: &ImChannelId,
        external_chat_id: &str,
        thread_key: &str,
    ) -> VibexResult<Option<ImBindingRecord>> {
        conn.query_row(
            "SELECT binding_id, channel_id, external_chat_id, thread_key, thread_kind, session_id,
                    created_by_principal_id, status, last_delivered_sequence, title,
                    title_sync_enabled, provider_payload_json, created_at_ms, updated_at_ms
             FROM im_bindings
             WHERE channel_id = ?1 AND external_chat_id = ?2 AND thread_key = ?3",
            params![channel_id.as_str(), external_chat_id, thread_key],
            map_binding,
        )
        .optional()
        .map_err(storage_error("im_binding_lookup_failed"))
    }

    pub fn list_active_for_session(
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Vec<ImBindingRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT binding_id, channel_id, external_chat_id, thread_key, thread_kind, session_id,
                        created_by_principal_id, status, last_delivered_sequence, title,
                        title_sync_enabled, provider_payload_json, created_at_ms, updated_at_ms
                 FROM im_bindings WHERE session_id = ?1 AND status = 'active'
                 ORDER BY created_at_ms ASC",
            )
            .map_err(storage_error("im_binding_list_failed"))?;
        let rows = statement
            .query_map(params![session_id.as_str()], map_binding)
            .map_err(storage_error("im_binding_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_binding_list_failed"))
    }

    pub fn list_for_channel(
        conn: &Connection,
        channel_id: &ImChannelId,
    ) -> VibexResult<Vec<ImBindingRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT binding_id, channel_id, external_chat_id, thread_key, thread_kind, session_id,
                        created_by_principal_id, status, last_delivered_sequence, title,
                        title_sync_enabled, provider_payload_json, created_at_ms, updated_at_ms
                 FROM im_bindings WHERE channel_id = ?1 ORDER BY updated_at_ms DESC",
            )
            .map_err(storage_error("im_binding_list_failed"))?;
        let rows = statement
            .query_map(params![channel_id.as_str()], map_binding)
            .map_err(storage_error("im_binding_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_binding_list_failed"))
    }

    pub fn upsert(conn: &Connection, record: &ImBindingRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_bindings (
                binding_id, channel_id, external_chat_id, thread_key, thread_kind, session_id,
                created_by_principal_id, status, last_delivered_sequence, title,
                title_sync_enabled, provider_payload_json, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(channel_id, external_chat_id, thread_key) DO UPDATE SET
                session_id = excluded.session_id,
                thread_kind = excluded.thread_kind,
                created_by_principal_id = excluded.created_by_principal_id,
                status = 'active',
                last_delivered_sequence = 0,
                title = excluded.title,
                title_sync_enabled = excluded.title_sync_enabled,
                provider_payload_json = excluded.provider_payload_json,
                updated_at_ms = excluded.updated_at_ms",
            params![
                record.binding_id.as_str(),
                record.channel_id.as_str(),
                record.external_chat_id,
                record.thread_key,
                record.thread_kind,
                record.session_id.as_str(),
                record
                    .created_by_principal_id
                    .as_ref()
                    .map(ImPrincipalId::as_str),
                record.status.as_str(),
                record.last_delivered_sequence,
                record.title,
                record.title_sync_enabled,
                record.provider_payload_json,
                record.created_at_ms,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_error("im_binding_upsert_failed"))?;
        Ok(())
    }

    pub fn set_status(
        conn: &Connection,
        binding_id: &ImBindingId,
        status: ImBindingStatus,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_bindings SET status = ?2, updated_at_ms = ?3 WHERE binding_id = ?1",
            params![binding_id.as_str(), status.as_str(), now_ms],
        )
        .map_err(storage_error("im_binding_status_failed"))?;
        Ok(())
    }

    pub fn set_delivered_sequence(
        conn: &Connection,
        binding_id: &ImBindingId,
        sequence: i64,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_bindings
             SET last_delivered_sequence = MAX(last_delivered_sequence, ?2), updated_at_ms = ?3
             WHERE binding_id = ?1",
            params![binding_id.as_str(), sequence, now_ms],
        )
        .map_err(storage_error("im_binding_sequence_failed"))?;
        Ok(())
    }

    pub fn update_title(
        conn: &Connection,
        binding_id: &ImBindingId,
        title: &str,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_bindings SET title = ?2, updated_at_ms = ?3 WHERE binding_id = ?1",
            params![binding_id.as_str(), title, now_ms],
        )
        .map_err(storage_error("im_binding_title_failed"))?;
        Ok(())
    }
}

pub struct ImApprovalCardRepository;

impl ImApprovalCardRepository {
    pub fn upsert(conn: &Connection, record: &ImApprovalCardRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_approval_cards (
                card_id, binding_id, session_id, request_id, external_message_id, status,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(binding_id, request_id) DO UPDATE SET
                external_message_id = COALESCE(excluded.external_message_id, im_approval_cards.external_message_id),
                status = excluded.status,
                updated_at_ms = excluded.updated_at_ms",
            params![
                record.card_id,
                record.binding_id.as_str(),
                record.session_id.as_str(),
                record.request_id,
                record.external_message_id,
                record.status,
                record.created_at_ms,
                record.updated_at_ms,
            ],
        )
        .map_err(storage_error("im_approval_upsert_failed"))?;
        Ok(())
    }

    pub fn get_by_request(
        conn: &Connection,
        binding_id: &ImBindingId,
        request_id: &str,
    ) -> VibexResult<Option<ImApprovalCardRecord>> {
        conn.query_row(
            "SELECT card_id, binding_id, session_id, request_id, external_message_id, status,
                    created_at_ms, updated_at_ms
             FROM im_approval_cards WHERE binding_id = ?1 AND request_id = ?2",
            params![binding_id.as_str(), request_id],
            map_approval_card,
        )
        .optional()
        .map_err(storage_error("im_approval_lookup_failed"))
    }

    pub fn list_for_request(
        conn: &Connection,
        session_id: &VibexSessionId,
        request_id: &str,
    ) -> VibexResult<Vec<ImApprovalCardRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT card_id, binding_id, session_id, request_id, external_message_id, status,
                        created_at_ms, updated_at_ms
                 FROM im_approval_cards WHERE session_id = ?1 AND request_id = ?2",
            )
            .map_err(storage_error("im_approval_list_failed"))?;
        let rows = statement
            .query_map(params![session_id.as_str(), request_id], map_approval_card)
            .map_err(storage_error("im_approval_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_approval_list_failed"))
    }

    pub fn list_pending_for_binding(
        conn: &Connection,
        binding_id: &ImBindingId,
    ) -> VibexResult<Vec<ImApprovalCardRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT card_id, binding_id, session_id, request_id, external_message_id, status,
                        created_at_ms, updated_at_ms
                 FROM im_approval_cards
                 WHERE binding_id = ?1 AND status = 'pending'
                 ORDER BY created_at_ms DESC",
            )
            .map_err(storage_error("im_approval_list_failed"))?;
        let rows = statement
            .query_map(params![binding_id.as_str()], map_approval_card)
            .map_err(storage_error("im_approval_list_failed"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_error("im_approval_list_failed"))
    }

    pub fn set_message_id(
        conn: &Connection,
        card_id: &str,
        external_message_id: &str,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_approval_cards
             SET external_message_id = ?2, updated_at_ms = ?3
             WHERE card_id = ?1",
            params![card_id, external_message_id, now_ms],
        )
        .map_err(storage_error("im_approval_message_failed"))?;
        Ok(())
    }

    pub fn set_status(
        conn: &Connection,
        card_id: &str,
        status: &str,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_approval_cards SET status = ?2, updated_at_ms = ?3 WHERE card_id = ?1",
            params![card_id, status, now_ms],
        )
        .map_err(storage_error("im_approval_status_failed"))?;
        Ok(())
    }
}

pub struct ImActionRepository;

impl ImActionRepository {
    pub fn insert(conn: &Connection, record: &ImActionRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_actions (
                action_id, short_code, card_id, channel_id, binding_id, kind, payload_json,
                expires_at_ms, consumed_at_ms, consumed_by_principal_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.action_id.as_str(),
                record.short_code,
                record.card_id,
                record.channel_id.as_str(),
                record.binding_id.as_str(),
                record.kind,
                record.payload_json,
                record.expires_at_ms,
                record.consumed_at_ms,
                record
                    .consumed_by_principal_id
                    .as_ref()
                    .map(ImPrincipalId::as_str),
                record.created_at_ms,
            ],
        )
        .map_err(storage_error("im_action_insert_failed"))?;
        Ok(())
    }

    pub fn get(conn: &Connection, action_id: &ImActionId) -> VibexResult<Option<ImActionRecord>> {
        conn.query_row(
            "SELECT action_id, short_code, card_id, channel_id, binding_id, kind, payload_json,
                    expires_at_ms, consumed_at_ms, consumed_by_principal_id, created_at_ms
             FROM im_actions WHERE action_id = ?1",
            params![action_id.as_str()],
            map_action,
        )
        .optional()
        .map_err(storage_error("im_action_lookup_failed"))
    }

    /// Finds the first live action for a binding's short code. Codes are unique
    /// among pending actions by the partial index on the table.
    pub fn get_pending_by_short_code(
        conn: &Connection,
        binding_id: &ImBindingId,
        short_code: &str,
        now_ms: i64,
    ) -> VibexResult<Option<ImActionRecord>> {
        conn.query_row(
            "SELECT action_id, short_code, card_id, channel_id, binding_id, kind, payload_json,
                    expires_at_ms, consumed_at_ms, consumed_by_principal_id, created_at_ms
             FROM im_actions
             WHERE binding_id = ?1 AND short_code = ?2 AND consumed_at_ms IS NULL
               AND expires_at_ms > ?3
             ORDER BY created_at_ms DESC LIMIT 1",
            params![binding_id.as_str(), short_code, now_ms],
            map_action,
        )
        .optional()
        .map_err(storage_error("im_action_lookup_failed"))
    }

    pub fn consume(
        conn: &Connection,
        action_id: &ImActionId,
        principal_id: &ImPrincipalId,
        now_ms: i64,
    ) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "UPDATE im_actions
                 SET consumed_at_ms = ?3, consumed_by_principal_id = ?2
                 WHERE action_id = ?1 AND consumed_at_ms IS NULL AND expires_at_ms > ?3",
                params![action_id.as_str(), principal_id.as_str(), now_ms],
            )
            .map_err(storage_error("im_action_consume_failed"))?;
        Ok(changed == 1)
    }

    pub fn invalidate_for_card(conn: &Connection, card_id: &str, now_ms: i64) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_actions SET consumed_at_ms = ?2
             WHERE card_id = ?1 AND consumed_at_ms IS NULL",
            params![card_id, now_ms],
        )
        .map_err(storage_error("im_action_invalidate_failed"))?;
        Ok(())
    }
}

pub struct ImMessageRepository;

impl ImMessageRepository {
    /// Inserts one message row. Returns false when the provider update was
    /// already recorded (the unique partial index rejects it).
    pub fn insert(conn: &Connection, record: &ImMessageRecord) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO im_messages (
                    message_id, channel_id, direction, provider_update_id, external_message_id,
                    binding_id, principal_id, kind, content_preview, content_sha256, status,
                    error_code, error_detail, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    record.message_id,
                    record.channel_id.as_str(),
                    record.direction,
                    record.provider_update_id,
                    record.external_message_id,
                    record.binding_id.as_ref().map(ImBindingId::as_str),
                    record.principal_id.as_ref().map(ImPrincipalId::as_str),
                    record.kind,
                    record.content_preview,
                    record.content_sha256,
                    record.status,
                    record.error_code,
                    record.error_detail,
                    record.created_at_ms,
                ],
            )
            .map_err(storage_error("im_message_insert_failed"))?;
        Ok(changed == 1)
    }

    pub fn set_status(
        conn: &Connection,
        message_id: &str,
        status: &str,
        error_code: Option<&str>,
        error_detail: Option<&str>,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE im_messages SET status = ?2, error_code = ?3, error_detail = ?4
             WHERE message_id = ?1",
            params![message_id, status, error_code, error_detail],
        )
        .map_err(storage_error("im_message_status_failed"))?;
        Ok(())
    }
}

pub struct ImAuditRepository;

impl ImAuditRepository {
    pub fn insert(conn: &Connection, record: &ImAuditRecord) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO im_audit_logs (
                audit_id, channel_id, principal_id, action, target_kind, target_id, outcome,
                detail_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.audit_id,
                record.channel_id.as_str(),
                record.principal_id.as_ref().map(ImPrincipalId::as_str),
                record.action,
                record.target_kind,
                record.target_id,
                record.outcome,
                record.detail_json,
                record.created_at_ms,
            ],
        )
        .map_err(storage_error("im_audit_insert_failed"))?;
        Ok(())
    }
}

fn map_channel(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImChannelRecord> {
    let kind: String = row.get(2)?;
    Ok(ImChannelRecord {
        channel_id: parse_id_sql(row.get(0)?, ImChannelId::parse)?,
        display_name: row.get(1)?,
        kind: ImChannelKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown IM channel kind: {kind}"),
                )),
            )
        })?,
        enabled: row.get(3)?,
        config_json: row.get(4)?,
        event_filter_json: row.get(5)?,
        locale: row.get(6)?,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
    })
}

fn map_secret(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImChannelSecretRecord> {
    Ok(ImChannelSecretRecord {
        channel_id: parse_id_sql(row.get(0)?, ImChannelId::parse)?,
        secret_kind: row.get(1)?,
        backend: row.get(2)?,
        lookup_key: row.get(3)?,
        setup_state: row.get(4)?,
        redacted_hint: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn map_principal(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImPrincipalRecord> {
    let role: String = row.get(4)?;
    let status: String = row.get(5)?;
    Ok(ImPrincipalRecord {
        principal_id: parse_id_sql(row.get(0)?, ImPrincipalId::parse)?,
        channel_id: parse_id_sql(row.get(1)?, ImChannelId::parse)?,
        external_user_id: row.get(2)?,
        display_name: row.get(3)?,
        role: ImPrincipalRole::parse(&role).ok_or_else(|| invalid_text(4, "role", &role))?,
        status: ImPrincipalStatus::parse(&status)
            .ok_or_else(|| invalid_text(5, "status", &status))?,
        paired_at_ms: row.get(6)?,
        last_seen_at_ms: row.get(7)?,
        revoked_at_ms: row.get(8)?,
        created_at_ms: row.get(9)?,
        updated_at_ms: row.get(10)?,
    })
}

fn map_pairing(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImPairingRecord> {
    let role: String = row.get(3)?;
    Ok(ImPairingRecord {
        pairing_id: parse_id_sql(row.get(0)?, ImPairingId::parse)?,
        channel_id: parse_id_sql(row.get(1)?, ImChannelId::parse)?,
        code_hash: row.get(2)?,
        role: ImPrincipalRole::parse(&role).ok_or_else(|| invalid_text(3, "role", &role))?,
        scope: row.get(4)?,
        target_chat_id: row.get(5)?,
        created_by_principal_id: parse_optional_id(row, 6, ImPrincipalId::parse)?,
        expires_at_ms: row.get(7)?,
        claimed_principal_id: parse_optional_id(row, 8, ImPrincipalId::parse)?,
        created_at_ms: row.get(9)?,
        claimed_at_ms: row.get(10)?,
    })
}

fn map_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImBindingRecord> {
    let status: String = row.get(7)?;
    Ok(ImBindingRecord {
        binding_id: parse_id_sql(row.get(0)?, ImBindingId::parse)?,
        channel_id: parse_id_sql(row.get(1)?, ImChannelId::parse)?,
        external_chat_id: row.get(2)?,
        thread_key: row.get(3)?,
        thread_kind: row.get(4)?,
        session_id: parse_id_sql(row.get(5)?, VibexSessionId::parse)?,
        created_by_principal_id: parse_optional_id(row, 6, ImPrincipalId::parse)?,
        status: ImBindingStatus::parse(&status)
            .ok_or_else(|| invalid_text(7, "status", &status))?,
        last_delivered_sequence: row.get(8)?,
        title: row.get(9)?,
        title_sync_enabled: row.get(10)?,
        provider_payload_json: row.get(11)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
    })
}

fn map_approval_card(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImApprovalCardRecord> {
    Ok(ImApprovalCardRecord {
        card_id: row.get(0)?,
        binding_id: parse_id_sql(row.get(1)?, ImBindingId::parse)?,
        session_id: parse_id_sql(row.get(2)?, VibexSessionId::parse)?,
        request_id: row.get(3)?,
        external_message_id: row.get(4)?,
        status: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn map_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImActionRecord> {
    Ok(ImActionRecord {
        action_id: parse_id_sql(row.get(0)?, ImActionId::parse)?,
        short_code: row.get(1)?,
        card_id: row.get(2)?,
        channel_id: parse_id_sql(row.get(3)?, ImChannelId::parse)?,
        binding_id: parse_id_sql(row.get(4)?, ImBindingId::parse)?,
        kind: row.get(5)?,
        payload_json: row.get(6)?,
        expires_at_ms: row.get(7)?,
        consumed_at_ms: row.get(8)?,
        consumed_by_principal_id: parse_optional_id(row, 9, ImPrincipalId::parse)?,
        created_at_ms: row.get(10)?,
    })
}

fn parse_optional_id<T>(
    row: &rusqlite::Row<'_>,
    index: usize,
    parse: impl Fn(String) -> VibexResult<T>,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(index)?
        .map(parse)
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn invalid_text(index: usize, field: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown IM {field}: {value}"),
        )),
    )
}

fn storage_error(code: &'static str) -> impl Fn(rusqlite::Error) -> VibexError {
    move |error| {
        VibexError::storage(code, "IM channel storage operation failed")
            .with_diagnostic("errorKind", sqlite_error_kind(&error))
    }
}

fn sqlite_error_kind(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(inner, _) => format!("{:?}", inner.code),
        _ => "sqlite_error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use vibex_core::{ImChannelConfig, ImChannelConfigEnvelope, ImTelegramConfig};

    fn migrated() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        super::super::apply_migrations(&mut conn).expect("apply migrations");
        conn
    }

    #[test]
    fn im_schema_migrates_and_cascades_channel_rows() {
        let conn = migrated();
        let channel_id = ImChannelId::new();
        let record = ImChannelRecord {
            channel_id: channel_id.clone(),
            display_name: "Telegram test".to_string(),
            kind: ImChannelKind::Telegram,
            enabled: false,
            config_json: serde_json::to_string(&ImChannelConfigEnvelope::new(
                ImChannelConfig::Telegram(ImTelegramConfig {
                    chat_id: "-100123".to_string(),
                    topic_mode: false,
                    require_mention_in_groups: true,
                }),
            ))
            .expect("encode config"),
            event_filter_json: None,
            locale: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        ImChannelRepository::insert(&conn, &record).expect("insert channel");
        assert_eq!(
            ImChannelRepository::get(&conn, &channel_id)
                .expect("read channel")
                .map(|record| record.display_name),
            Some("Telegram test".to_string())
        );

        ImChannelSecretRepository::upsert(
            &conn,
            &ImChannelSecretRecord {
                channel_id: channel_id.clone(),
                secret_kind: "bot_token".to_string(),
                backend: "host_file".to_string(),
                lookup_key: "im:test:bot_token".to_string(),
                setup_state: "configured".to_string(),
                redacted_hint: "configured".to_string(),
                updated_at_ms: 2,
            },
        )
        .expect("insert secret");
        assert!(
            ImChannelSecretRepository::get(&conn, &channel_id, "bot_token")
                .expect("read secret")
                .is_some()
        );

        ImChannelRepository::delete(&conn, &channel_id).expect("delete channel");
        assert!(
            ImChannelSecretRepository::get(&conn, &channel_id, "bot_token")
                .expect("read deleted secret")
                .is_none()
        );
    }

    #[test]
    fn pairing_code_is_claimed_once() {
        let conn = migrated();
        let channel_id = ImChannelId::new();
        ImChannelRepository::insert(
            &conn,
            &ImChannelRecord {
                channel_id: channel_id.clone(),
                display_name: "Pair".to_string(),
                kind: ImChannelKind::Telegram,
                enabled: false,
                config_json: "{}".to_string(),
                event_filter_json: None,
                locale: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .expect("insert channel");

        let pairing_id = ImPairingId::new();
        let pairing = ImPairingRecord {
            pairing_id: pairing_id.clone(),
            channel_id,
            code_hash: "hash".to_string(),
            role: ImPrincipalRole::Approver,
            scope: "dm".to_string(),
            target_chat_id: None,
            created_by_principal_id: None,
            expires_at_ms: 100,
            claimed_principal_id: None,
            created_at_ms: 1,
            claimed_at_ms: None,
        };
        ImPairingRepository::insert(&conn, &pairing).expect("insert pairing");
        let first = ImPairingRepository::claim(&conn, "hash", 50)
            .expect("first claim")
            .expect("claimable");
        assert_eq!(first.pairing_id, pairing_id);
        assert!(
            ImPairingRepository::claim(&conn, "hash", 50)
                .expect("second claim")
                .is_none()
        );
        assert!(
            ImPairingRepository::claim(&conn, "missing", 50)
                .expect("missing claim")
                .is_none()
        );
    }
}

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use vibex_core::{
    DeviceId, RemotePairingCandidate, RemotePairingOfferSummary, RemoteProtocolVersion,
    RemoteProtocolVersionRange, RequestId, VibexError, VibexResult,
};

use super::{enum_from_db_sql, enum_to_db, json_from_db_sql, json_to_db, parse_id_sql};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingOfferRecord {
    pub summary: RemotePairingOfferSummary,
    pub challenge_hash: String,
    pub created_at_ms: i64,
    pub claimed_at_ms: Option<i64>,
    pub canceled_at_ms: Option<i64>,
    pub claim_nonce_hash: Option<String>,
    pub device_ephemeral_public_key: Option<String>,
}

pub struct RemotePairingOfferRepository;

impl RemotePairingOfferRepository {
    pub fn insert(conn: &Connection, record: &RemotePairingOfferRecord) -> VibexResult<()> {
        let summary = &record.summary;
        conn.execute(
            "
            INSERT INTO remote_pairing_codes (
                pairing_id, code_hash, permission_level, expires_at_ms,
                claimed_device_id, created_at_ms, claimed_at_ms,
                offer_format_version, protocol_min_major, protocol_min_minor,
                protocol_max_major, protocol_max_minor, server_id,
                server_identity_public_key, direct_candidates_json,
                relay_candidate_json, granted_permissions_json, canceled_at_ms,
                claim_nonce_hash, device_ephemeral_public_key
            )
            VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
            )
            ",
            params![
                summary.offer_id.as_str(),
                record.challenge_hash,
                enum_to_db(&summary.permission_level)?,
                summary.expires_at_ms,
                summary.claimed_device_id.as_ref().map(DeviceId::as_str),
                record.created_at_ms,
                record.claimed_at_ms,
                summary.format_version,
                summary.protocol_range.min.major,
                summary.protocol_range.min.minor,
                summary.protocol_range.max.major,
                summary.protocol_range.max.minor,
                summary.server_id,
                summary.server_identity_public_key,
                json_to_db(&summary.direct_candidates)?,
                summary
                    .relay_candidate
                    .as_ref()
                    .map(json_to_db)
                    .transpose()?,
                json_to_db(&summary.granted_permissions)?,
                record.canceled_at_ms,
                record.claim_nonce_hash,
                record.device_ephemeral_public_key,
            ],
        )
        .map_err(|error| {
            VibexError::storage(
                "remote_pairing_offer_insert_failed",
                "failed to insert remote pairing offer",
            )
            .with_diagnostic("errorKind", sqlite_error_kind(&error))
        })?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        offer_id: &RequestId,
    ) -> VibexResult<Option<RemotePairingOfferRecord>> {
        conn.query_row(
            "
            SELECT pairing_id, code_hash, permission_level, expires_at_ms,
                claimed_device_id, created_at_ms, claimed_at_ms,
                offer_format_version, protocol_min_major, protocol_min_minor,
                protocol_max_major, protocol_max_minor, server_id,
                server_identity_public_key, direct_candidates_json,
                relay_candidate_json, granted_permissions_json, canceled_at_ms,
                claim_nonce_hash, device_ephemeral_public_key
            FROM remote_pairing_codes
            WHERE pairing_id = ?1 AND offer_format_version IS NOT NULL
            ",
            params![offer_id.as_str()],
            map_offer,
        )
        .optional()
        .map_err(|error| {
            VibexError::storage(
                "remote_pairing_offer_lookup_failed",
                "failed to lookup remote pairing offer",
            )
            .with_diagnostic("errorKind", sqlite_error_kind(&error))
        })
    }

    pub fn cancel(conn: &Connection, offer_id: &RequestId, now_ms: i64) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "
                UPDATE remote_pairing_codes
                SET canceled_at_ms = ?2
                WHERE pairing_id = ?1
                    AND offer_format_version IS NOT NULL
                    AND claimed_at_ms IS NULL
                    AND canceled_at_ms IS NULL
                    AND expires_at_ms > ?2
                ",
                params![offer_id.as_str(), now_ms],
            )
            .map_err(|error| {
                VibexError::storage(
                    "remote_pairing_offer_cancel_failed",
                    "failed to cancel remote pairing offer",
                )
                .with_diagnostic("errorKind", sqlite_error_kind(&error))
            })?;
        Ok(changed == 1)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        conn: &Connection,
        offer_id: &RequestId,
        challenge_hash: &str,
        device_id: &DeviceId,
        claim_nonce_hash: &str,
        now_ms: i64,
    ) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "
                UPDATE remote_pairing_codes
                SET claimed_device_id = ?3, claimed_at_ms = ?5,
                    claim_nonce_hash = ?4
                WHERE pairing_id = ?1
                    AND code_hash = ?2
                    AND offer_format_version IS NOT NULL
                    AND claimed_at_ms IS NULL
                    AND canceled_at_ms IS NULL
                    AND expires_at_ms > ?5
                ",
                params![
                    offer_id.as_str(),
                    challenge_hash,
                    device_id.as_str(),
                    claim_nonce_hash,
                    now_ms,
                ],
            )
            .map_err(|error| {
                VibexError::storage(
                    "remote_pairing_offer_claim_failed",
                    "failed to claim remote pairing offer",
                )
                .with_diagnostic("errorKind", sqlite_error_kind(&error))
            })?;
        Ok(changed == 1)
    }
}

fn map_offer(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemotePairingOfferRecord> {
    let relay_candidate_json: Option<String> = row.get(15)?;
    let canceled_at_ms: Option<i64> = row.get(17)?;
    Ok(RemotePairingOfferRecord {
        summary: RemotePairingOfferSummary {
            offer_id: parse_id_sql(row.get(0)?, RequestId::parse)?,
            permission_level: enum_from_db_sql(row.get(2)?)?,
            expires_at_ms: row.get(3)?,
            claimed_device_id: row
                .get::<_, Option<String>>(4)?
                .map(DeviceId::parse)
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            format_version: row.get(7)?,
            protocol_range: RemoteProtocolVersionRange {
                min: RemoteProtocolVersion {
                    major: row.get(8)?,
                    minor: row.get(9)?,
                },
                max: RemoteProtocolVersion {
                    major: row.get(10)?,
                    minor: row.get(11)?,
                },
            },
            server_id: row.get(12)?,
            server_identity_public_key: row.get(13)?,
            direct_candidates: json_from_db_sql::<Vec<RemotePairingCandidate>>(row.get(14)?)?,
            relay_candidate: relay_candidate_json
                .map(json_from_db_sql::<RemotePairingCandidate>)
                .transpose()?,
            granted_permissions: json_from_db_sql(row.get(16)?)?,
            canceled: canceled_at_ms.is_some(),
        },
        challenge_hash: row.get(1)?,
        created_at_ms: row.get(5)?,
        claimed_at_ms: row.get(6)?,
        canceled_at_ms,
        claim_nonce_hash: row.get(18)?,
        device_ephemeral_public_key: row.get(19)?,
    })
}

fn sqlite_error_kind(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(inner, _) => format!("{:?}", inner.code),
        _ => "sqlite_error".to_string(),
    }
}

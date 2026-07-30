use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};
use url::Url;
use vibex_core::{
    DeviceId, RemoteAuditAction, RemoteAuditOutcome, RemoteAuditTargetKind,
    RemoteCancelPairingOfferRequest, RemoteClaimPairingOfferRequest,
    RemoteClaimPairingOfferResponse, RemoteCreatePairingOfferRequest,
    RemoteCreatePairingOfferResponse, RemoteDeviceDetail, RemoteDeviceStatus,
    RemotePairingCandidate, RemotePairingOffer, RemotePairingOfferSummary, RemotePairingTransport,
    RemoteProtocolVersionRange, RequestId, VibexError, VibexResult, remote_permissions_for_level,
    unix_timestamp_ms,
};
use vibex_db::{
    DbConnection, RemoteDeviceRecord, RemoteDeviceRepository, RemotePairingOfferRecord,
    RemotePairingOfferRepository,
};

use super::{RemoteIdentity, RemoteTrustService, hash_secret, remote_error};

impl RemoteTrustService {
    pub const DEFAULT_PAIRING_OFFER_TTL_MS: u32 = 90_000;
    pub const MIN_PAIRING_OFFER_TTL_MS: u32 = 60_000;
    pub const MAX_PAIRING_OFFER_TTL_MS: u32 = 120_000;

    pub fn create_pairing_offer(
        conn: &DbConnection,
        identity: &RemoteIdentity,
        request: RemoteCreatePairingOfferRequest,
    ) -> VibexResult<RemoteCreatePairingOfferResponse> {
        let direct_candidates = request
            .direct_candidates
            .into_iter()
            .map(validate_pairing_candidate)
            .collect::<VibexResult<Vec<_>>>()?;
        if direct_candidates.len() > 8 {
            return Err(VibexError::validation(
                "remote_pairing_candidates_too_many",
                "pairing offer has too many Direct candidates",
            ));
        }
        let relay_candidate = request
            .relay_candidate
            .map(validate_pairing_candidate)
            .transpose()?;
        if relay_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.transport != RemotePairingTransport::SelfHostedRelay)
        {
            return Err(VibexError::validation(
                "remote_pairing_relay_candidate_invalid",
                "pairing Relay candidate must use the self-hosted Relay transport",
            ));
        }

        let now = unix_timestamp_ms();
        let ttl_ms = request
            .ttl_ms
            .unwrap_or(Self::DEFAULT_PAIRING_OFFER_TTL_MS)
            .clamp(
                Self::MIN_PAIRING_OFFER_TTL_MS,
                Self::MAX_PAIRING_OFFER_TTL_MS,
            );
        let challenge = secure_secret("pair");
        let summary = RemotePairingOfferSummary {
            format_version: 1,
            protocol_range: RemoteProtocolVersionRange::v2(),
            server_id: identity.server_id().to_string(),
            server_identity_public_key: identity.public_key_base64(),
            offer_id: RequestId::new(),
            expires_at_ms: now + i64::from(ttl_ms),
            direct_candidates,
            relay_candidate,
            permission_level: request.permission_level,
            granted_permissions: remote_permissions_for_level(request.permission_level),
            canceled: false,
            claimed_device_id: None,
        };
        let offer = RemotePairingOffer {
            summary: summary.clone(),
            one_time_challenge: challenge.clone(),
        };

        let transaction = conn.unchecked_transaction().map_err(|_| {
            VibexError::storage(
                "remote_pairing_offer_transaction_failed",
                "failed to start remote pairing offer transaction",
            )
        })?;
        RemotePairingOfferRepository::insert(
            &transaction,
            &RemotePairingOfferRecord {
                summary: summary.clone(),
                challenge_hash: hash_secret(&challenge),
                created_at_ms: now,
                claimed_at_ms: None,
                canceled_at_ms: None,
                claim_nonce_hash: None,
                device_ephemeral_public_key: None,
            },
        )?;
        Self::insert_audit(
            &transaction,
            None,
            RemoteAuditAction::PairingOfferCreated,
            RemoteAuditTargetKind::PairingOffer,
            Some(summary.offer_id.as_str().to_string()),
            RemoteAuditOutcome::Created,
            "Short-lived pairing offer created",
            None,
            None,
        )?;
        transaction.commit().map_err(|_| {
            VibexError::storage(
                "remote_pairing_offer_commit_failed",
                "failed to commit remote pairing offer",
            )
        })?;

        let encoded_offer = serde_json::to_vec(&offer).map_err(|_| {
            VibexError::validation(
                "remote_pairing_offer_encode_failed",
                "failed to encode remote pairing offer",
            )
        })?;
        Ok(RemoteCreatePairingOfferResponse {
            offer,
            launch_fragment: format!("#/pair/{}", URL_SAFE_NO_PAD.encode(encoded_offer)),
        })
    }

    pub fn cancel_pairing_offer(
        conn: &DbConnection,
        request: RemoteCancelPairingOfferRequest,
    ) -> VibexResult<RemotePairingOfferSummary> {
        let now = unix_timestamp_ms();
        let transaction = conn.unchecked_transaction().map_err(|_| {
            VibexError::storage(
                "remote_pairing_offer_transaction_failed",
                "failed to start remote pairing offer transaction",
            )
        })?;
        if !RemotePairingOfferRepository::cancel(&transaction, &request.offer_id, now)? {
            let record = RemotePairingOfferRepository::get(&transaction, &request.offer_id)?;
            return Err(classify_unavailable_offer(record.as_ref(), now));
        }
        Self::insert_audit(
            &transaction,
            None,
            RemoteAuditAction::PairingOfferCanceled,
            RemoteAuditTargetKind::PairingOffer,
            Some(request.offer_id.as_str().to_string()),
            RemoteAuditOutcome::Revoked,
            "Pairing offer canceled",
            None,
            None,
        )?;
        transaction.commit().map_err(|_| {
            VibexError::storage(
                "remote_pairing_offer_commit_failed",
                "failed to commit pairing offer cancellation",
            )
        })?;
        let mut summary = RemotePairingOfferRepository::get(conn, &request.offer_id)?
            .ok_or_else(|| {
                remote_error("remote_pairing_offer_unknown", "pairing offer is unknown")
            })?
            .summary;
        summary.canceled = true;
        Ok(summary)
    }

    pub fn claim_pairing_offer(
        conn: &DbConnection,
        request: RemoteClaimPairingOfferRequest,
    ) -> VibexResult<RemoteClaimPairingOfferResponse> {
        let result = Self::claim_pairing_offer_inner(conn, &request);
        if let Err(error) = &result {
            let _ = Self::insert_audit(
                conn,
                None,
                RemoteAuditAction::PairingOfferRejected,
                RemoteAuditTargetKind::PairingOffer,
                Some(request.offer_id.as_str().to_string()),
                RemoteAuditOutcome::Denied,
                format!("Pairing offer claim rejected: {}", error.code),
                None,
                None,
            );
        }
        result
    }

    fn claim_pairing_offer_inner(
        conn: &DbConnection,
        request: &RemoteClaimPairingOfferRequest,
    ) -> VibexResult<RemoteClaimPairingOfferResponse> {
        let display_name = request.display_name.trim();
        if display_name.is_empty() || display_name.chars().count() > 128 {
            return Err(remote_error(
                "remote_device_display_name_invalid",
                "remote device display name must be non-empty and bounded",
            ));
        }
        validate_public_key(&request.device_identity_public_key)?;
        validate_claim_nonce(&request.claim_nonce)?;
        if request.one_time_challenge.len() < 24 || request.one_time_challenge.len() > 256 {
            return Err(remote_error(
                "remote_pairing_offer_challenge_invalid",
                "pairing offer challenge is invalid",
            ));
        }

        let now = unix_timestamp_ms();
        let transaction = conn.unchecked_transaction().map_err(|_| {
            VibexError::storage(
                "remote_pairing_offer_transaction_failed",
                "failed to start remote pairing claim transaction",
            )
        })?;
        let record = RemotePairingOfferRepository::get(&transaction, &request.offer_id)?
            .ok_or_else(|| {
                remote_error("remote_pairing_offer_unknown", "pairing offer is unknown")
            })?;
        if record.summary.server_id != request.expected_server_id
            || record.summary.server_identity_public_key
                != request.expected_server_identity_public_key
        {
            return Err(remote_error(
                "remote_pairing_server_identity_mismatch",
                "pairing offer belongs to a different desktop identity",
            ));
        }
        if record.claimed_at_ms.is_some() {
            return Err(remote_error(
                "remote_pairing_offer_already_claimed",
                "pairing offer has already been claimed",
            ));
        }
        if record.canceled_at_ms.is_some() {
            return Err(remote_error(
                "remote_pairing_offer_canceled",
                "pairing offer has been canceled",
            ));
        }
        if record.summary.expires_at_ms <= now {
            return Err(remote_error(
                "remote_pairing_offer_expired",
                "pairing offer has expired",
            ));
        }
        let challenge_hash = hash_secret(&request.one_time_challenge);
        if challenge_hash != record.challenge_hash {
            return Err(remote_error(
                "remote_pairing_offer_challenge_invalid",
                "pairing offer challenge is invalid",
            ));
        }

        let device_grant_token = secure_secret("grant");
        let device = RemoteDeviceDetail {
            device_id: DeviceId::new(),
            display_name: display_name.to_string(),
            public_key: Some(request.device_identity_public_key.clone()),
            grant_revision: 1,
            permission_level: record.summary.permission_level,
            status: RemoteDeviceStatus::Active,
            paired_at_ms: Some(now),
            last_seen_at_ms: Some(now),
            revoked_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        RemoteDeviceRepository::upsert(
            &transaction,
            &RemoteDeviceRecord {
                detail: device.clone(),
                auth_secret_hash: hash_secret(&device_grant_token),
            },
        )?;
        if !RemotePairingOfferRepository::claim(
            &transaction,
            &request.offer_id,
            &challenge_hash,
            &device.device_id,
            &hash_secret(&request.claim_nonce),
            now,
        )? {
            return Err(remote_error(
                "remote_pairing_offer_already_claimed",
                "pairing offer was claimed concurrently",
            ));
        }
        Self::insert_audit(
            &transaction,
            Some(device.device_id.clone()),
            RemoteAuditAction::PairingOfferClaimed,
            RemoteAuditTargetKind::Device,
            Some(device.device_id.as_str().to_string()),
            RemoteAuditOutcome::Allowed,
            format!(
                "Device '{}' paired from a short-lived offer",
                device.display_name
            ),
            None,
            None,
        )?;
        transaction.commit().map_err(|_| {
            VibexError::storage(
                "remote_pairing_offer_commit_failed",
                "failed to commit remote pairing claim",
            )
        })?;

        Ok(RemoteClaimPairingOfferResponse {
            device,
            device_grant_token,
            session_id: RequestId::new(),
        })
    }
}

pub(super) fn validate_pairing_candidate(
    mut candidate: RemotePairingCandidate,
) -> VibexResult<RemotePairingCandidate> {
    if candidate.transport == RemotePairingTransport::Unknown {
        return Err(VibexError::validation(
            "remote_pairing_transport_unknown",
            "pairing candidate transport is unknown",
        ));
    }
    match candidate.transport {
        RemotePairingTransport::SelfHostedRelay => {
            if candidate.relay_room_id.is_none()
                || candidate.relay_pc_peer_id.is_none()
                || candidate
                    .relay_pc_public_key
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                return Err(VibexError::validation(
                    "remote_pairing_relay_routing_invalid",
                    "pairing Relay candidate must pin room, PC peer, and PC Relay key",
                ));
            }
        }
        RemotePairingTransport::Direct | RemotePairingTransport::Tailnet => {
            if candidate.relay_room_id.is_some()
                || candidate.relay_pc_peer_id.is_some()
                || candidate.relay_pc_public_key.is_some()
            {
                return Err(VibexError::validation(
                    "remote_pairing_direct_routing_invalid",
                    "Direct pairing candidate must not carry Relay routing metadata",
                ));
            }
        }
        RemotePairingTransport::Unknown => unreachable!(),
    }
    let url = Url::parse(candidate.url.trim()).map_err(|_| {
        VibexError::validation(
            "remote_pairing_candidate_url_invalid",
            "pairing candidate URL is invalid",
        )
    })?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(VibexError::validation(
            "remote_pairing_candidate_url_invalid",
            "pairing candidate URL must not contain credentials, query, or fragment",
        ));
    }
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && url
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
    if !secure && !loopback_http {
        return Err(VibexError::validation(
            "remote_pairing_candidate_tls_required",
            "pairing candidate must use HTTPS outside loopback",
        ));
    }
    candidate.url = url.to_string();
    Ok(candidate)
}

fn validate_public_key(value: &str) -> VibexResult<()> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        remote_error(
            "remote_device_identity_key_invalid",
            "remote device identity public key is invalid",
        )
    })?;
    if decoded.len() != 32 {
        return Err(remote_error(
            "remote_device_identity_key_invalid",
            "remote device identity public key has an invalid length",
        ));
    }
    Ok(())
}

fn validate_claim_nonce(value: &str) -> VibexResult<()> {
    if value.len() < 16 || value.len() > 256 || value.chars().any(char::is_whitespace) {
        return Err(remote_error(
            "remote_pairing_claim_nonce_invalid",
            "pairing claim nonce is invalid",
        ));
    }
    Ok(())
}

fn classify_unavailable_offer(
    record: Option<&RemotePairingOfferRecord>,
    now_ms: i64,
) -> VibexError {
    let Some(record) = record else {
        return remote_error("remote_pairing_offer_unknown", "pairing offer is unknown");
    };
    if record.claimed_at_ms.is_some() {
        remote_error(
            "remote_pairing_offer_already_claimed",
            "pairing offer has already been claimed",
        )
    } else if record.canceled_at_ms.is_some() {
        remote_error(
            "remote_pairing_offer_canceled",
            "pairing offer has been canceled",
        )
    } else if record.summary.expires_at_ms <= now_ms {
        remote_error("remote_pairing_offer_expired", "pairing offer has expired")
    } else {
        remote_error(
            "remote_pairing_offer_unavailable",
            "pairing offer is unavailable",
        )
    }
}

pub(crate) fn secure_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}-{}", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use vibex_core::{
        RemoteAuditListRequest, RemoteDevicePermissionLevel, RemotePairingCandidate,
        RemotePairingTransport,
    };
    use vibex_db::{
        RemoteAuditRepository, RemoteDeviceRepository, apply_migrations, open_database,
    };

    use super::*;

    fn test_identity(directory: &tempfile::TempDir) -> RemoteIdentity {
        super::super::RemoteIdentityStore::new(directory.path().join("identity.json"))
            .load_or_create()
            .unwrap()
    }

    fn create_offer(
        conn: &DbConnection,
        identity: &RemoteIdentity,
    ) -> RemoteCreatePairingOfferResponse {
        RemoteTrustService::create_pairing_offer(
            conn,
            identity,
            RemoteCreatePairingOfferRequest {
                permission_level: RemoteDevicePermissionLevel::FullControl,
                ttl_ms: Some(1),
                direct_candidates: vec![RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: "http://127.0.0.1:1428".to_string(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                }],
                relay_candidate: Some(RemotePairingCandidate {
                    transport: RemotePairingTransport::SelfHostedRelay,
                    url: "https://relay.example.test".to_string(),
                    relay_room_id: Some(vibex_core::RelayRoomId::new()),
                    relay_pc_peer_id: Some(vibex_core::RelayPeerId::new()),
                    relay_pc_public_key: Some("pc-public".to_string()),
                }),
            },
        )
        .unwrap()
    }

    fn claim_request(
        offer: &RemoteCreatePairingOfferResponse,
        key_byte: u8,
        nonce: &str,
    ) -> RemoteClaimPairingOfferRequest {
        RemoteClaimPairingOfferRequest {
            offer_id: offer.offer.summary.offer_id.clone(),
            one_time_challenge: offer.offer.one_time_challenge.clone(),
            expected_server_id: offer.offer.summary.server_id.clone(),
            expected_server_identity_public_key: offer
                .offer
                .summary
                .server_identity_public_key
                .clone(),
            display_name: format!("Phone {key_byte}"),
            device_identity_public_key: URL_SAFE_NO_PAD.encode([key_byte; 32]),
            claim_nonce: nonce.to_string(),
        }
    }

    #[test]
    fn pairing_offer_is_short_lived_fragment_safe_cancelable_and_audited() {
        let directory = tempfile::tempdir().unwrap();
        let identity = test_identity(&directory);
        let mut conn = DbConnection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let created = create_offer(&conn, &identity);
        let ttl = created.offer.summary.expires_at_ms - unix_timestamp_ms();
        assert!(ttl > 55_000 && ttl <= i64::from(RemoteTrustService::MIN_PAIRING_OFFER_TTL_MS));
        assert!(created.launch_fragment.starts_with("#/pair/"));
        for forbidden in ["authToken", "deviceGrantToken", "workspace", "privateKey"] {
            assert!(!created.launch_fragment.contains(forbidden));
        }
        let canceled = RemoteTrustService::cancel_pairing_offer(
            &conn,
            RemoteCancelPairingOfferRequest {
                offer_id: created.offer.summary.offer_id.clone(),
            },
        )
        .unwrap();
        assert!(canceled.canceled);
        let error = RemoteTrustService::claim_pairing_offer(
            &conn,
            claim_request(&created, 3, "claim-nonce-canceled"),
        )
        .unwrap_err();
        assert_eq!(error.code, "remote_pairing_offer_canceled");
        let audits = RemoteAuditRepository::list(
            &conn,
            &RemoteAuditListRequest {
                device_id: None,
                limit: Some(20),
            },
        )
        .unwrap();
        assert!(
            audits
                .iter()
                .any(|record| record.action == RemoteAuditAction::PairingOfferCanceled)
        );
        assert!(audits.iter().all(|record| {
            !record
                .redacted_summary
                .contains(&created.offer.one_time_challenge)
        }));
    }

    #[test]
    fn pairing_offer_rejects_tamper_wrong_identity_expiry_and_replay() {
        let directory = tempfile::tempdir().unwrap();
        let identity = test_identity(&directory);
        let mut conn = DbConnection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let created = create_offer(&conn, &identity);
        let mut wrong_identity = claim_request(&created, 4, "claim-nonce-wrong-identity");
        wrong_identity.expected_server_id = "server-wrong".to_string();
        assert_eq!(
            RemoteTrustService::claim_pairing_offer(&conn, wrong_identity)
                .unwrap_err()
                .code,
            "remote_pairing_server_identity_mismatch"
        );
        let mut tampered = claim_request(&created, 4, "claim-nonce-tampered");
        tampered.one_time_challenge = secure_secret("pair");
        assert_eq!(
            RemoteTrustService::claim_pairing_offer(&conn, tampered)
                .unwrap_err()
                .code,
            "remote_pairing_offer_challenge_invalid"
        );

        let claimed = RemoteTrustService::claim_pairing_offer(
            &conn,
            claim_request(&created, 4, "claim-nonce-success"),
        )
        .unwrap();
        assert_eq!(
            claimed.device.permission_level,
            RemoteDevicePermissionLevel::FullControl
        );
        assert_eq!(
            RemoteTrustService::claim_pairing_offer(
                &conn,
                claim_request(&created, 4, "claim-nonce-replay"),
            )
            .unwrap_err()
            .code,
            "remote_pairing_offer_already_claimed"
        );

        let expired = create_offer(&conn, &identity);
        conn.execute(
            "UPDATE remote_pairing_codes SET expires_at_ms = ?2 WHERE pairing_id = ?1",
            (
                expired.offer.summary.offer_id.as_str(),
                unix_timestamp_ms() - 1,
            ),
        )
        .unwrap();
        assert_eq!(
            RemoteTrustService::claim_pairing_offer(
                &conn,
                claim_request(&expired, 5, "claim-nonce-expired"),
            )
            .unwrap_err()
            .code,
            "remote_pairing_offer_expired"
        );
    }

    #[test]
    fn concurrent_pairing_claim_has_exactly_one_winner_and_one_device() {
        let directory = tempfile::tempdir().unwrap();
        let identity = test_identity(&directory);
        let database_path = directory.path().join("pairing.db");
        let mut conn = open_database(&database_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let offer = create_offer(&conn, &identity);
        drop(conn);

        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for (key, nonce) in [(7, "claim-nonce-race-a"), (8, "claim-nonce-race-b")] {
            let database_path = database_path.clone();
            let barrier = barrier.clone();
            let request = claim_request(&offer, key, nonce);
            threads.push(std::thread::spawn(move || {
                let conn = open_database(&database_path).unwrap();
                barrier.wait();
                RemoteTrustService::claim_pairing_offer(&conn, request)
            }));
        }
        barrier.wait();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

        let conn = open_database(&database_path).unwrap();
        assert_eq!(RemoteDeviceRepository::list(&conn).unwrap().len(), 1);
    }
}

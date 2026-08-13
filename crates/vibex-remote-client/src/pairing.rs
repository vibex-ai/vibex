use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use url::Url;
use vibex_backend::{BackendError, BackendResult};
use vibex_core::{
    RemoteClaimPairingOfferRequest, RemotePairingCandidate, RemotePairingOffer,
    RemotePairingTransport, RemoteProtocolVersionRange,
};

pub const PAIRING_FRAGMENT_PREFIX: &str = "#/pair/";
pub const MAX_PAIRING_FRAGMENT_BYTES: usize = 32 * 1024;
pub const PAIRING_ENTRY_HINT_SCHEMA_VERSION: &str = "vibex-pairing-entry.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingEntryHintKind {
    Origin,
    MobileApp,
    UntrustedCustomScheme,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingEntryHint {
    pub schema_version: String,
    pub kind: PairingEntryHintKind,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub transport: Option<RemotePairingTransport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingClaimRoute {
    Direct {
        claim_base_url: String,
        transport: RemotePairingTransport,
    },
    Relay(RemotePairingCandidate),
}

/// Decode and validate the short-lived pairing payload carried in a URL
/// fragment. The fragment is intentionally consumed before any network call;
/// callers must remove it from browser history immediately after this returns.
pub fn parse_pairing_offer_fragment(value: &str, now_ms: i64) -> BackendResult<RemotePairingOffer> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_PAIRING_FRAGMENT_BYTES {
        return Err(BackendError::failed(
            "remote_pairing_fragment_invalid",
            "pairing fragment is empty or exceeds the bounded size",
        ));
    }
    let marker = value.find(PAIRING_FRAGMENT_PREFIX).ok_or_else(|| {
        BackendError::failed(
            "remote_pairing_fragment_invalid",
            "pairing fragment does not contain the expected route",
        )
    })?;
    let encoded = &value[marker + PAIRING_FRAGMENT_PREFIX.len()..];
    if encoded.is_empty()
        || encoded.len() > MAX_PAIRING_FRAGMENT_BYTES
        || encoded
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    {
        return Err(BackendError::failed(
            "remote_pairing_fragment_invalid",
            "pairing fragment payload is not bounded base64url data",
        ));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        BackendError::failed(
            "remote_pairing_fragment_invalid",
            "pairing fragment payload is not valid base64url data",
        )
    })?;
    if decoded.len() > MAX_PAIRING_FRAGMENT_BYTES {
        return Err(BackendError::failed(
            "remote_pairing_fragment_invalid",
            "decoded pairing payload exceeds the bounded size",
        ));
    }
    let offer: RemotePairingOffer = serde_json::from_slice(&decoded).map_err(|_| {
        BackendError::failed(
            "remote_pairing_fragment_invalid",
            "pairing fragment payload is not a valid offer",
        )
    })?;
    validate_pairing_offer(&offer, now_ms)?;
    Ok(offer)
}

pub fn direct_pairing_candidate(offer: &RemotePairingOffer) -> BackendResult<String> {
    offer
        .summary
        .direct_candidates
        .iter()
        .find(|candidate| {
            matches!(
                candidate.transport,
                RemotePairingTransport::Direct | RemotePairingTransport::Tailnet
            ) && valid_candidate_url(&candidate.url)
        })
        .map(|candidate| candidate.url.clone())
        .ok_or_else(|| {
            BackendError::unsupported(
                "remote_pairing_direct_candidate_missing",
                "pairing offer does not contain a Direct or private-network candidate",
            )
        })
}

pub fn relay_pairing_candidate(
    offer: &RemotePairingOffer,
) -> BackendResult<&vibex_core::RemotePairingCandidate> {
    offer
        .summary
        .relay_candidate
        .as_ref()
        .filter(|candidate| {
            candidate.transport == RemotePairingTransport::SelfHostedRelay
                && valid_candidate_url(&candidate.url)
                && candidate.relay_room_id.is_some()
                && candidate.relay_pc_peer_id.is_some()
                && candidate
                    .relay_pc_public_key
                    .as_deref()
                    .is_some_and(|key| !key.trim().is_empty())
        })
        .ok_or_else(|| {
            BackendError::unsupported(
                "remote_pairing_relay_candidate_missing",
                "pairing offer does not contain a complete self-hosted Relay candidate",
            )
        })
}

/// Convert a validated Direct WebSocket candidate to the HTTP(S) origin used
/// for the one-shot pairing claim endpoint.
pub fn pairing_claim_base_url(offer: &RemotePairingOffer) -> BackendResult<String> {
    let candidate = direct_pairing_candidate(offer)?;
    pairing_claim_base_url_for_candidate(&candidate)
}

fn pairing_claim_base_url_for_candidate(candidate: &str) -> BackendResult<String> {
    let mut url = Url::parse(candidate).map_err(|_| {
        BackendError::failed(
            "remote_pairing_direct_candidate_invalid",
            "pairing offer contains an invalid Direct candidate",
        )
    })?;
    match url.scheme() {
        "ws" => url.set_scheme("http"),
        "wss" => url.set_scheme("https"),
        "http" | "https" => Ok(()),
        _ => Err(()),
    }
    .map_err(|_| {
        BackendError::failed(
            "remote_pairing_direct_candidate_invalid",
            "pairing Direct candidate cannot be used for an HTTP claim",
        )
    })?;
    Ok(url.to_string().trim_end_matches('/').to_string())
}

/// Select exactly one claim route from the server-owned offer candidates.
/// The entry hint can only identify an existing candidate by development-host
/// origin or mobile transport class; it can never inject a URL used for the
/// claim request.
pub fn select_pairing_claim_route(
    offer: &RemotePairingOffer,
    entry_hint: &PairingEntryHint,
) -> BackendResult<PairingClaimRoute> {
    if entry_hint.schema_version != PAIRING_ENTRY_HINT_SCHEMA_VERSION {
        return Err(BackendError::unsupported(
            "remote_pairing_entry_hint_incompatible",
            "pairing entry hint schema is incompatible",
        ));
    }
    let mut matches = Vec::new();
    let entry_origin = match entry_hint.kind {
        PairingEntryHintKind::Origin => {
            if entry_hint.transport.is_some() {
                return Err(invalid_entry_hint(
                    "development-host pairing entry cannot include a transport",
                ));
            }
            let origin = entry_hint.origin.as_deref().ok_or_else(|| {
                invalid_entry_hint("development-host pairing entry omitted its origin")
            })?;
            Some(normalized_entry_origin(origin)?)
        }
        PairingEntryHintKind::MobileApp => {
            if entry_hint.origin.is_some() {
                return Err(invalid_entry_hint(
                    "mobile pairing entry cannot include a network origin",
                ));
            }
            let transport = entry_hint
                .transport
                .ok_or_else(|| invalid_entry_hint("mobile pairing entry omitted its transport"))?;
            if !matches!(
                transport,
                RemotePairingTransport::Direct
                    | RemotePairingTransport::Tailnet
                    | RemotePairingTransport::SelfHostedRelay
            ) {
                return Err(invalid_entry_hint(
                    "mobile pairing entry transport is unsupported",
                ));
            }
            None
        }
        PairingEntryHintKind::UntrustedCustomScheme => {
            return Err(BackendError::permission(
                "remote_pairing_entry_untrusted",
                "unrecognized custom-scheme pairing entry cannot select a network claim route",
            ));
        }
    };

    for candidate in &offer.summary.direct_candidates {
        if !matches!(
            candidate.transport,
            RemotePairingTransport::Direct | RemotePairingTransport::Tailnet
        ) || !valid_candidate_url(&candidate.url)
        {
            continue;
        }
        let selected = match entry_hint.kind {
            PairingEntryHintKind::Origin => {
                normalized_candidate_origin(&candidate.url).as_deref() == entry_origin.as_deref()
            }
            PairingEntryHintKind::MobileApp => entry_hint.transport == Some(candidate.transport),
            PairingEntryHintKind::UntrustedCustomScheme => false,
        };
        if selected {
            matches.push(PairingClaimRoute::Direct {
                claim_base_url: pairing_claim_base_url_for_candidate(&candidate.url)?,
                transport: candidate.transport,
            });
        }
    }
    if let Ok(candidate) = relay_pairing_candidate(offer) {
        let selected = match entry_hint.kind {
            PairingEntryHintKind::Origin => {
                normalized_candidate_origin(&candidate.url).as_deref() == entry_origin.as_deref()
            }
            PairingEntryHintKind::MobileApp => {
                entry_hint.transport == Some(RemotePairingTransport::SelfHostedRelay)
            }
            PairingEntryHintKind::UntrustedCustomScheme => false,
        };
        if selected {
            matches.push(PairingClaimRoute::Relay(candidate.clone()));
        }
    }

    if entry_hint.kind == PairingEntryHintKind::MobileApp {
        return matches.into_iter().next().ok_or_else(|| {
            BackendError::permission(
                "remote_pairing_entry_route_mismatch",
                "mobile pairing entry transport does not match an offered claim route",
            )
        });
    }

    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(BackendError::permission(
            "remote_pairing_entry_route_mismatch",
            "pairing entry origin does not match an offered claim route",
        )),
        _ => Err(BackendError::conflict(
            "remote_pairing_entry_route_ambiguous",
            "pairing entry origin matches more than one offered claim route",
        )),
    }
}

fn invalid_entry_hint(message: &'static str) -> BackendError {
    BackendError::failed("remote_pairing_entry_hint_invalid", message)
}

fn normalized_entry_origin(value: &str) -> BackendResult<String> {
    let url = Url::parse(value).map_err(|_| {
        BackendError::failed(
            "remote_pairing_entry_hint_invalid",
            "pairing entry origin is invalid",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(BackendError::failed(
            "remote_pairing_entry_hint_invalid",
            "pairing entry hint must contain only a Web origin",
        ));
    }
    normalized_candidate_origin(value).ok_or_else(|| {
        BackendError::failed(
            "remote_pairing_entry_hint_invalid",
            "pairing entry origin could not be normalized",
        )
    })
}

fn normalized_candidate_origin(value: &str) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    match url.scheme() {
        "ws" => url.set_scheme("http").ok()?,
        "wss" => url.set_scheme("https").ok()?,
        "http" | "https" => {}
        _ => return None,
    }
    let origin = url.origin().ascii_serialization();
    (origin != "null").then_some(origin)
}

fn valid_candidate_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https" | "ws" | "wss")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

pub fn pairing_claim_request(
    offer: &RemotePairingOffer,
    display_name: &str,
    device_identity_public_key: String,
    claim_nonce: String,
) -> BackendResult<RemoteClaimPairingOfferRequest> {
    let display_name = display_name.trim();
    if display_name.is_empty() || display_name.chars().count() > 128 {
        return Err(BackendError::failed(
            "remote_device_display_name_invalid",
            "remote device display name must be non-empty and bounded",
        ));
    }
    if claim_nonce.len() < 16
        || claim_nonce.len() > 256
        || claim_nonce.chars().any(char::is_whitespace)
    {
        return Err(BackendError::failed(
            "remote_pairing_claim_nonce_invalid",
            "pairing claim nonce is invalid",
        ));
    }
    Ok(RemoteClaimPairingOfferRequest {
        offer_id: offer.summary.offer_id.clone(),
        one_time_challenge: offer.one_time_challenge.clone(),
        expected_server_id: offer.summary.server_id.clone(),
        expected_server_identity_public_key: offer.summary.server_identity_public_key.clone(),
        display_name: display_name.to_string(),
        device_identity_public_key,
        claim_nonce,
    })
}

fn validate_pairing_offer(offer: &RemotePairingOffer, now_ms: i64) -> BackendResult<()> {
    if offer.summary.format_version != 1
        || offer
            .summary
            .protocol_range
            .negotiate(RemoteProtocolVersionRange::v2())
            .is_none()
    {
        return Err(BackendError::unsupported(
            "remote_pairing_protocol_incompatible",
            "pairing offer does not support the current protocol",
        ));
    }
    if offer.summary.canceled {
        return Err(BackendError::conflict(
            "remote_pairing_offer_canceled",
            "pairing offer has been canceled",
        ));
    }
    if offer.summary.claimed_device_id.is_some() {
        return Err(BackendError::conflict(
            "remote_pairing_offer_already_claimed",
            "pairing offer has already been claimed",
        ));
    }
    if offer.summary.expires_at_ms <= now_ms {
        return Err(BackendError::conflict(
            "remote_pairing_offer_expired",
            "pairing offer has expired",
        ));
    }
    if offer.one_time_challenge.len() < 24 || offer.one_time_challenge.len() > 256 {
        return Err(BackendError::failed(
            "remote_pairing_offer_challenge_invalid",
            "pairing offer challenge is invalid",
        ));
    }
    if direct_pairing_candidate(offer).is_err() && relay_pairing_candidate(offer).is_err() {
        return Err(BackendError::unsupported(
            "remote_pairing_candidate_missing",
            "pairing offer has no usable Direct or self-hosted Relay candidate",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        RemoteActionClass, RemoteDevicePermissionLevel, RemotePairingCandidate,
        RemotePairingOfferSummary, RequestId,
    };

    fn offer(expires_at_ms: i64) -> RemotePairingOffer {
        RemotePairingOffer {
            summary: RemotePairingOfferSummary {
                format_version: 1,
                protocol_range: RemoteProtocolVersionRange::v2(),
                server_id: "desktop-test".into(),
                server_identity_public_key: "server-public-key".into(),
                offer_id: RequestId::new(),
                expires_at_ms,
                direct_candidates: vec![RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: "https://desktop.example.test".into(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                }],
                relay_candidate: None,
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                granted_permissions: vec![RemoteActionClass::ReadProject],
                canceled: false,
                claimed_device_id: None,
            },
            one_time_challenge: "pairing-challenge-at-least-twenty-four-bytes".into(),
        }
    }

    fn fragment(offer: &RemotePairingOffer) -> String {
        format!(
            "{PAIRING_FRAGMENT_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(offer).unwrap())
        )
    }

    #[test]
    fn pairing_fragment_round_trips_without_query_or_path_secrets() {
        let offer = offer(10_000);
        let parsed =
            parse_pairing_offer_fragment(&format!("vibex://open{}", fragment(&offer)), 1).unwrap();
        assert_eq!(parsed.summary.offer_id, offer.summary.offer_id);
        assert_eq!(
            direct_pairing_candidate(&parsed).unwrap(),
            "https://desktop.example.test"
        );
        assert_eq!(
            pairing_claim_base_url(&parsed).unwrap(),
            "https://desktop.example.test"
        );
    }

    #[test]
    fn pairing_fragment_rejects_expiry_tamper_and_relay_only_offers() {
        let expired = offer(1);
        assert_eq!(
            parse_pairing_offer_fragment(&fragment(&expired), 1)
                .unwrap_err()
                .code,
            "remote_pairing_offer_expired"
        );
        assert_eq!(
            parse_pairing_offer_fragment("#/pair/not+base64", 0)
                .unwrap_err()
                .code,
            "remote_pairing_fragment_invalid"
        );
        let mut relay_only = offer(10_000);
        relay_only.summary.direct_candidates.clear();
        relay_only.summary.relay_candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: "https://relay.example.test".into(),
            relay_room_id: Some(vibex_core::RelayRoomId::new()),
            relay_pc_peer_id: Some(vibex_core::RelayPeerId::new()),
            relay_pc_public_key: Some("pc-public".into()),
        });
        assert_eq!(
            relay_pairing_candidate(
                &parse_pairing_offer_fragment(&fragment(&relay_only), 0).unwrap()
            )
            .unwrap()
            .url,
            "https://relay.example.test"
        );

        let mut secret_in_url = offer(10_000);
        secret_in_url.summary.direct_candidates[0].url =
            "https://desktop.example.test?grant=secret".into();
        assert_eq!(
            parse_pairing_offer_fragment(&fragment(&secret_in_url), 0)
                .unwrap_err()
                .code,
            "remote_pairing_candidate_missing"
        );
    }

    #[test]
    fn claim_request_binds_display_identity_and_nonce_without_debug_secrets() {
        let offer = offer(10_000);
        let request = pairing_claim_request(
            &offer,
            "Phone",
            "device-public-key".into(),
            "claim-nonce-abcdefghijklmnop".into(),
        )
        .unwrap();
        let debug = format!("{request:?}");
        assert_eq!(request.display_name, "Phone");
        assert!(!debug.contains(&offer.one_time_challenge));
        assert!(!debug.contains("claim-nonce-abcdefghijklmnop"));
    }

    #[test]
    fn entry_hint_selects_exact_direct_tailnet_and_relay_origins() {
        let mut offer = offer(10_000);
        offer
            .summary
            .direct_candidates
            .push(RemotePairingCandidate {
                transport: RemotePairingTransport::Tailnet,
                url: "wss://desktop.tailnet.test:443/v2".into(),
                relay_room_id: None,
                relay_pc_peer_id: None,
                relay_pc_public_key: None,
            });
        offer.summary.relay_candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: "https://relay.example.test".into(),
            relay_room_id: Some(vibex_core::RelayRoomId::new()),
            relay_pc_peer_id: Some(vibex_core::RelayPeerId::new()),
            relay_pc_public_key: Some("pc-public".into()),
        });

        let hint = |origin: &str| PairingEntryHint {
            schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
            kind: PairingEntryHintKind::Origin,
            origin: Some(origin.into()),
            transport: None,
        };
        assert_eq!(
            select_pairing_claim_route(&offer, &hint("https://desktop.example.test")).unwrap(),
            PairingClaimRoute::Direct {
                claim_base_url: "https://desktop.example.test".into(),
                transport: RemotePairingTransport::Direct,
            }
        );
        assert_eq!(
            select_pairing_claim_route(&offer, &hint("https://desktop.tailnet.test")).unwrap(),
            PairingClaimRoute::Direct {
                claim_base_url: "https://desktop.tailnet.test/v2".into(),
                transport: RemotePairingTransport::Tailnet,
            }
        );
        assert!(matches!(
            select_pairing_claim_route(&offer, &hint("https://relay.example.test")).unwrap(),
            PairingClaimRoute::Relay(_)
        ));

        let mobile_hint = |transport| PairingEntryHint {
            schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
            kind: PairingEntryHintKind::MobileApp,
            origin: None,
            transport: Some(transport),
        };
        assert_eq!(
            select_pairing_claim_route(&offer, &mobile_hint(RemotePairingTransport::Tailnet))
                .unwrap(),
            PairingClaimRoute::Direct {
                claim_base_url: "https://desktop.tailnet.test/v2".into(),
                transport: RemotePairingTransport::Tailnet,
            }
        );
        assert!(matches!(
            select_pairing_claim_route(
                &offer,
                &mobile_hint(RemotePairingTransport::SelfHostedRelay)
            )
            .unwrap(),
            PairingClaimRoute::Relay(_)
        ));
    }

    #[test]
    fn mobile_entry_selects_first_matching_candidate_in_offer_order() {
        let mut offer = offer(10_000);
        offer
            .summary
            .direct_candidates
            .push(RemotePairingCandidate {
                transport: RemotePairingTransport::Direct,
                url: "wss://desktop-secondary.example.test/v2".into(),
                relay_room_id: None,
                relay_pc_peer_id: None,
                relay_pc_public_key: None,
            });
        let hint = PairingEntryHint {
            schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
            kind: PairingEntryHintKind::MobileApp,
            origin: None,
            transport: Some(RemotePairingTransport::Direct),
        };

        assert_eq!(
            select_pairing_claim_route(&offer, &hint).unwrap(),
            PairingClaimRoute::Direct {
                claim_base_url: "https://desktop.example.test".into(),
                transport: RemotePairingTransport::Direct,
            }
        );
        assert_eq!(offer.summary.direct_candidates.len(), 2);
    }

    #[test]
    fn entry_hint_rejects_mismatch_untrusted_and_ambiguous_routes() {
        let mut offer = offer(10_000);
        let hint = PairingEntryHint {
            schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
            kind: PairingEntryHintKind::Origin,
            origin: Some("https://other.example.test".into()),
            transport: None,
        };
        assert_eq!(
            select_pairing_claim_route(&offer, &hint).unwrap_err().code,
            "remote_pairing_entry_route_mismatch"
        );

        let untrusted = PairingEntryHint {
            schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
            kind: PairingEntryHintKind::UntrustedCustomScheme,
            origin: None,
            transport: None,
        };
        assert_eq!(
            select_pairing_claim_route(&offer, &untrusted)
                .unwrap_err()
                .code,
            "remote_pairing_entry_untrusted"
        );

        offer.summary.relay_candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: "wss://desktop.example.test/relay".into(),
            relay_room_id: Some(vibex_core::RelayRoomId::new()),
            relay_pc_peer_id: Some(vibex_core::RelayPeerId::new()),
            relay_pc_public_key: Some("pc-public".into()),
        });
        let same_origin = PairingEntryHint {
            schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
            kind: PairingEntryHintKind::Origin,
            origin: Some("https://desktop.example.test".into()),
            transport: None,
        };
        assert_eq!(
            select_pairing_claim_route(&offer, &same_origin)
                .unwrap_err()
                .code,
            "remote_pairing_entry_route_ambiguous"
        );

        for hint in [
            PairingEntryHint {
                schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
                kind: PairingEntryHintKind::MobileApp,
                origin: Some("https://injected.example.test".into()),
                transport: Some(RemotePairingTransport::Direct),
            },
            PairingEntryHint {
                schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
                kind: PairingEntryHintKind::MobileApp,
                origin: None,
                transport: Some(RemotePairingTransport::Unknown),
            },
            PairingEntryHint {
                schema_version: PAIRING_ENTRY_HINT_SCHEMA_VERSION.into(),
                kind: PairingEntryHintKind::Origin,
                origin: Some("https://desktop.example.test".into()),
                transport: Some(RemotePairingTransport::Direct),
            },
        ] {
            assert_eq!(
                select_pairing_claim_route(&offer, &hint).unwrap_err().code,
                "remote_pairing_entry_hint_invalid"
            );
        }
    }
}

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use url::Url;
use vibex_core::{
    ErrorCategory, REMOTE_LAN_PAIRING_SCHEMA_VERSION, RemoteLanPairingAdvertisement,
    RemoteLanPairingDiscoverySummary, RemoteLanPairingPendingRequestSummary,
    RemoteLanPairingRequest, RemoteLanPairingRequestAccepted, RemoteLanPairingRequestState,
    RemoteLanPairingStatusRequest, RemoteLanPairingStatusResponse, RemoteLanPairingWindowSnapshot,
    RemotePairingOffer, RemotePairingTransport, RequestId, VibexError, VibexResult,
    remote_lan_pairing_device_fingerprint, remote_lan_pairing_verification_code,
};

use super::pairing_v2::{secure_secret, validate_public_key};

pub(crate) const LAN_PAIRING_MAX_PENDING_REQUESTS: usize = 8;
pub(crate) const LAN_PAIRING_MIN_POLL_INTERVAL_MS: i64 = 500;

type HmacSha256 = Hmac<Sha256>;

pub(crate) struct LanPairingCoordinator {
    secret_hash_key: [u8; 32],
    window: Mutex<Option<LanPairingWindow>>,
}

impl std::fmt::Debug for LanPairingCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LanPairingCoordinator")
            .field(
                "has_active_window",
                &self.window.lock().is_ok_and(|window| window.is_some()),
            )
            .finish()
    }
}

struct LanPairingWindow {
    discovery: RemoteLanPairingDiscoverySummary,
    advertisement: RemoteLanPairingAdvertisement,
    offer: RemotePairingOffer,
    pending: HashMap<RequestId, PendingRequest>,
}

struct PendingRequest {
    request_id: RequestId,
    device_identity_public_key: String,
    display_name: String,
    client_nonce: String,
    idempotency_key: String,
    request_secret_hash: [u8; 32],
    verification_code: String,
    state: RemoteLanPairingRequestState,
    last_poll_at_ms: Option<i64>,
}

impl Default for LanPairingCoordinator {
    fn default() -> Self {
        let mut secret_hash_key = [0u8; 32];
        OsRng.fill_bytes(&mut secret_hash_key);
        Self {
            secret_hash_key,
            window: Mutex::new(None),
        }
    }
}

impl LanPairingCoordinator {
    pub(crate) fn start_zero_config(
        &self,
        offer: RemotePairingOffer,
        local_port: u16,
        display_name: &str,
        now_ms: i64,
    ) -> VibexResult<RemoteLanPairingWindowSnapshot> {
        if local_port == 0 {
            return Err(VibexError::validation(
                "remote_zero_config_pairing_listener_invalid",
                "zero-config LAN pairing listener port is invalid",
            ));
        }
        if offer.summary.expires_at_ms <= now_ms
            || offer.summary.canceled
            || offer.summary.claimed_device_id.is_some()
        {
            return Err(remote_error(
                "remote_zero_config_pairing_unavailable",
                "zero-config LAN pairing window is already unavailable",
            ));
        }
        let display_name = bounded_display_name(display_name)?;
        let mut window = self.window.lock().map_err(|_| state_error())?;
        if window.as_ref().is_some_and(|current| {
            current.discovery.expires_at_ms > now_ms
                && !current.offer.summary.canceled
                && current.offer.summary.claimed_device_id.is_none()
        }) {
            return Err(VibexError::conflict(
                "remote_zero_config_pairing_active",
                "another zero-config LAN pairing window is already active",
            ));
        }

        let window_id = RequestId::new();
        let advertisement_id = secure_secret("local-adv");
        let service_instance = service_instance_name(&display_name, &advertisement_id);
        let discovery = RemoteLanPairingDiscoverySummary {
            schema_version: REMOTE_LAN_PAIRING_SCHEMA_VERSION.to_string(),
            window_id,
            server_id: offer.summary.server_id.clone(),
            server_identity_public_key: offer.summary.server_identity_public_key.clone(),
            protocol_range: offer.summary.protocol_range,
            permission_level: offer.summary.permission_level,
            expires_at_ms: offer.summary.expires_at_ms,
        };
        let advertisement = RemoteLanPairingAdvertisement {
            advertisement_id,
            service_instance,
            display_name,
            direct_origin: format!("http://127.0.0.1:{local_port}"),
            protocol_min: offer.summary.protocol_range.min.major,
            protocol_max: offer.summary.protocol_range.max.major,
        };
        let next = LanPairingWindow {
            discovery,
            advertisement,
            offer,
            pending: HashMap::new(),
        };
        let snapshot = next.snapshot(now_ms);
        *window = Some(next);
        Ok(snapshot)
    }

    pub(crate) fn start(
        &self,
        offer: RemotePairingOffer,
        direct_origin: &str,
        display_name: &str,
        now_ms: i64,
    ) -> VibexResult<RemoteLanPairingWindowSnapshot> {
        let direct_origin = normalize_https_origin(direct_origin)?;
        if !offer.summary.direct_candidates.iter().any(|candidate| {
            candidate.transport == RemotePairingTransport::Direct
                && normalize_https_origin(&candidate.url)
                    .is_ok_and(|candidate_origin| candidate_origin == direct_origin)
        }) {
            return Err(remote_error(
                "remote_pairing_entry_route_mismatch",
                "LAN discovery origin is not an offered Direct pairing route",
            ));
        }
        if offer.summary.expires_at_ms <= now_ms
            || offer.summary.canceled
            || offer.summary.claimed_device_id.is_some()
        {
            return Err(remote_error(
                "remote_lan_pairing_window_unavailable",
                "LAN pairing window is already unavailable",
            ));
        }
        let display_name = bounded_display_name(display_name)?;
        let mut window = self.window.lock().map_err(|_| state_error())?;
        if window.as_ref().is_some_and(|current| {
            current.discovery.expires_at_ms > now_ms
                && !current.offer.summary.canceled
                && current.offer.summary.claimed_device_id.is_none()
        }) {
            return Err(VibexError::conflict(
                "remote_lan_pairing_window_active",
                "another LAN pairing window is already active",
            ));
        }

        let window_id = RequestId::new();
        let advertisement_id = secure_secret("adv");
        let service_instance = service_instance_name(&display_name, &advertisement_id);
        let discovery = RemoteLanPairingDiscoverySummary {
            schema_version: REMOTE_LAN_PAIRING_SCHEMA_VERSION.to_string(),
            window_id,
            server_id: offer.summary.server_id.clone(),
            server_identity_public_key: offer.summary.server_identity_public_key.clone(),
            protocol_range: offer.summary.protocol_range,
            permission_level: offer.summary.permission_level,
            expires_at_ms: offer.summary.expires_at_ms,
        };
        let advertisement = RemoteLanPairingAdvertisement {
            advertisement_id,
            service_instance,
            display_name,
            direct_origin,
            protocol_min: offer.summary.protocol_range.min.major,
            protocol_max: offer.summary.protocol_range.max.major,
        };
        let next = LanPairingWindow {
            discovery,
            advertisement,
            offer,
            pending: HashMap::new(),
        };
        let snapshot = next.snapshot(now_ms);
        *window = Some(next);
        Ok(snapshot)
    }

    pub(crate) fn discovery(&self, now_ms: i64) -> VibexResult<RemoteLanPairingDiscoverySummary> {
        let mut window = self.window.lock().map_err(|_| state_error())?;
        let window = active_window_mut(&mut window, now_ms)?;
        Ok(window.discovery.clone())
    }

    pub(crate) fn snapshot(&self, now_ms: i64) -> VibexResult<RemoteLanPairingWindowSnapshot> {
        let mut window = self.window.lock().map_err(|_| state_error())?;
        let window = active_window_mut(&mut window, now_ms)?;
        Ok(window.snapshot(now_ms))
    }

    pub(crate) fn submit(
        &self,
        request: RemoteLanPairingRequest,
        now_ms: i64,
    ) -> VibexResult<RemoteLanPairingRequestAccepted> {
        validate_request(&request)?;
        let request_secret_hash = self.hash_secret(&request.request_secret)?;
        let mut window = self.window.lock().map_err(|_| state_error())?;
        let window = active_window_mut(&mut window, now_ms)?;
        if window.discovery.window_id != request.window_id {
            return Err(window_unavailable());
        }

        if let Some(existing) = window
            .pending
            .values()
            .find(|pending| pending.idempotency_key == request.idempotency_key)
        {
            if existing.device_identity_public_key == request.device_identity_public_key
                && existing.display_name == request.display_name.trim()
                && existing.client_nonce == request.client_nonce
                && self.verify_secret_hash(&request.request_secret, &existing.request_secret_hash)
            {
                return Ok(existing.accepted(window.discovery.expires_at_ms));
            }
            return Err(VibexError::conflict(
                "remote_lan_pairing_request_conflict",
                "LAN pairing idempotency key is already bound to another request",
            ));
        }
        if window.pending.values().any(|pending| {
            pending.device_identity_public_key == request.device_identity_public_key
                && pending.client_nonce == request.client_nonce
        }) {
            return Err(VibexError::conflict(
                "remote_lan_pairing_request_conflict",
                "LAN pairing device identity and nonce are already pending",
            ));
        }
        if window.pending.len() >= LAN_PAIRING_MAX_PENDING_REQUESTS {
            return Err(VibexError::conflict(
                "remote_lan_pairing_request_limit",
                "LAN pairing window reached its pending request limit",
            ));
        }

        let request_id = RequestId::new();
        let verification_code = remote_lan_pairing_verification_code(
            &window.discovery.window_id,
            &request_id,
            &window.discovery.server_id,
            &window.discovery.server_identity_public_key,
            &request.device_identity_public_key,
            &request.client_nonce,
        );
        let pending = PendingRequest {
            request_id: request_id.clone(),
            device_identity_public_key: request.device_identity_public_key,
            display_name: request.display_name.trim().to_string(),
            client_nonce: request.client_nonce,
            idempotency_key: request.idempotency_key,
            request_secret_hash,
            verification_code: verification_code.clone(),
            state: RemoteLanPairingRequestState::Pending,
            last_poll_at_ms: None,
        };
        window.pending.insert(request_id.clone(), pending);
        Ok(RemoteLanPairingRequestAccepted {
            request_id,
            verification_code,
            expires_at_ms: window.discovery.expires_at_ms,
        })
    }

    pub(crate) fn status(
        &self,
        request: RemoteLanPairingStatusRequest,
        now_ms: i64,
    ) -> VibexResult<RemoteLanPairingStatusResponse> {
        validate_request_secret(&request.request_secret)?;
        let mut stored_window = self.window.lock().map_err(|_| state_error())?;
        let window = stored_window.as_mut().ok_or_else(window_unavailable)?;
        let pending = window.pending.get_mut(&request.request_id).ok_or_else(|| {
            remote_error(
                "remote_lan_pairing_request_unauthorized",
                "LAN pairing request credentials are invalid",
            )
        })?;
        if !self.verify_secret_hash(&request.request_secret, &pending.request_secret_hash) {
            return Err(remote_error(
                "remote_lan_pairing_request_unauthorized",
                "LAN pairing request credentials are invalid",
            ));
        }
        if pending
            .last_poll_at_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < LAN_PAIRING_MIN_POLL_INTERVAL_MS)
        {
            return Err(VibexError::conflict(
                "remote_lan_pairing_poll_rate_limited",
                "LAN pairing status was polled too quickly",
            ));
        }
        pending.last_poll_at_ms = Some(now_ms);
        if window.discovery.expires_at_ms <= now_ms {
            *stored_window = None;
            return Ok(RemoteLanPairingStatusResponse {
                state: RemoteLanPairingRequestState::Expired,
                offer: None,
            });
        }
        let state = pending.state;
        let offer = (state == RemoteLanPairingRequestState::Approved).then(|| window.offer.clone());
        Ok(RemoteLanPairingStatusResponse { state, offer })
    }

    pub(crate) fn approve(
        &self,
        request_id: &RequestId,
        now_ms: i64,
    ) -> VibexResult<RemoteLanPairingWindowSnapshot> {
        let mut window = self.window.lock().map_err(|_| state_error())?;
        let window = active_window_mut(&mut window, now_ms)?;
        if window
            .pending
            .values()
            .any(|pending| pending.state == RemoteLanPairingRequestState::Approved)
        {
            return Err(VibexError::conflict(
                "remote_lan_pairing_request_already_approved",
                "LAN pairing window already approved another request",
            ));
        }
        let target = window.pending.get(request_id).ok_or_else(|| {
            remote_error(
                "remote_lan_pairing_request_unknown",
                "LAN pairing request is unknown",
            )
        })?;
        if target.state != RemoteLanPairingRequestState::Pending {
            return Err(VibexError::conflict(
                "remote_lan_pairing_request_unavailable",
                "LAN pairing request is no longer pending",
            ));
        }
        for pending in window.pending.values_mut() {
            pending.state = if &pending.request_id == request_id {
                RemoteLanPairingRequestState::Approved
            } else if pending.state == RemoteLanPairingRequestState::Pending {
                RemoteLanPairingRequestState::Rejected
            } else {
                pending.state
            };
        }
        Ok(window.snapshot(now_ms))
    }

    pub(crate) fn reject(
        &self,
        request_id: &RequestId,
        now_ms: i64,
    ) -> VibexResult<RemoteLanPairingWindowSnapshot> {
        let mut window = self.window.lock().map_err(|_| state_error())?;
        let window = active_window_mut(&mut window, now_ms)?;
        let pending = window.pending.get_mut(request_id).ok_or_else(|| {
            remote_error(
                "remote_lan_pairing_request_unknown",
                "LAN pairing request is unknown",
            )
        })?;
        if pending.state != RemoteLanPairingRequestState::Pending {
            return Err(VibexError::conflict(
                "remote_lan_pairing_request_unavailable",
                "LAN pairing request is no longer pending",
            ));
        }
        pending.state = RemoteLanPairingRequestState::Rejected;
        Ok(window.snapshot(now_ms))
    }

    pub(crate) fn active_offer_id(&self) -> Option<RequestId> {
        self.window.lock().ok().and_then(|window| {
            window
                .as_ref()
                .map(|window| window.offer.summary.offer_id.clone())
        })
    }

    pub(crate) fn active_direct_origin(&self, now_ms: i64) -> Option<String> {
        self.window.lock().ok().and_then(|window| {
            window
                .as_ref()
                .filter(|window| window.discovery.expires_at_ms > now_ms)
                .map(|window| window.advertisement.direct_origin.clone())
        })
    }

    pub(crate) fn clear_offer(&self, offer_id: &RequestId) -> VibexResult<bool> {
        let mut window = self.window.lock().map_err(|_| state_error())?;
        let matches = window
            .as_ref()
            .is_some_and(|window| &window.offer.summary.offer_id == offer_id);
        if matches {
            *window = None;
        }
        Ok(matches)
    }

    fn hash_secret(&self, secret: &str) -> VibexResult<[u8; 32]> {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret_hash_key).map_err(|_| state_error())?;
        mac.update(secret.as_bytes());
        Ok(mac.finalize().into_bytes().into())
    }

    fn verify_secret_hash(&self, secret: &str, expected: &[u8; 32]) -> bool {
        HmacSha256::new_from_slice(&self.secret_hash_key)
            .map(|mut mac| {
                mac.update(secret.as_bytes());
                mac.verify_slice(expected).is_ok()
            })
            .unwrap_or(false)
    }
}

impl LanPairingWindow {
    fn snapshot(&self, now_ms: i64) -> RemoteLanPairingWindowSnapshot {
        let mut pending_requests = self
            .pending
            .values()
            .map(|pending| RemoteLanPairingPendingRequestSummary {
                request_id: pending.request_id.clone(),
                display_name: pending.display_name.clone(),
                device_fingerprint: remote_lan_pairing_device_fingerprint(
                    &pending.device_identity_public_key,
                ),
                verification_code: pending.verification_code.clone(),
                state: if self.discovery.expires_at_ms <= now_ms {
                    RemoteLanPairingRequestState::Expired
                } else {
                    pending.state
                },
                expires_at_ms: self.discovery.expires_at_ms,
            })
            .collect::<Vec<_>>();
        pending_requests
            .sort_by(|left, right| left.request_id.as_str().cmp(right.request_id.as_str()));
        RemoteLanPairingWindowSnapshot {
            discovery: self.discovery.clone(),
            advertisement: self.advertisement.clone(),
            pending_requests,
        }
    }
}

impl PendingRequest {
    fn accepted(&self, expires_at_ms: i64) -> RemoteLanPairingRequestAccepted {
        RemoteLanPairingRequestAccepted {
            request_id: self.request_id.clone(),
            verification_code: self.verification_code.clone(),
            expires_at_ms,
        }
    }
}

fn active_window_mut(
    window: &mut Option<LanPairingWindow>,
    now_ms: i64,
) -> VibexResult<&mut LanPairingWindow> {
    if window.as_ref().is_none_or(|window| {
        window.discovery.expires_at_ms <= now_ms
            || window.offer.summary.canceled
            || window.offer.summary.claimed_device_id.is_some()
    }) {
        *window = None;
        return Err(window_unavailable());
    }
    window.as_mut().ok_or_else(window_unavailable)
}

fn validate_request(request: &RemoteLanPairingRequest) -> VibexResult<()> {
    validate_public_key(&request.device_identity_public_key)?;
    let display_name = request.display_name.trim();
    if display_name.is_empty() || display_name.chars().count() > 128 {
        return Err(VibexError::validation(
            "remote_device_display_name_invalid",
            "LAN pairing device display name must be non-empty and bounded",
        ));
    }
    validate_bounded_token(
        &request.client_nonce,
        16,
        256,
        "remote_lan_pairing_verification_invalid",
        "LAN pairing client nonce is invalid",
    )?;
    validate_request_secret(&request.request_secret)?;
    validate_bounded_token(
        &request.idempotency_key,
        8,
        128,
        "remote_lan_pairing_request_invalid",
        "LAN pairing idempotency key is invalid",
    )
}

fn validate_request_secret(secret: &str) -> VibexResult<()> {
    validate_bounded_token(
        secret,
        22,
        256,
        "remote_lan_pairing_request_unauthorized",
        "LAN pairing request secret is invalid",
    )?;
    let encoded = secret
        .rsplit_once('-')
        .map_or(secret, |(_, encoded)| encoded);
    if !URL_SAFE_NO_PAD
        .decode(encoded)
        .is_ok_and(|bytes| bytes.len() >= 16)
    {
        return Err(remote_error(
            "remote_lan_pairing_request_unauthorized",
            "LAN pairing request secret is invalid",
        ));
    }
    Ok(())
}

fn validate_bounded_token(
    value: &str,
    min: usize,
    max: usize,
    code: &'static str,
    message: &'static str,
) -> VibexResult<()> {
    if value.len() < min
        || value.len() > max
        || value.chars().any(char::is_whitespace)
        || !value.is_ascii()
    {
        return Err(VibexError::validation(code, message));
    }
    Ok(())
}

pub(crate) fn normalize_https_origin(value: &str) -> VibexResult<String> {
    let url = Url::parse(value.trim()).map_err(|_| {
        VibexError::validation(
            "remote_lan_discovery_invalid",
            "LAN pairing origin is invalid",
        )
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(VibexError::validation(
            "remote_lan_discovery_invalid",
            "LAN pairing requires an exact HTTPS origin",
        ));
    }
    Ok(url.origin().ascii_serialization())
}

fn bounded_display_name(value: &str) -> VibexResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 63 || value.len() > 192 {
        return Err(VibexError::validation(
            "remote_lan_display_name_invalid",
            "LAN pairing desktop display name must be non-empty and bounded",
        ));
    }
    Ok(value.to_string())
}

fn service_instance_name(display_name: &str, advertisement_id: &str) -> String {
    let suffix = advertisement_id
        .rsplit_once('-')
        .map_or(advertisement_id, |(_, suffix)| suffix);
    let suffix = suffix.get(..8).unwrap_or(suffix);
    let max_prefix_bytes = 63usize.saturating_sub(suffix.len() + 1);
    let mut prefix = display_name.to_string();
    while prefix.len() > max_prefix_bytes {
        prefix.pop();
    }
    format!("{}-{suffix}", prefix.trim_end())
}

fn window_unavailable() -> VibexError {
    remote_error(
        "remote_lan_pairing_window_unavailable",
        "LAN pairing window is unavailable",
    )
}

fn state_error() -> VibexError {
    VibexError::process(
        "remote_lan_pairing_state_unavailable",
        "LAN pairing runtime state is unavailable",
    )
}

fn remote_error(code: &'static str, message: &'static str) -> VibexError {
    VibexError::new(ErrorCategory::Remote, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        RemoteActionClass, RemoteDevicePermissionLevel, RemotePairingCandidate,
        RemotePairingOfferSummary, RemoteProtocolVersionRange,
    };

    fn offer(expires_at_ms: i64) -> RemotePairingOffer {
        RemotePairingOffer {
            summary: RemotePairingOfferSummary {
                format_version: 1,
                protocol_range: RemoteProtocolVersionRange::v2(),
                server_id: "desktop-test".into(),
                server_identity_public_key: URL_SAFE_NO_PAD.encode([7u8; 32]),
                offer_id: RequestId::new(),
                expires_at_ms,
                direct_candidates: vec![RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: "https://desktop.example.test/".into(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                }],
                relay_candidate: None,
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                granted_permissions: vec![RemoteActionClass::ReadAgentSession],
                canceled: false,
                claimed_device_id: None,
            },
            one_time_challenge: secure_secret("pair"),
        }
    }

    fn request(window_id: RequestId) -> RemoteLanPairingRequest {
        RemoteLanPairingRequest {
            window_id,
            device_identity_public_key: URL_SAFE_NO_PAD.encode([9u8; 32]),
            display_name: "Vibex Mobile".into(),
            client_nonce: "client-nonce-0123456789".into(),
            request_secret: format!("secret-{}", URL_SAFE_NO_PAD.encode([11u8; 32])),
            idempotency_key: "idempotency-0123456789".into(),
        }
    }

    #[test]
    fn request_is_idempotent_and_approval_has_one_winner() {
        let coordinator = LanPairingCoordinator::default();
        let snapshot = coordinator
            .start(
                offer(100_000),
                "https://desktop.example.test",
                "Desktop",
                10_000,
            )
            .unwrap();
        let first_request = request(snapshot.discovery.window_id.clone());
        let first = coordinator.submit(first_request.clone(), 10_100).unwrap();
        assert_eq!(coordinator.submit(first_request, 10_200).unwrap(), first);

        let mut second_request = request(snapshot.discovery.window_id);
        second_request.device_identity_public_key = URL_SAFE_NO_PAD.encode([10u8; 32]);
        second_request.client_nonce = "client-nonce-9876543210".into();
        second_request.request_secret = format!("secret-{}", URL_SAFE_NO_PAD.encode([12u8; 32]));
        second_request.idempotency_key = "idempotency-9876543210".into();
        let second = coordinator.submit(second_request, 10_300).unwrap();

        let approved = coordinator.approve(&first.request_id, 10_400).unwrap();
        assert_eq!(
            approved
                .pending_requests
                .iter()
                .find(|pending| pending.request_id == first.request_id)
                .unwrap()
                .state,
            RemoteLanPairingRequestState::Approved
        );
        assert_eq!(
            approved
                .pending_requests
                .iter()
                .find(|pending| pending.request_id == second.request_id)
                .unwrap()
                .state,
            RemoteLanPairingRequestState::Rejected
        );
        assert_eq!(
            coordinator
                .approve(&second.request_id, 10_500)
                .unwrap_err()
                .code,
            "remote_lan_pairing_request_already_approved"
        );
    }

    #[test]
    fn status_requires_secret_and_only_approved_releases_offer() {
        let coordinator = LanPairingCoordinator::default();
        let snapshot = coordinator
            .start(
                offer(100_000),
                "https://desktop.example.test",
                "Desktop",
                10_000,
            )
            .unwrap();
        let request = request(snapshot.discovery.window_id);
        let secret = request.request_secret.clone();
        let accepted = coordinator.submit(request, 10_100).unwrap();
        let pending = coordinator
            .status(
                RemoteLanPairingStatusRequest {
                    request_id: accepted.request_id.clone(),
                    request_secret: secret.clone(),
                },
                10_600,
            )
            .unwrap();
        assert_eq!(pending.state, RemoteLanPairingRequestState::Pending);
        assert!(pending.offer.is_none());

        coordinator.approve(&accepted.request_id, 10_700).unwrap();
        let approved = coordinator
            .status(
                RemoteLanPairingStatusRequest {
                    request_id: accepted.request_id.clone(),
                    request_secret: secret,
                },
                11_100,
            )
            .unwrap();
        assert_eq!(approved.state, RemoteLanPairingRequestState::Approved);
        assert!(approved.offer.is_some());

        let error = coordinator
            .status(
                RemoteLanPairingStatusRequest {
                    request_id: accepted.request_id,
                    request_secret: format!("secret-{}", URL_SAFE_NO_PAD.encode([99u8; 32])),
                },
                11_700,
            )
            .unwrap_err();
        assert_eq!(error.code, "remote_lan_pairing_request_unauthorized");
    }

    #[test]
    fn authenticated_expiry_response_clears_window_and_secret_hashes() {
        let coordinator = LanPairingCoordinator::default();
        let snapshot = coordinator
            .start(
                offer(10_500),
                "https://desktop.example.test",
                "Desktop",
                10_000,
            )
            .unwrap();
        let request = request(snapshot.discovery.window_id);
        let secret = request.request_secret.clone();
        let accepted = coordinator.submit(request, 10_100).unwrap();

        let expired = coordinator
            .status(
                RemoteLanPairingStatusRequest {
                    request_id: accepted.request_id,
                    request_secret: secret,
                },
                10_501,
            )
            .unwrap();
        assert_eq!(expired.state, RemoteLanPairingRequestState::Expired);
        assert!(expired.offer.is_none());
        assert!(coordinator.active_offer_id().is_none());
        assert_eq!(
            coordinator.snapshot(10_502).unwrap_err().code,
            "remote_lan_pairing_window_unavailable"
        );
    }

    #[test]
    fn pending_request_count_is_bounded_per_window() {
        let coordinator = LanPairingCoordinator::default();
        let snapshot = coordinator
            .start(
                offer(100_000),
                "https://desktop.example.test",
                "Desktop",
                10_000,
            )
            .unwrap();
        for index in 0..LAN_PAIRING_MAX_PENDING_REQUESTS {
            let mut request = request(snapshot.discovery.window_id.clone());
            request.device_identity_public_key = URL_SAFE_NO_PAD.encode([index as u8 + 20; 32]);
            request.client_nonce = format!("client-nonce-{index:016}");
            request.request_secret =
                format!("secret-{}", URL_SAFE_NO_PAD.encode([index as u8 + 40; 32]));
            request.idempotency_key = format!("idempotency-{index:016}");
            coordinator.submit(request, 10_100 + index as i64).unwrap();
        }

        let mut overflow = request(snapshot.discovery.window_id);
        overflow.device_identity_public_key = URL_SAFE_NO_PAD.encode([99u8; 32]);
        overflow.client_nonce = "client-nonce-overflow-0001".into();
        overflow.request_secret = format!("secret-{}", URL_SAFE_NO_PAD.encode([100u8; 32]));
        overflow.idempotency_key = "idempotency-overflow-0001".into();
        assert_eq!(
            coordinator.submit(overflow, 10_200).unwrap_err().code,
            "remote_lan_pairing_request_limit"
        );
    }

    #[test]
    fn debug_output_never_contains_request_secret_or_offer_challenge() {
        let secret = format!("secret-{}", URL_SAFE_NO_PAD.encode([11u8; 32]));
        let request = request(RequestId::new());
        let request_debug = format!("{request:?}");
        assert!(!request_debug.contains(&secret));

        let response = RemoteLanPairingStatusResponse {
            state: RemoteLanPairingRequestState::Approved,
            offer: Some(offer(100_000)),
        };
        let challenge = response.offer.as_ref().unwrap().one_time_challenge.clone();
        assert!(!format!("{response:?}").contains(&challenge));
    }
}

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use url::Url;
use vibex_backend::{BackendError, BackendResult};
use vibex_core::{
    DeviceId, REMOTE_LAN_PAIRING_SCHEMA_VERSION, RemoteClaimPairingOfferResponse,
    RemoteDeviceStatus, RemoteLanPairingDiscoverySummary, RemoteLanPairingRequest,
    RemoteLanPairingRequestAccepted, RemoteLanPairingRequestState, RemoteLanPairingStatusRequest,
    RemoteLanPairingStatusResponse, RemotePairingOffer, RemotePairingTransport,
    RemoteProtocolVersionRange, RequestId, remote_lan_pairing_verification_code, unix_timestamp_ms,
};

use crate::credentials::ClientDeviceIdentity;
use crate::pairing::{pairing_claim_request, validate_pairing_offer};
use crate::transport::http_json;
#[cfg(target_os = "android")]
use crate::transport::remote_http_client;

const LAN_DISCOVERY_PATH: &str = "/api/v2/pairing/lan";
const LAN_REQUEST_PATH: &str = "/api/v2/pairing/lan/request";
const LAN_STATUS_PATH: &str = "/api/v2/pairing/lan/status";
const PAIRING_CLAIM_PATH: &str = "/api/v2/pairing/claim";
const WS_PATH: &str = "/ws/v2";
const WS_TICKET_PATH: &str = "/api/v2/ws-ticket";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LanGatewayInfo {
    server_id: String,
    server_identity_public_key: String,
    protocol_range: RemoteProtocolVersionRange,
    ws_path: String,
    pairing_claim_path: String,
    lan_pairing_discovery_path: String,
    lan_pairing_request_path: String,
    lan_pairing_status_path: String,
    ws_ticket_path: String,
    deployment_mode: String,
    tls_policy: String,
}

#[derive(Clone)]
pub struct LanPairingSession {
    client: reqwest::Client,
    origin: String,
    discovery: RemoteLanPairingDiscoverySummary,
    request_id: RequestId,
    verification_code: String,
    expires_at_ms: i64,
    identity: ClientDeviceIdentity,
    client_nonce: String,
    request_secret: String,
    display_name: String,
}

impl fmt::Debug for LanPairingSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanPairingSession")
            .field("origin", &self.origin)
            .field("server_id", &self.discovery.server_id)
            .field("request_id", &self.request_id)
            .field("verification_code", &self.verification_code)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("has_device_identity", &true)
            .field("has_client_nonce", &!self.client_nonce.is_empty())
            .field("has_request_secret", &!self.request_secret.is_empty())
            .field("display_name", &self.display_name)
            .finish()
    }
}

pub struct LanPairingClaim {
    pub offer: RemotePairingOffer,
    pub response: RemoteClaimPairingOfferResponse,
    pub identity: ClientDeviceIdentity,
    pub origin: String,
}

impl fmt::Debug for LanPairingClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanPairingClaim")
            .field("offer", &self.offer)
            .field("response", &self.response)
            .field("has_device_identity", &true)
            .field("origin", &self.origin)
            .finish()
    }
}

impl LanPairingSession {
    pub async fn start(origin: impl Into<String>, display_name: &str) -> BackendResult<Self> {
        let origin = normalize_lan_https_origin(&origin.into())?;
        let display_name = validate_display_name(display_name)?;
        let client = lan_http_client()?;
        let discovery: RemoteLanPairingDiscoverySummary =
            http_json(client.get(endpoint_url(&origin, LAN_DISCOVERY_PATH)?)).await?;
        let info: LanGatewayInfo =
            http_json(client.get(endpoint_url(&origin, "/api/v2/info")?)).await?;
        validate_discovery(&discovery, &info, unix_timestamp_ms())?;

        let identity = ClientDeviceIdentity::generate(DeviceId::new())?;
        let client_nonce = random_token()?;
        let request_secret = random_token()?;
        let request = RemoteLanPairingRequest {
            window_id: discovery.window_id.clone(),
            device_identity_public_key: identity.public_key_base64(),
            display_name: display_name.clone(),
            client_nonce: client_nonce.clone(),
            request_secret: request_secret.clone(),
            idempotency_key: RequestId::new().into_string(),
        };
        let accepted: RemoteLanPairingRequestAccepted = http_json(
            client
                .post(endpoint_url(&origin, LAN_REQUEST_PATH)?)
                .json(&request),
        )
        .await?;
        let expected_code = remote_lan_pairing_verification_code(
            &discovery.window_id,
            &accepted.request_id,
            &discovery.server_id,
            &discovery.server_identity_public_key,
            &request.device_identity_public_key,
            &client_nonce,
        );
        if accepted.verification_code != expected_code
            || accepted.expires_at_ms > discovery.expires_at_ms
            || accepted.expires_at_ms <= unix_timestamp_ms()
        {
            return Err(BackendError::permission(
                "remote_lan_pairing_verification_invalid",
                "LAN pairing verification transcript did not match the server response",
            ));
        }

        Ok(Self {
            client,
            origin,
            discovery,
            request_id: accepted.request_id,
            verification_code: expected_code,
            expires_at_ms: accepted.expires_at_ms,
            identity,
            client_nonce,
            request_secret,
            display_name,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn server_id(&self) -> &str {
        &self.discovery.server_id
    }

    pub fn verification_code(&self) -> &str {
        &self.verification_code
    }

    pub fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    pub async fn poll(&self) -> BackendResult<RemoteLanPairingStatusResponse> {
        http_json(
            self.client
                .post(endpoint_url(&self.origin, LAN_STATUS_PATH)?)
                .json(&RemoteLanPairingStatusRequest {
                    request_id: self.request_id.clone(),
                    request_secret: self.request_secret.clone(),
                }),
        )
        .await
    }

    pub async fn claim_approved(
        &self,
        status: RemoteLanPairingStatusResponse,
    ) -> BackendResult<LanPairingClaim> {
        if status.state != RemoteLanPairingRequestState::Approved {
            return Err(BackendError::conflict(
                "remote_lan_pairing_request_not_approved",
                "LAN pairing request has not been approved",
            ));
        }
        let offer = status.offer.ok_or_else(|| {
            BackendError::failed(
                "remote_lan_pairing_offer_missing",
                "approved LAN pairing status omitted the pairing offer",
            )
        })?;
        validate_pairing_offer(&offer, unix_timestamp_ms())?;
        if offer.summary.server_id != self.discovery.server_id
            || offer.summary.server_identity_public_key != self.discovery.server_identity_public_key
        {
            return Err(BackendError::permission(
                "remote_lan_server_identity_mismatch",
                "LAN discovery and pairing offer identities did not match",
            ));
        }
        validate_offer_direct_origin(&offer, &self.origin)?;

        let request = pairing_claim_request(
            &offer,
            &self.display_name,
            self.identity.public_key_base64(),
            RequestId::new().into_string(),
        )?;
        let private_key = self.identity.private_key_base64();
        let response: RemoteClaimPairingOfferResponse = http_json(
            self.client
                .post(endpoint_url(&self.origin, PAIRING_CLAIM_PATH)?)
                .json(&request),
        )
        .await?;
        if response.device.status != RemoteDeviceStatus::Active
            || response.device.public_key.as_deref()
                != Some(request.device_identity_public_key.as_str())
            || response.device_grant_token.trim().is_empty()
        {
            return Err(BackendError::failed(
                "remote_pairing_claim_response_invalid",
                "pairing claim did not return the expected active device grant",
            ));
        }
        let identity = ClientDeviceIdentity::from_private_key_base64(
            response.device.device_id.clone(),
            &private_key,
        )?;
        Ok(LanPairingClaim {
            offer,
            response,
            identity,
            origin: self.origin.clone(),
        })
    }
}

pub fn normalize_lan_https_origin(value: &str) -> BackendResult<String> {
    let url = Url::parse(value.trim()).map_err(|_| invalid_discovery())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_discovery());
    }
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
        return Err(invalid_discovery());
    }
    Ok(origin)
}

fn validate_discovery(
    discovery: &RemoteLanPairingDiscoverySummary,
    info: &LanGatewayInfo,
    now_ms: i64,
) -> BackendResult<()> {
    if discovery.schema_version != REMOTE_LAN_PAIRING_SCHEMA_VERSION
        || discovery.server_id.trim().is_empty()
        || discovery.server_identity_public_key.trim().is_empty()
        || discovery.expires_at_ms <= now_ms
    {
        return Err(invalid_discovery());
    }
    if discovery
        .protocol_range
        .negotiate(RemoteProtocolVersionRange::v2())
        .is_none()
        || info
            .protocol_range
            .negotiate(RemoteProtocolVersionRange::v2())
            .is_none()
    {
        return Err(BackendError::unsupported(
            "remote_pairing_protocol_incompatible",
            "LAN pairing server does not support the current protocol",
        ));
    }
    if discovery.server_id != info.server_id
        || discovery.server_identity_public_key != info.server_identity_public_key
    {
        return Err(BackendError::permission(
            "remote_lan_server_identity_mismatch",
            "LAN discovery and gateway identities did not match",
        ));
    }
    if discovery.protocol_range != info.protocol_range
        || info.ws_path != WS_PATH
        || info.pairing_claim_path != PAIRING_CLAIM_PATH
        || info.lan_pairing_discovery_path != LAN_DISCOVERY_PATH
        || info.lan_pairing_request_path != LAN_REQUEST_PATH
        || info.lan_pairing_status_path != LAN_STATUS_PATH
        || info.ws_ticket_path != WS_TICKET_PATH
        || info.deployment_mode != "lan"
        || info.tls_policy != "trusted_https_proxy"
    {
        return Err(BackendError::permission(
            "remote_lan_gateway_policy_invalid",
            "LAN pairing gateway paths or Direct HTTPS policy are invalid",
        ));
    }
    Ok(())
}

fn validate_offer_direct_origin(offer: &RemotePairingOffer, origin: &str) -> BackendResult<()> {
    let matching = offer
        .summary
        .direct_candidates
        .iter()
        .filter(|candidate| candidate.transport == RemotePairingTransport::Direct)
        .filter(|candidate| candidate_origin(&candidate.url).as_deref() == Some(origin))
        .count();
    if matching != 1 {
        return Err(BackendError::permission(
            "remote_pairing_entry_route_mismatch",
            "LAN discovery origin does not match exactly one offered Direct route",
        ));
    }
    Ok(())
}

fn candidate_origin(value: &str) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    match url.scheme() {
        "https" => {}
        "wss" => url.set_scheme("https").ok()?,
        _ => return None,
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let origin = url.origin().ascii_serialization();
    (origin != "null").then_some(origin)
}

fn endpoint_url(origin: &str, path: &str) -> BackendResult<Url> {
    let mut url = Url::parse(origin).map_err(|_| invalid_discovery())?;
    url.set_path(path);
    Ok(url)
}

fn validate_display_name(value: &str) -> BackendResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 {
        return Err(BackendError::failed(
            "remote_device_display_name_invalid",
            "remote device display name must be non-empty and bounded",
        ));
    }
    Ok(value.to_string())
}

fn invalid_discovery() -> BackendError {
    BackendError::failed(
        "remote_lan_discovery_invalid",
        "LAN discovery result is not a strict HTTPS origin or valid discovery response",
    )
}

#[cfg(target_os = "android")]
fn lan_http_client() -> BackendResult<reqwest::Client> {
    remote_http_client()
}

#[cfg(not(target_os = "android"))]
fn lan_http_client() -> BackendResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| {
            BackendError::failed(
                "remote_http_client_build_failed",
                "LAN pairing HTTP TLS client could not be initialized",
            )
        })
}

fn random_token() -> BackendResult<String> {
    let mut bytes = [0u8; 32];
    #[cfg(not(target_family = "wasm"))]
    {
        use rand_core::RngCore as _;
        rand_core::OsRng.fill_bytes(&mut bytes);
    }
    #[cfg(target_family = "wasm")]
    {
        let crypto = web_sys::window()
            .and_then(|window| window.crypto().ok())
            .ok_or_else(|| {
                BackendError::failed(
                    "remote_lan_pairing_randomness_unavailable",
                    "browser cryptographic randomness is unavailable",
                )
            })?;
        crypto
            .get_random_values_with_u8_array(&mut bytes)
            .map_err(|_| {
                BackendError::failed(
                    "remote_lan_pairing_randomness_failed",
                    "browser cryptographic randomness failed",
                )
            })?;
    }
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        RemoteActionClass, RemoteDevicePermissionLevel, RemotePairingCandidate,
        RemotePairingOfferSummary,
    };

    #[test]
    fn discovery_origin_requires_an_exact_https_origin() {
        assert_eq!(
            normalize_lan_https_origin("https://desktop.example:443/").unwrap(),
            "https://desktop.example"
        );
        for invalid in [
            "http://desktop.example",
            "https://user@desktop.example",
            "https://desktop.example/path",
            "https://desktop.example?secret=x",
            "https://desktop.example/#fragment",
        ] {
            assert_eq!(
                normalize_lan_https_origin(invalid).unwrap_err().code,
                "remote_lan_discovery_invalid"
            );
        }
    }

    #[test]
    fn discovery_validation_binds_identity_paths_and_https_policy() {
        let discovery = discovery(10_000);
        let mut info = gateway_info();
        validate_discovery(&discovery, &info, 1).unwrap();
        info.server_identity_public_key = "other-key".into();
        assert_eq!(
            validate_discovery(&discovery, &info, 1).unwrap_err().code,
            "remote_lan_server_identity_mismatch"
        );
        let mut info = gateway_info();
        info.lan_pairing_status_path = "/future/status".into();
        assert_eq!(
            validate_discovery(&discovery, &info, 1).unwrap_err().code,
            "remote_lan_gateway_policy_invalid"
        );
        let mut info = gateway_info();
        info.protocol_range.max.minor = 1;
        assert_eq!(
            validate_discovery(&discovery, &info, 1).unwrap_err().code,
            "remote_lan_gateway_policy_invalid"
        );
    }

    #[test]
    fn approved_offer_must_match_one_exact_direct_origin() {
        let mut offer = offer();
        validate_offer_direct_origin(&offer, "https://desktop.example").unwrap();
        assert_eq!(
            validate_offer_direct_origin(&offer, "https://other.example")
                .unwrap_err()
                .code,
            "remote_pairing_entry_route_mismatch"
        );
        offer
            .summary
            .direct_candidates
            .push(RemotePairingCandidate {
                transport: RemotePairingTransport::Direct,
                url: "wss://desktop.example/another-path".into(),
                relay_room_id: None,
                relay_pc_peer_id: None,
                relay_pc_public_key: None,
            });
        assert_eq!(
            validate_offer_direct_origin(&offer, "https://desktop.example")
                .unwrap_err()
                .code,
            "remote_pairing_entry_route_mismatch"
        );
    }

    #[test]
    fn session_debug_does_not_expose_secret_or_private_key() {
        let identity = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
        let private_key = identity.private_key_base64();
        let session = LanPairingSession {
            client: lan_http_client().unwrap(),
            origin: "https://desktop.example".into(),
            discovery: discovery(10_000),
            request_id: RequestId::new(),
            verification_code: "482193".into(),
            expires_at_ms: 10_000,
            identity,
            client_nonce: "client-nonce-secret".into(),
            request_secret: "request-secret-value".into(),
            display_name: "Phone".into(),
        };
        let debug = format!("{session:?}");
        assert!(!debug.contains("client-nonce-secret"));
        assert!(!debug.contains("request-secret-value"));
        assert!(!debug.contains(&private_key));
    }

    fn discovery(expires_at_ms: i64) -> RemoteLanPairingDiscoverySummary {
        RemoteLanPairingDiscoverySummary {
            schema_version: REMOTE_LAN_PAIRING_SCHEMA_VERSION.into(),
            window_id: RequestId::new(),
            server_id: "desktop".into(),
            server_identity_public_key: "server-key".into(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            permission_level: RemoteDevicePermissionLevel::ReadOnly,
            expires_at_ms,
        }
    }

    fn gateway_info() -> LanGatewayInfo {
        LanGatewayInfo {
            server_id: "desktop".into(),
            server_identity_public_key: "server-key".into(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            ws_path: WS_PATH.into(),
            pairing_claim_path: PAIRING_CLAIM_PATH.into(),
            lan_pairing_discovery_path: LAN_DISCOVERY_PATH.into(),
            lan_pairing_request_path: LAN_REQUEST_PATH.into(),
            lan_pairing_status_path: LAN_STATUS_PATH.into(),
            ws_ticket_path: WS_TICKET_PATH.into(),
            deployment_mode: "lan".into(),
            tls_policy: "trusted_https_proxy".into(),
        }
    }

    fn offer() -> RemotePairingOffer {
        RemotePairingOffer {
            summary: RemotePairingOfferSummary {
                format_version: 1,
                protocol_range: RemoteProtocolVersionRange::v2(),
                server_id: "desktop".into(),
                server_identity_public_key: "server-key".into(),
                offer_id: RequestId::new(),
                expires_at_ms: i64::MAX,
                direct_candidates: vec![RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: "wss://desktop.example/ws/v2".into(),
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
}

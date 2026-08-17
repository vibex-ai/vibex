#![cfg(not(target_family = "wasm"))]

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand_core::RngCore;
use serde::de::DeserializeOwned;
use vibex_backend::{BackendError, BackendResult};
use vibex_core::{
    REMOTE_LOCAL_LAN_TLS_HOSTNAME, REMOTE_ZERO_CONFIG_LAN_PAIRING_SCHEMA_VERSION,
    RelayEncryptedFrame, RelayFrameKind, RelayPeerId, RelaySessionId,
    RemoteClaimPairingOfferResponse, RemoteLanPairingRequest, RemoteLanPairingRequestAccepted,
    RemoteLanPairingStatusRequest, RemoteLanPairingStatusResponse, RemotePairingOffer,
    RemotePairingTransport, RemoteZeroConfigLanPairingHello,
    RemoteZeroConfigLanPairingHelloAccepted, RequestId, remote_zero_config_lan_session_context,
    unix_timestamp_ms,
};
use vibex_relay::{RelayCryptoSuite, RelayKeypair, RelaySession, RelaySessionConfig};

use crate::credentials::ClientDeviceIdentity;
use crate::pairing::{pairing_claim_request, validate_pairing_offer};
use crate::transport::http_json;

const HELLO_PATH: &str = "/api/v2/pairing/lan-zero/hello";
const REQUEST_PATH: &str = "/api/v2/pairing/lan-zero/request";
const STATUS_PATH: &str = "/api/v2/pairing/lan-zero/status";
const CLAIM_PATH: &str = "/api/v2/pairing/lan-zero/claim";
const MAX_TLS_CERTIFICATE_BYTES: usize = 8 * 1024;

pub struct ZeroConfigLanPairingSession {
    client: reqwest::Client,
    origin: String,
    server_identity_public_key: String,
    local_network_url: String,
    lan_gateway_tls_certificate: String,
    discovery: vibex_core::RemoteLanPairingDiscoverySummary,
    session_id: RelaySessionId,
    session: RelaySession,
    request_id: RequestId,
    verification_code: String,
    expires_at_ms: i64,
    identity: ClientDeviceIdentity,
    request_secret: String,
    display_name: String,
}

impl fmt::Debug for ZeroConfigLanPairingSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZeroConfigLanPairingSession")
            .field("origin", &self.origin)
            .field(
                "server_identity_public_key",
                &self.server_identity_public_key,
            )
            .field("local_network_url", &self.local_network_url)
            .field(
                "has_lan_gateway_tls_certificate",
                &!self.lan_gateway_tls_certificate.is_empty(),
            )
            .field("session_id", &self.session_id)
            .field("request_id", &self.request_id)
            .field("verification_code", &self.verification_code)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("has_device_identity", &true)
            .field("has_request_secret", &true)
            .finish()
    }
}

impl ZeroConfigLanPairingSession {
    pub async fn start(
        origin: impl Into<String>,
        expected_server_id: &str,
        expected_server_identity_public_key: &str,
        display_name: &str,
    ) -> BackendResult<Self> {
        let origin = normalize_zero_config_lan_origin(&origin.into())?;
        let expected_server_id = expected_server_id.trim();
        let expected_server_identity_public_key = expected_server_identity_public_key.trim();
        if expected_server_id.is_empty()
            || expected_server_id.len() > 128
            || expected_server_id.chars().any(char::is_control)
        {
            return Err(BackendError::permission(
                "remote_zero_config_pairing_identity_invalid",
                "zero-config LAN pairing identity is invalid",
            ));
        }
        let expected_key = URL_SAFE_NO_PAD
            .decode(expected_server_identity_public_key)
            .ok()
            .filter(|key| key.len() == 32 && key.iter().any(|byte| *byte != 0))
            .ok_or_else(|| {
                BackendError::permission(
                    "remote_zero_config_pairing_identity_invalid",
                    "zero-config LAN pairing identity is invalid",
                )
            })?;
        let server_relay_public_key = STANDARD.encode(expected_key);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                BackendError::failed(
                    "remote_zero_config_pairing_client_build_failed",
                    "zero-config LAN pairing client could not be initialized",
                )
            })?;
        let client_keypair = RelayKeypair::generate();
        let client_peer_id = RelayPeerId::new();
        let client_nonce = random_token()?;
        let hello = RemoteZeroConfigLanPairingHello {
            client_peer_id: client_peer_id.clone(),
            client_ephemeral_public_key: URL_SAFE_NO_PAD.encode(client_keypair.public_key_bytes()),
            client_nonce,
        };
        let accepted: RemoteZeroConfigLanPairingHelloAccepted =
            http_json(client.post(endpoint_url(&origin, HELLO_PATH)?).json(&hello)).await?;
        let lan_gateway_tls_certificate =
            validate_tls_certificate(&accepted.lan_gateway_tls_certificate)?;
        let local_network_url = local_gateway_origin(&origin, accepted.lan_gateway_port)?;
        if accepted.schema_version != REMOTE_ZERO_CONFIG_LAN_PAIRING_SCHEMA_VERSION
            || accepted.server_identity_public_key != expected_server_identity_public_key
            || accepted.server_id != expected_server_id
            || accepted.discovery.server_identity_public_key != accepted.server_identity_public_key
            || accepted.discovery.expires_at_ms <= unix_timestamp_ms()
            || accepted.session_id.as_str().is_empty()
            || accepted.room_id.as_str().is_empty()
            || accepted.client_peer_id != client_peer_id
            || accepted.lan_gateway_port == 0
        {
            return Err(BackendError::permission(
                "remote_zero_config_pairing_identity_mismatch",
                "zero-config LAN pairing server identity did not match discovery",
            ));
        }
        let server_peer_id = accepted.server_peer_id.clone();
        let session_config = RelaySessionConfig {
            room_id: accepted.room_id.clone(),
            session_id: accepted.session_id.clone(),
            local_peer_id: client_peer_id,
            remote_peer_id: server_peer_id,
        };
        let session = RelaySession::establish_with_suite(
            &client_keypair,
            &server_relay_public_key,
            session_config,
            RelayCryptoSuite::DirectionalV2,
            Some(&remote_zero_config_lan_session_context(
                &hello.client_nonce,
                accepted.lan_gateway_port,
                &lan_gateway_tls_certificate,
            )),
        )
        .map_err(|_| {
            BackendError::permission(
                "remote_zero_config_pairing_session_failed",
                "zero-config LAN pairing encrypted session could not be established",
            )
        })?;
        let identity = ClientDeviceIdentity::generate(vibex_core::DeviceId::new())?;
        let client_nonce = random_token()?;
        let request_secret = random_token()?;
        let request = RemoteLanPairingRequest {
            window_id: accepted.discovery.window_id.clone(),
            device_identity_public_key: identity.public_key_base64(),
            display_name: validate_display_name(display_name)?,
            client_nonce: client_nonce.clone(),
            request_secret: request_secret.clone(),
            idempotency_key: RequestId::new().into_string(),
        };
        let mut session = session;
        let accepted_request: RemoteLanPairingRequestAccepted =
            send_encrypted(&client, &origin, REQUEST_PATH, &mut session, &request).await?;
        let expected_code = vibex_core::remote_lan_pairing_verification_code(
            &accepted.discovery.window_id,
            &accepted_request.request_id,
            &accepted.discovery.server_id,
            &accepted.discovery.server_identity_public_key,
            &request.device_identity_public_key,
            &client_nonce,
        );
        if accepted_request.verification_code != expected_code
            || accepted_request.expires_at_ms > accepted.discovery.expires_at_ms
            || accepted_request.expires_at_ms <= unix_timestamp_ms()
        {
            return Err(BackendError::permission(
                "remote_zero_config_pairing_verification_invalid",
                "zero-config LAN pairing verification transcript did not match",
            ));
        }
        Ok(Self {
            client,
            origin,
            server_identity_public_key: accepted.server_identity_public_key,
            local_network_url,
            lan_gateway_tls_certificate,
            discovery: accepted.discovery,
            session_id: accepted.session_id,
            session,
            request_id: accepted_request.request_id,
            verification_code: expected_code,
            expires_at_ms: accepted_request.expires_at_ms,
            identity,
            request_secret,
            display_name: validate_display_name(display_name)?,
        })
    }

    pub fn verification_code(&self) -> &str {
        &self.verification_code
    }

    pub fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    pub async fn poll(&mut self) -> BackendResult<RemoteLanPairingStatusResponse> {
        let request = RemoteLanPairingStatusRequest {
            request_id: self.request_id.clone(),
            request_secret: self.request_secret.clone(),
        };
        send_encrypted(
            &self.client,
            &self.origin,
            STATUS_PATH,
            &mut self.session,
            &request,
        )
        .await
    }

    pub async fn claim_approved(
        &mut self,
        status: RemoteLanPairingStatusResponse,
    ) -> BackendResult<ZeroConfigLanPairingClaim> {
        if status.state != vibex_core::RemoteLanPairingRequestState::Approved {
            return Err(BackendError::conflict(
                "remote_zero_config_pairing_not_approved",
                "zero-config LAN pairing request has not been approved",
            ));
        }
        let offer = status.offer.ok_or_else(|| {
            BackendError::failed(
                "remote_zero_config_pairing_offer_missing",
                "approved zero-config LAN pairing status omitted the offer",
            )
        })?;
        validate_pairing_offer(&offer, unix_timestamp_ms())?;
        if offer.summary.server_identity_public_key != self.server_identity_public_key
            || offer.summary.server_id != self.discovery.server_id
        {
            return Err(BackendError::permission(
                "remote_zero_config_pairing_identity_mismatch",
                "zero-config LAN pairing offer identity did not match discovery",
            ));
        }
        let expected_placeholder = placeholder_gateway_origin(&self.local_network_url)?;
        if offer
            .summary
            .direct_candidates
            .iter()
            .filter(|candidate| {
                candidate.transport == RemotePairingTransport::Direct
                    && url::Url::parse(&candidate.url)
                        .is_ok_and(|url| url.origin().ascii_serialization() == expected_placeholder)
            })
            .count()
            != 1
        {
            return Err(BackendError::permission(
                "remote_zero_config_pairing_route_mismatch",
                "zero-config LAN pairing offer did not match the encrypted local route",
            ));
        }
        let request = pairing_claim_request(
            &offer,
            &self.display_name,
            self.identity.public_key_base64(),
            RequestId::new().into_string(),
        )?;
        let response: RemoteClaimPairingOfferResponse = send_encrypted(
            &self.client,
            &self.origin,
            CLAIM_PATH,
            &mut self.session,
            &request,
        )
        .await?;
        if response.device.status != vibex_core::RemoteDeviceStatus::Active
            || response.device.public_key.as_deref()
                != Some(self.identity.public_key_base64().as_str())
            || response.device_grant_token.trim().is_empty()
        {
            return Err(BackendError::failed(
                "remote_zero_config_pairing_claim_invalid",
                "zero-config LAN pairing claim response was invalid",
            ));
        }
        Ok(ZeroConfigLanPairingClaim {
            offer,
            response,
            identity: self.identity.clone(),
            local_network_url: self.local_network_url.clone(),
            lan_gateway_tls_certificate: self.lan_gateway_tls_certificate.clone(),
        })
    }
}

pub struct ZeroConfigLanPairingClaim {
    pub offer: RemotePairingOffer,
    pub response: RemoteClaimPairingOfferResponse,
    pub identity: ClientDeviceIdentity,
    pub local_network_url: String,
    pub lan_gateway_tls_certificate: String,
}

async fn send_encrypted<T: serde::Serialize, R: DeserializeOwned>(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    session: &mut RelaySession,
    value: &T,
) -> BackendResult<R> {
    let payload = serde_json::to_value(value).map_err(|_| {
        BackendError::failed(
            "remote_zero_config_pairing_payload_invalid",
            "zero-config LAN pairing payload could not be encoded",
        )
    })?;
    let frame = session
        .seal_json(RelayFrameKind::PairRequest, None, payload)
        .map_err(BackendError::from)?;
    let response: RelayEncryptedFrame =
        http_json(client.post(endpoint_url(origin, path)?).json(&frame)).await?;
    let plaintext = session
        .open_json(&response)
        .map_err(BackendError::from)?
        .business_payload_json;
    serde_json::from_value(plaintext).map_err(|_| {
        BackendError::failed(
            "remote_zero_config_pairing_response_invalid",
            "zero-config LAN pairing response could not be decoded",
        )
    })
}

pub fn normalize_zero_config_lan_origin(value: &str) -> BackendResult<String> {
    let mut url = url::Url::parse(value.trim()).map_err(|_| invalid_origin())?;
    let Some(url::Host::Ipv4(address)) = url.host() else {
        return Err(invalid_origin());
    };
    if url.scheme() != "http"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !(address.is_loopback() || address.is_private() || address.is_link_local())
    {
        return Err(invalid_origin());
    }
    url.set_path("");
    Ok(url.origin().ascii_serialization())
}

fn local_gateway_origin(bootstrap_origin: &str, port: u16) -> BackendResult<String> {
    if port == 0 {
        return Err(BackendError::permission(
            "remote_zero_config_pairing_route_invalid",
            "zero-config LAN pairing returned an invalid local Gateway port",
        ));
    }
    let mut url = url::Url::parse(bootstrap_origin).map_err(|_| invalid_origin())?;
    url.set_scheme("https").map_err(|_| invalid_origin())?;
    url.set_port(Some(port)).map_err(|_| invalid_origin())?;
    url.set_path("");
    Ok(url.origin().ascii_serialization())
}

fn placeholder_gateway_origin(local_network_url: &str) -> BackendResult<String> {
    let url = url::Url::parse(local_network_url).map_err(|_| invalid_origin())?;
    let port = url.port_or_known_default().ok_or_else(invalid_origin)?;
    let placeholder = url::Url::parse(&format!("https://{REMOTE_LOCAL_LAN_TLS_HOSTNAME}:{port}"))
        .map_err(|_| invalid_origin())?;
    Ok(placeholder.origin().ascii_serialization())
}

fn validate_tls_certificate(encoded: &str) -> BackendResult<String> {
    let max_encoded_len = MAX_TLS_CERTIFICATE_BYTES.saturating_mul(4).div_ceil(3);
    let certificate = (encoded.len() <= max_encoded_len)
        .then(|| URL_SAFE_NO_PAD.decode(encoded).ok())
        .flatten()
        .filter(|certificate| {
            !certificate.is_empty() && certificate.len() <= MAX_TLS_CERTIFICATE_BYTES
        })
        .ok_or_else(|| {
            BackendError::permission(
                "remote_zero_config_pairing_route_invalid",
                "zero-config LAN pairing returned an invalid local TLS certificate",
            )
        })?;
    reqwest::Certificate::from_der(&certificate).map_err(|_| {
        BackendError::permission(
            "remote_zero_config_pairing_route_invalid",
            "zero-config LAN pairing returned an invalid local TLS certificate",
        )
    })?;
    Ok(encoded.to_string())
}

fn endpoint_url(origin: &str, path: &str) -> BackendResult<url::Url> {
    let mut url = url::Url::parse(origin).map_err(|_| invalid_origin())?;
    url.set_path(path);
    Ok(url)
}

fn random_token() -> BackendResult<String> {
    let mut bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut bytes);
    Ok(format!("local-{}", URL_SAFE_NO_PAD.encode(bytes)))
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

fn invalid_origin() -> BackendError {
    BackendError::failed(
        "remote_zero_config_pairing_origin_invalid",
        "zero-config LAN pairing origin is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_config_origin_requires_an_exact_http_origin() {
        assert_eq!(
            normalize_zero_config_lan_origin(" http://192.168.1.10:4321 ").unwrap(),
            "http://192.168.1.10:4321"
        );
        for invalid in [
            "https://192.168.1.10:4321",
            "http://user@192.168.1.10:4321",
            "http://192.168.1.10:4321/pair",
            "http://192.168.1.10:4321?token=secret",
            "http://192.168.1.10:4321#pair",
            "http://[fe80::1]:4321",
        ] {
            assert_eq!(
                normalize_zero_config_lan_origin(invalid).unwrap_err().code,
                "remote_zero_config_pairing_origin_invalid"
            );
        }
    }

    #[tokio::test]
    async fn zero_config_start_rejects_invalid_identity_before_network_use() {
        for invalid_key in ["invalid", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
            assert_eq!(
                ZeroConfigLanPairingSession::start(
                    "http://127.0.0.1:9",
                    "desktop-test",
                    invalid_key,
                    "Vibex Mobile",
                )
                .await
                .unwrap_err()
                .code,
                "remote_zero_config_pairing_identity_invalid"
            );
        }
    }
}

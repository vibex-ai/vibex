use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::IpAddr;
#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
#[cfg(target_family = "wasm")]
use futures_channel::mpsc;
use futures_channel::oneshot;
#[cfg(not(target_family = "wasm"))]
use futures_util::SinkExt;
use futures_util::StreamExt;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use url::Url;
use vibex_backend::{BackendBound, BackendError, BackendErrorKind, BackendFuture, BackendResult};
use vibex_core::{
    CorrelationId, RelayControlMessage, RelayError, RelayErrorCode, RelayFrameKind,
    RelayHandshakeHello, RelayPeerId, RelayPeerMessage, RelayPeerRole, RelayRoomId,
    RelayTransportMode, RemoteAttachRequestV2, RemoteAttachmentAcceptedV2, RemoteAuthProof,
    RemoteBinaryFrame, RemoteClaimPairingOfferRequest, RemoteClaimPairingOfferResponse,
    RemoteCloseCode, RemoteControlMessageV2, RemoteEventV2, RemoteHello, RemoteJsonMessageV2,
    RemotePing, RemoteProtocolVersionRange, RemoteRpcRequestV2, RemoteRpcResponseV2,
    RemoteServerInfoV2, RemoteStreamCursor, RemoteSubscribeRequestV2, RemoteSubscriptionAcceptedV2,
    RemoteTimeoutClass, RemoteWsTicketRequest, RemoteWsTicketResponse, RequestId, VibexError,
    unix_timestamp_ms,
};
use vibex_relay::{
    RELAY_CRYPTO_SUITE_V2, RelayCryptoSuite, RelayKeypair, RelaySession, RelaySessionConfig,
    relay_handshake_authentication_tag, relay_handshake_transcript,
    relay_transcript_hash_with_ephemeral, verify_relay_handshake_authentication_tag,
};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::credentials::{ClientDeviceIdentity, RemoteCredentialRecord};
use crate::sync::{DomainSyncEngine, SyncDecision};

#[cfg(target_family = "wasm")]
const REMOTE_V2_SUBPROTOCOL: &str = "vibex-v2";
#[cfg(target_family = "wasm")]
const REMOTE_V2_TICKET_PREFIX: &str = "vibex-ticket.";
const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 256;
const DEFAULT_BINARY_QUEUE_CAPACITY: usize = 128;
const MAX_DIRECT_CANDIDATES: usize = 16;
const MAX_EVENT_QUEUE_CAPACITY: usize = 8192;
const MAX_BINARY_QUEUE_CAPACITY: usize = 4096;
const MAX_HTTP_JSON_BYTES: usize = 64 * 1024;
const MAX_PINNED_TLS_CERTIFICATE_BYTES: usize = 8 * 1024;
const PROJECTION_INVALIDATION_CHANNEL_COUNT: usize = 4;
const RELAY_REMOTE_JSON_KIND: RelayFrameKind = RelayFrameKind::Command;

#[cfg(target_os = "android")]
pub(crate) fn remote_http_client() -> BackendResult<reqwest::Client> {
    android_remote_http_client(false)
}

#[cfg(target_os = "android")]
fn android_remote_http_client(no_proxy: bool) -> BackendResult<reqwest::Client> {
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|certificate| {
            reqwest::Certificate::from_der(certificate.as_ref()).map_err(|_| {
                BackendError::failed(
                    "remote_tls_root_certificate_invalid",
                    "bundled remote TLS root certificate is invalid",
                )
            })
        })
        .collect::<BackendResult<Vec<_>>>()?;
    let mut builder = reqwest::Client::builder()
        // Android's platform verifier can reject otherwise valid public chains
        // when the leaf omits an OCSP responder.  A Mozilla WebPKI store keeps
        // normal chain, hostname, signature, and validity checks in rustls
        // without accepting invalid certificates.
        .tls_certs_only(roots)
        .redirect(reqwest::redirect::Policy::none());
    if no_proxy {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|_| {
        BackendError::failed(
            "remote_http_client_build_failed",
            "remote HTTP TLS client could not be initialized",
        )
    })
}

#[cfg(not(target_os = "android"))]
pub(crate) fn remote_http_client() -> BackendResult<reqwest::Client> {
    Ok(reqwest::Client::new())
}

#[cfg(not(target_family = "wasm"))]
fn remote_http_client_for_url(url: &Url) -> BackendResult<reqwest::Client> {
    if !is_proxy_bypassed_remote_url(url) {
        return remote_http_client();
    }
    #[cfg(target_os = "android")]
    {
        android_remote_http_client(true)
    }
    #[cfg(not(target_os = "android"))]
    {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                BackendError::failed(
                    "remote_http_client_build_failed",
                    "direct remote HTTP client could not be initialized",
                )
            })
    }
}

#[cfg(target_family = "wasm")]
fn remote_http_client_for_url(_url: &Url) -> BackendResult<reqwest::Client> {
    remote_http_client()
}

#[cfg(not(target_family = "wasm"))]
fn remote_http_client_for_config(config: &RemoteClientConfig) -> BackendResult<reqwest::Client> {
    let Some(encoded) = config.pinned_tls_certificate_der.as_deref() else {
        return remote_http_client_for_url(&config.validate()?);
    };
    let certificate_der = decode_pinned_tls_certificate(encoded)?;
    let certificate = reqwest::Certificate::from_der(&certificate_der).map_err(|_| {
        BackendError::failed(
            "remote_tls_certificate_invalid",
            "pinned local network TLS certificate is invalid",
        )
    })?;
    reqwest::Client::builder()
        .tls_backend_rustls()
        .tls_certs_only([certificate])
        .tls_danger_accept_invalid_hostnames(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| {
            BackendError::failed(
                "remote_http_client_build_failed",
                "pinned local network HTTP client could not be initialized",
            )
        })
}

#[cfg(target_family = "wasm")]
fn remote_http_client_for_config(config: &RemoteClientConfig) -> BackendResult<reqwest::Client> {
    if config.pinned_tls_certificate_der.is_some() {
        return Err(BackendError::unsupported(
            "remote_pinned_tls_unsupported",
            "pinned local network TLS is unavailable in browser transports",
        ));
    }
    remote_http_client()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteConnectionState {
    Idle,
    Resolving,
    Probing,
    Connecting,
    Authenticating,
    Syncing,
    Online,
    Degraded,
    Reconnecting,
    Offline,
    Revoked,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnectionSnapshot {
    pub state: RemoteConnectionState,
    pub session_epoch: Option<u64>,
    pub reconnect_attempt: u32,
    pub next_retry_at_ms: Option<i64>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
}

impl Default for RemoteConnectionSnapshot {
    fn default() -> Self {
        Self {
            state: RemoteConnectionState::Idle,
            session_epoch: None,
            reconnect_attempt: 0,
            next_retry_at_ms: None,
            last_error_code: None,
            last_error_message: None,
        }
    }
}

impl RemoteConnectionSnapshot {
    pub fn transition(&mut self, next: RemoteConnectionState) {
        self.state = next;
        if matches!(next, RemoteConnectionState::Online) {
            self.reconnect_attempt = 0;
            self.next_retry_at_ms = None;
            self.last_error_code = None;
            self.last_error_message = None;
        }
    }

    pub fn record_error(&mut self, error: &BackendError) {
        self.last_error_code = Some(error.code.clone());
        self.last_error_message = Some(error.message.clone());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteLifecycleSignal {
    VisibilityChanged { visible: bool },
    NetworkChanged,
    ComputerSuspended,
    ComputerResumed,
    AppBackgrounded,
    AppResumed,
}

#[derive(Clone)]
pub struct RemoteClientConfig {
    pub base_url: String,
    pub pinned_tls_certificate_der: Option<String>,
    pub auth: RemoteAuthProof,
    pub device_identity: Option<ClientDeviceIdentity>,
    pub expected_server_id: Option<String>,
    pub expected_server_identity_public_key: Option<String>,
    pub client_id: String,
    pub app_version: String,
    pub client_type: vibex_core::RemoteClientType,
    pub allow_insecure_local_dev: bool,
    pub reconnect_initial: Duration,
    pub reconnect_max: Duration,
    pub max_reconnect_attempts: u32,
    pub event_queue_capacity: usize,
    pub binary_queue_capacity: usize,
}

impl fmt::Debug for RemoteClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteClientConfig")
            .field("base_url", &self.base_url)
            .field(
                "has_pinned_tls_certificate",
                &self.pinned_tls_certificate_der.is_some(),
            )
            .field("auth", &self.auth)
            .field("has_device_identity", &self.device_identity.is_some())
            .field("expected_server_id", &self.expected_server_id)
            .field(
                "has_expected_server_identity_public_key",
                &self.expected_server_identity_public_key.is_some(),
            )
            .field("client_id", &self.client_id)
            .field("app_version", &self.app_version)
            .field("client_type", &self.client_type)
            .field("allow_insecure_local_dev", &self.allow_insecure_local_dev)
            .field("reconnect_initial", &self.reconnect_initial)
            .field("reconnect_max", &self.reconnect_max)
            .field("max_reconnect_attempts", &self.max_reconnect_attempts)
            .finish()
    }
}

impl RemoteClientConfig {
    pub fn new(base_url: impl Into<String>, auth: RemoteAuthProof) -> Self {
        Self {
            base_url: base_url.into(),
            pinned_tls_certificate_der: None,
            auth,
            device_identity: None,
            expected_server_id: None,
            expected_server_identity_public_key: None,
            client_id: "vibex-mobile".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            client_type: vibex_core::RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            reconnect_initial: Duration::from_millis(250),
            reconnect_max: Duration::from_secs(15),
            max_reconnect_attempts: 8,
            event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            binary_queue_capacity: DEFAULT_BINARY_QUEUE_CAPACITY,
        }
    }

    pub fn with_device_identity(mut self, identity: ClientDeviceIdentity) -> Self {
        self.device_identity = Some(identity);
        self
    }

    pub fn from_credentials(
        record: RemoteCredentialRecord,
        identity: ClientDeviceIdentity,
    ) -> BackendResult<Self> {
        if identity.device_id() != &record.auth.device_id
            || identity.public_key_base64() != record.device_identity_public_key
        {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "stored client identity does not match the remote device grant",
            ));
        }
        let mut config = Self::new(record.server_url, record.auth).with_device_identity(identity);
        config.expected_server_identity_public_key = record.server_identity_public_key;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> BackendResult<Url> {
        let url = Url::parse(&self.base_url).map_err(|_| {
            BackendError::failed("remote_url_invalid", "remote server URL is invalid")
        })?;
        if !matches!(url.scheme(), "http" | "https" | "ws" | "wss")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(BackendError::failed(
                "remote_url_invalid",
                "remote server URL must use HTTP(S)/WS(S) without embedded credentials",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(BackendError::failed(
                "remote_url_secret_boundary_invalid",
                "remote server URL must not contain query or fragment data",
            ));
        }
        let secure = matches!(url.scheme(), "https" | "wss");
        let loopback = url
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
        if !secure && !(self.allow_insecure_local_dev && loopback) {
            return Err(BackendError::failed(
                "remote_secure_context_required",
                "remote Web entry requires HTTPS/WSS (loopback HTTP is a development exception)",
            ));
        }
        if let Some(certificate) = self.pinned_tls_certificate_der.as_deref() {
            decode_pinned_tls_certificate(certificate)?;
            if !secure
                || !url
                    .host_str()
                    .and_then(|host| host.parse::<IpAddr>().ok())
                    .is_some_and(is_local_network_ip)
            {
                return Err(BackendError::failed(
                    "remote_pinned_tls_route_invalid",
                    "pinned TLS requires an HTTPS/WSS local numeric address",
                ));
            }
        }
        if self.reconnect_initial.is_zero()
            || self.reconnect_max.is_zero()
            || self.reconnect_initial > self.reconnect_max
        {
            return Err(BackendError::failed(
                "remote_reconnect_range_invalid",
                "remote reconnect backoff range is invalid",
            ));
        }
        if self.event_queue_capacity == 0
            || self.binary_queue_capacity == 0
            || self.event_queue_capacity > MAX_EVENT_QUEUE_CAPACITY
            || self.binary_queue_capacity > MAX_BINARY_QUEUE_CAPACITY
        {
            return Err(BackendError::failed(
                "remote_queue_capacity_invalid",
                "remote event and binary queue capacities must be positive and bounded",
            ));
        }
        if self.max_reconnect_attempts > 32 {
            return Err(BackendError::failed(
                "remote_reconnect_attempts_invalid",
                "remote reconnect attempts must be bounded to 32 or fewer",
            ));
        }
        Ok(url)
    }

    pub fn credential_record(&self) -> Option<RemoteCredentialRecord> {
        self.device_identity
            .as_ref()
            .map(|identity| RemoteCredentialRecord {
                server_url: self.base_url.clone(),
                auth: self.auth.clone(),
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: self.expected_server_identity_public_key.clone(),
            })
    }
}

fn decode_pinned_tls_certificate(encoded: &str) -> BackendResult<Vec<u8>> {
    let max_encoded_len = MAX_PINNED_TLS_CERTIFICATE_BYTES
        .saturating_mul(4)
        .div_ceil(3);
    if encoded.is_empty() || encoded.len() > max_encoded_len {
        return Err(BackendError::failed(
            "remote_tls_certificate_invalid",
            "pinned local network TLS certificate is missing or too large",
        ));
    }
    let certificate = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        BackendError::failed(
            "remote_tls_certificate_invalid",
            "pinned local network TLS certificate is not valid base64url",
        )
    })?;
    if certificate.is_empty() || certificate.len() > MAX_PINNED_TLS_CERTIFICATE_BYTES {
        return Err(BackendError::failed(
            "remote_tls_certificate_invalid",
            "pinned local network TLS certificate is missing or too large",
        ));
    }
    #[cfg(not(target_family = "wasm"))]
    reqwest::Certificate::from_der(&certificate).map_err(|_| {
        BackendError::failed(
            "remote_tls_certificate_invalid",
            "pinned local network TLS certificate is invalid",
        )
    })?;
    Ok(certificate)
}

fn is_local_network_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
    }
}

fn is_proxy_bypassed_remote_url(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(is_local_network_ip)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGatewayInfo {
    pub server_id: String,
    pub server_identity_public_key: String,
    pub protocol_range: RemoteProtocolVersionRange,
    pub ws_path: String,
    pub pairing_claim_path: String,
    pub ws_ticket_path: String,
    pub deployment_mode: String,
    pub tls_policy: String,
    pub session_epoch: u64,
    #[serde(default)]
    pub enabled_features: Vec<String>,
    /// Relay mode carries a one-use proof challenge inside the E2EE Ready
    /// frame instead of obtaining an HTTP WS ticket.
    #[serde(default)]
    pub proof_challenge: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpointInfo {
    pub service_name: String,
    pub server_version: String,
    pub protocol_version: vibex_core::RelayProtocolVersion,
    pub features: RelayEndpointFeatures,
    pub limits: RelayEndpointLimits,
}

impl RelayEndpointInfo {
    pub fn validate_transport_capabilities(&self) -> BackendResult<()> {
        if !self.features.pc_websocket
            || !self.features.device_websocket
            || !self.features.websocket_frames
            || !self.features.http_pair_bridge
        {
            return Err(BackendError::unsupported(
                "relay_transport_unavailable",
                "self-hosted Relay does not expose the required mobile transport surface",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpointFeatures {
    pub pc_websocket: bool,
    #[serde(default)]
    pub device_websocket: bool,
    #[serde(default)]
    pub websocket_frames: bool,
    pub http_pair_bridge: bool,
    pub http_command_bridge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpointLimits {
    #[serde(default)]
    pub max_total_connections: usize,
    pub max_body_bytes: usize,
    #[serde(default)]
    pub max_queue_bytes_per_connection: usize,
    #[serde(default)]
    pub max_bandwidth_bytes_per_window: usize,
}

#[derive(Clone)]
pub struct RelayClientConfig {
    pub relay_url: String,
    pub room_id: RelayRoomId,
    pub local_peer_id: RelayPeerId,
    pub pc_peer_id: RelayPeerId,
    pub pc_public_key: Option<String>,
    pub remote: RemoteClientConfig,
}

#[derive(Clone)]
pub struct AutoRemoteTransportConfig {
    pub remote: RemoteClientConfig,
    pub direct_candidates: Vec<DirectCandidate>,
    pub relay: Option<RelayClientConfig>,
}

impl fmt::Debug for AutoRemoteTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AutoRemoteTransportConfig")
            .field("remote", &self.remote)
            .field("direct_candidate_count", &self.direct_candidates.len())
            .field("has_relay", &self.relay.is_some())
            .finish()
    }
}

impl fmt::Debug for RelayClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayClientConfig")
            .field("relay_url", &self.relay_url)
            .field("room_id", &self.room_id)
            .field("local_peer_id", &self.local_peer_id)
            .field("pc_peer_id", &self.pc_peer_id)
            .field("has_pc_public_key", &self.pc_public_key.is_some())
            .field("remote", &self.remote)
            .finish()
    }
}

impl RelayClientConfig {
    fn validate(&self) -> BackendResult<Url> {
        self.remote.validate()?;
        if self.local_peer_id == self.pc_peer_id {
            return Err(BackendError::failed(
                "relay_peer_identity_invalid",
                "relay device and PC peer ids must differ",
            ));
        }
        let url = Url::parse(&self.relay_url).map_err(|_| {
            BackendError::failed("relay_url_invalid", "self-hosted Relay URL is invalid")
        })?;
        if !matches!(url.scheme(), "http" | "https" | "ws" | "wss")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(BackendError::failed(
                "relay_url_invalid",
                "self-hosted Relay URL must not contain credentials, query, or fragment",
            ));
        }
        let secure = matches!(url.scheme(), "https" | "wss");
        let loopback = url
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
        if !secure && !(self.remote.allow_insecure_local_dev && loopback) {
            return Err(BackendError::failed(
                "relay_secure_context_required",
                "self-hosted Relay requires HTTPS/WSS outside loopback development",
            ));
        }
        Ok(url)
    }
}

/// Claim a short-lived pairing offer without opening a WebSocket or retaining
/// the one-time challenge in this client.  The caller should immediately move
/// the returned grant and device identity into its selected credential stores.
pub fn claim_pairing_offer(
    base_url: impl Into<String>,
    request: RemoteClaimPairingOfferRequest,
    allow_insecure_local_dev: bool,
) -> BackendFuture<'static, RemoteClaimPairingOfferResponse> {
    let base_url = base_url.into();
    Box::pin(async move {
        if request.one_time_challenge.trim().is_empty()
            || request.claim_nonce.trim().is_empty()
            || request.device_identity_public_key.trim().is_empty()
        {
            return Err(BackendError::failed(
                "remote_pairing_request_invalid",
                "pairing claim requires a challenge, nonce, and device identity",
            ));
        }
        let mut url = Url::parse(&base_url).map_err(|_| {
            BackendError::failed("remote_url_invalid", "remote server URL is invalid")
        })?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(BackendError::failed(
                "remote_secure_context_required",
                "pairing claim requires an HTTP(S) URL without embedded credentials",
            ));
        }
        let loopback = url
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
        if url.scheme() == "http" && !(allow_insecure_local_dev && loopback) {
            return Err(BackendError::failed(
                "remote_secure_context_required",
                "pairing claim requires HTTPS outside the explicit loopback development exception",
            ));
        }
        url.set_query(None);
        url.set_fragment(None);
        let endpoint = endpoint_url(&url, "/api/v2/pairing/claim")?;
        http_json(
            remote_http_client_for_url(&url)?
                .post(endpoint)
                .json(&request),
        )
        .await
    })
}

/// Claim a short-lived pairing offer through the self-hosted Relay HTTP
/// compatibility bridge. The claim body is encrypted end-to-end with a fresh
/// authenticated Relay session, so the Relay only observes room/correlation
/// metadata and ciphertext.
#[allow(clippy::too_many_arguments)]
pub fn claim_pairing_offer_via_relay(
    relay_url: impl Into<String>,
    room_id: RelayRoomId,
    local_peer_id: RelayPeerId,
    pc_peer_id: RelayPeerId,
    pc_public_key: String,
    request: RemoteClaimPairingOfferRequest,
    identity: ClientDeviceIdentity,
    allow_insecure_local_dev: bool,
) -> BackendFuture<'static, RemoteClaimPairingOfferResponse> {
    let relay_url = relay_url.into();
    Box::pin(async move {
        if request.one_time_challenge.trim().is_empty()
            || request.claim_nonce.trim().is_empty()
            || request.device_identity_public_key != identity.public_key_base64()
            || pc_public_key.trim().is_empty()
            || local_peer_id == pc_peer_id
        {
            return Err(BackendError::failed(
                "remote_pairing_request_invalid",
                "Relay pairing claim identity, challenge, nonce, or route is invalid",
            ));
        }
        let base = validate_relay_pairing_url(&relay_url, allow_insecure_local_dev)?;
        let client = remote_http_client_for_url(&base)?;
        let endpoint = canonical_relay_endpoint(&base)?;
        let relay_keypair =
            RelayKeypair::from_private_key_base64url(&identity.private_key_base64())
                .map_err(BackendError::from)?;
        let relay_ephemeral = RelayKeypair::generate();
        let mut hello = RelayHandshakeHello::new(
            room_id.clone(),
            local_peer_id.clone(),
            relay_keypair.public_key_base64(),
        );
        hello.role = RelayPeerRole::Device;
        hello.endpoint = Some(endpoint.clone());
        hello.capabilities = vec!["relay_pairing_claim".to_string()];
        hello.crypto_suite = Some(RELAY_CRYPTO_SUITE_V2.to_string());
        hello.remote_device_id = Some(identity.device_id().clone());
        hello.remote_device_identity_public_key = Some(identity.public_key_base64());
        hello.ephemeral_public_key = Some(relay_ephemeral.public_key_base64());
        let hello_transcript = relay_handshake_transcript(
            hello.protocol_version,
            &endpoint,
            &room_id,
            None,
            &local_peer_id,
            &relay_keypair.public_key_base64(),
            &relay_ephemeral.public_key_base64(),
            &pc_peer_id,
            &pc_public_key,
            "",
            None,
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(BackendError::from)?;
        hello.ephemeral_proof = Some(
            relay_handshake_authentication_tag(&relay_keypair, &pc_public_key, &hello_transcript)
                .map_err(BackendError::from)?,
        );

        let pair_path = format!("/api/rooms/{}/pair", room_id.as_str());
        let ready_message: RelayControlMessage = http_json(
            client
                .post(endpoint_url(&base, &pair_path)?)
                .json(&RelayControlMessage::Hello(hello)),
        )
        .await?;
        let ready = match ready_message {
            RelayControlMessage::Ready(ready) => ready,
            RelayControlMessage::Error(error) => return Err(map_relay_error(error)),
            _ => {
                return Err(BackendError::failed(
                    "relay_pairing_ready_invalid",
                    "Relay pairing bridge returned an unexpected handshake response",
                ));
            }
        };
        if ready.room_id != room_id
            || ready.peer_id != pc_peer_id
            || ready.public_key != pc_public_key
            || ready.transport_mode != RelayTransportMode::WebSocket
            || ready.crypto_suite.as_deref() != Some(RELAY_CRYPTO_SUITE_V2)
        {
            return Err(BackendError::permission(
                "relay_handshake_identity_mismatch",
                "Relay pairing Ready did not match the pinned PC route",
            ));
        }
        let pc_ephemeral_public_key = ready.ephemeral_public_key.clone().ok_or_else(|| {
            BackendError::permission(
                "relay_ephemeral_key_required",
                "Relay pairing Ready omitted the PC ephemeral key",
            )
        })?;
        let expected_hash = relay_transcript_hash_with_ephemeral(
            ready.protocol_version,
            &endpoint,
            &ready.room_id,
            &ready.session_id,
            &local_peer_id,
            &relay_keypair.public_key_base64(),
            &relay_ephemeral.public_key_base64(),
            &ready.peer_id,
            &ready.public_key,
            &pc_ephemeral_public_key,
            None,
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(BackendError::from)?;
        if ready.transcript_hash.as_deref() != Some(expected_hash.as_str()) {
            return Err(BackendError::permission(
                "relay_handshake_transcript_mismatch",
                "Relay pairing transcript did not match the pinned route",
            ));
        }
        let ready_transcript = relay_handshake_transcript(
            ready.protocol_version,
            &endpoint,
            &ready.room_id,
            Some(&ready.session_id),
            &local_peer_id,
            &relay_keypair.public_key_base64(),
            &relay_ephemeral.public_key_base64(),
            &ready.peer_id,
            &ready.public_key,
            &pc_ephemeral_public_key,
            None,
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(BackendError::from)?;
        verify_relay_handshake_authentication_tag(
            &relay_keypair,
            &ready.public_key,
            &ready_transcript,
            ready.ephemeral_proof.as_deref().ok_or_else(|| {
                BackendError::permission(
                    "relay_ephemeral_proof_required",
                    "Relay pairing Ready omitted the PC authentication proof",
                )
            })?,
        )
        .map_err(BackendError::from)?;
        let mut session = RelaySession::establish_with_ephemeral(
            &relay_ephemeral,
            &pc_ephemeral_public_key,
            RelaySessionConfig {
                room_id: room_id.clone(),
                session_id: ready.session_id,
                local_peer_id,
                remote_peer_id: pc_peer_id,
            },
            Some(&endpoint),
            &relay_keypair.public_key_base64(),
            &ready.public_key,
        )
        .map_err(BackendError::from)?;
        let correlation_id = CorrelationId::new();
        let request_id = request.offer_id.clone();
        let payload = serde_json::to_value(request).map_err(|_| {
            BackendError::failed(
                "remote_pairing_request_encode_failed",
                "Relay pairing claim could not be encoded",
            )
        })?;
        let frame = session
            .seal_json(
                RelayFrameKind::PairRequest,
                Some(correlation_id.clone()),
                payload,
            )
            .map_err(BackendError::from)?;
        let command_path = format!("/api/rooms/{}/command", room_id.as_str());
        let response: RelayControlMessage = http_json(
            client
                .post(endpoint_url(&base, &command_path)?)
                .json(&frame),
        )
        .await?;
        let response = match response {
            RelayControlMessage::Encrypted(frame) => frame,
            RelayControlMessage::Error(error) => return Err(map_relay_error(error)),
            _ => {
                return Err(BackendError::failed(
                    "relay_pairing_response_invalid",
                    "Relay pairing bridge returned an unexpected claim response",
                ));
            }
        };
        if response.kind != RelayFrameKind::PairResponse
            || response.correlation_id.as_ref() != Some(&correlation_id)
        {
            return Err(BackendError::permission(
                "relay_pairing_response_invalid",
                "Relay pairing response metadata did not match the claim",
            ));
        }
        let opened = session.open_json(&response).map_err(BackendError::from)?;
        decode_relay_pairing_claim_response(
            opened.business_payload_json,
            &request_id,
            &correlation_id,
        )
    })
}

fn decode_relay_pairing_claim_response(
    value: JsonValue,
    request_id: &RequestId,
    correlation_id: &CorrelationId,
) -> BackendResult<RemoteClaimPairingOfferResponse> {
    let response: RemoteRpcResponseV2 = serde_json::from_value(value).map_err(|_| {
        BackendError::failed(
            "remote_pairing_claim_response_invalid",
            "Relay pairing response did not contain a valid result envelope",
        )
    })?;
    if &response.request_id != request_id
        || response.correlation_id.as_ref() != Some(correlation_id)
    {
        return Err(BackendError::permission(
            "relay_pairing_response_invalid",
            "Relay pairing response identity did not match the claim",
        ));
    }
    match (response.payload, response.error) {
        (Some(payload), None) => serde_json::from_value(payload).map_err(|_| {
            BackendError::failed(
                "remote_pairing_claim_response_invalid",
                "Relay pairing response did not contain a valid device grant",
            )
        }),
        (None, Some(protocol_error)) => {
            let mut error = BackendError::from(protocol_error.error);
            if error.kind == BackendErrorKind::Offline {
                error.kind = BackendErrorKind::Failed;
            }
            Err(error)
        }
        _ => Err(BackendError::failed(
            "remote_pairing_claim_response_invalid",
            "Relay pairing response must contain exactly one result",
        )),
    }
}

fn validate_relay_pairing_url(
    relay_url: &str,
    allow_insecure_local_dev: bool,
) -> BackendResult<Url> {
    let mut url = Url::parse(relay_url).map_err(|_| {
        BackendError::failed("relay_url_invalid", "self-hosted Relay URL is invalid")
    })?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BackendError::failed(
            "relay_url_invalid",
            "self-hosted Relay URL must not contain credentials, query, or fragment",
        ));
    }
    let secure = matches!(url.scheme(), "https" | "wss");
    let loopback = url
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"));
    if !secure && !(allow_insecure_local_dev && loopback) {
        return Err(BackendError::failed(
            "relay_secure_context_required",
            "self-hosted Relay pairing requires HTTPS outside loopback development",
        ));
    }
    match url.scheme() {
        "ws" => url.set_scheme("http"),
        "wss" => url.set_scheme("https"),
        "http" | "https" => Ok(()),
        _ => Err(()),
    }
    .map_err(|_| BackendError::failed("relay_url_invalid", "Relay URL scheme is invalid"))?;
    Ok(url)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteInboundEvent {
    pub event: RemoteEventV2,
    pub decision: SyncDecision,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoteTransportEvent {
    Event(RemoteInboundEvent),
    Binary(RemoteBinaryFrame),
    Control(RemoteControlMessageV2),
    Closed,
}

pub trait RemoteTransport: BackendBound {
    fn state(&self) -> RemoteConnectionSnapshot;
    fn server_info(&self) -> Option<RemoteServerInfoV2>;
    fn gateway_info(&self) -> Option<RemoteGatewayInfo>;
    fn connect(&self) -> BackendFuture<'_, RemoteServerInfoV2>;
    fn disconnect(&self) -> BackendFuture<'_, ()>;
    fn request(&self, request: RemoteRpcRequestV2) -> BackendFuture<'_, RemoteRpcResponseV2>;
    fn subscribe(
        &self,
        request: RemoteSubscribeRequestV2,
    ) -> BackendFuture<'_, RemoteSubscriptionAcceptedV2>;
    fn attach(
        &self,
        request: RemoteAttachRequestV2,
    ) -> BackendFuture<'_, vibex_core::RemoteAttachmentAcceptedV2>;
    fn detach(&self, attachment_id: String) -> BackendFuture<'_, ()>;
    fn send_binary(&self, frame: RemoteBinaryFrame) -> BackendFuture<'_, ()>;
    fn next_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>>;
    /// Domain/control event stream. Implementations may provide a dedicated
    /// queue so binary consumers cannot steal authoritative domain events.
    fn next_domain_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        self.next_event()
    }
    /// Binary stream. The default keeps compatibility with transports that
    /// expose one multiplexed queue; Direct uses a dedicated bounded queue.
    fn next_binary_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        self.next_event()
    }
    fn next_binary_event_for(
        &self,
        _stream_id: Option<String>,
    ) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        self.next_binary_event()
    }
    fn heartbeat(&self) -> BackendFuture<'_, ()>;
    fn apply_lifecycle_signal(&self, signal: RemoteLifecycleSignal);
    fn cursors(&self) -> Vec<RemoteStreamCursor>;
    fn seed_cursors(&self, _cursors: Vec<RemoteStreamCursor>) {}
    fn clear_unknown_mutation(&self, _request_id: &RequestId) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingMutation {
    idempotency_key: String,
    unknown: bool,
}

#[cfg(not(target_family = "wasm"))]
type Shared<T> = Arc<T>;
#[cfg(target_family = "wasm")]
type Shared<T> = std::rc::Rc<T>;

type AsyncMutex<T> = futures_util::lock::Mutex<T>;

#[cfg(not(target_family = "wasm"))]
struct NativeSocketWriter {
    sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
}

#[cfg(target_family = "wasm")]
struct BrowserSocketWriter {
    socket: web_sys::WebSocket,
    _on_open: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    _on_message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    _on_close: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::CloseEvent)>,
}

#[cfg(not(target_family = "wasm"))]
type SocketWriter = NativeSocketWriter;
#[cfg(target_family = "wasm")]
type SocketWriter = BrowserSocketWriter;

enum TransportSocketWriter {
    Direct(SocketWriter),
    Relay {
        writer: SocketWriter,
        room_id: RelayRoomId,
        local_peer_id: RelayPeerId,
        remote_peer_id: RelayPeerId,
        session: Shared<AsyncMutex<RelaySession>>,
    },
}

struct TransportInner {
    writer: Shared<AsyncMutex<Option<TransportSocketWriter>>>,
    pending: Shared<Mutex<BTreeMap<String, oneshot::Sender<BackendResult<RemoteRpcResponseV2>>>>>,
    control_queue: Shared<Mutex<VecDeque<RemoteControlMessageV2>>>,
    control_waiters: Shared<Mutex<VecDeque<ControlWaiter>>>,
    events: Shared<Mutex<VecDeque<RemoteTransportEvent>>>,
    event_waiters: Shared<Mutex<VecDeque<oneshot::Sender<Option<RemoteTransportEvent>>>>>,
    binary_events: Shared<Mutex<VecDeque<RemoteTransportEvent>>>,
    binary_event_waiters: Shared<Mutex<VecDeque<BinaryEventWaiter>>>,
    state: Shared<Mutex<RemoteConnectionSnapshot>>,
    sync: Shared<Mutex<DomainSyncEngine>>,
    server_info: Shared<Mutex<Option<RemoteServerInfoV2>>>,
    gateway_info: Shared<Mutex<Option<RemoteGatewayInfo>>>,
    lifecycle: Shared<AsyncMutex<()>>,
    pending_mutations: Shared<Mutex<BTreeMap<String, PendingMutation>>>,
    event_queue_capacity: usize,
    binary_queue_capacity: usize,
    connection_generation: AtomicU64,
    control_waiter_generation: AtomicU64,
    reconnect_active: AtomicBool,
}

#[derive(Debug)]
struct ControlWaiter {
    id: u64,
    kind: ControlWaitKind,
    sender: oneshot::Sender<BackendResult<RemoteControlMessageV2>>,
}

#[derive(Debug)]
struct BinaryEventWaiter {
    stream_id: Option<String>,
    sender: oneshot::Sender<Option<RemoteTransportEvent>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlWaitKind {
    Subscribe(String),
    Attach(String),
    Detach(String),
    Heartbeat(u64),
}

impl ControlWaitKind {
    fn matches(&self, control: &RemoteControlMessageV2) -> bool {
        match control {
            RemoteControlMessageV2::Subscribed(accepted) => {
                matches!(self, Self::Subscribe(id) if id == &accepted.subscription_id)
            }
            RemoteControlMessageV2::Attached(accepted) => {
                matches!(self, Self::Attach(id) if id == &accepted.attachment_id)
            }
            RemoteControlMessageV2::Detached(detached) => {
                matches!(self, Self::Detach(id) if id == &detached.attachment_id)
            }
            RemoteControlMessageV2::Pong(pong) => {
                matches!(self, Self::Heartbeat(nonce) if *nonce == pong.nonce)
            }
            // A resync response belongs to whichever control operation is
            // currently waiting, but never to a heartbeat waiter.
            RemoteControlMessageV2::ResyncRequired(_) => !matches!(self, Self::Heartbeat(_)),
            _ => false,
        }
    }
}

impl TransportInner {
    fn new(config: &RemoteClientConfig) -> Self {
        let mut sync = DomainSyncEngine::new(config.event_queue_capacity);
        sync.register_domains([
            "agent_session",
            "sidebar",
            "terminal",
            "file",
            "git",
            "provider",
            "device",
            "runtime",
        ]);
        sync.register_ephemeral_domain("agent_notification");
        Self {
            writer: Shared::new(AsyncMutex::new(None)),
            pending: Shared::new(Mutex::new(BTreeMap::new())),
            control_queue: Shared::new(Mutex::new(VecDeque::new())),
            control_waiters: Shared::new(Mutex::new(VecDeque::new())),
            events: Shared::new(Mutex::new(VecDeque::new())),
            event_waiters: Shared::new(Mutex::new(VecDeque::new())),
            binary_events: Shared::new(Mutex::new(VecDeque::new())),
            binary_event_waiters: Shared::new(Mutex::new(VecDeque::new())),
            state: Shared::new(Mutex::new(RemoteConnectionSnapshot::default())),
            sync: Shared::new(Mutex::new(sync)),
            server_info: Shared::new(Mutex::new(None)),
            gateway_info: Shared::new(Mutex::new(None)),
            lifecycle: Shared::new(AsyncMutex::new(())),
            pending_mutations: Shared::new(Mutex::new(BTreeMap::new())),
            event_queue_capacity: config.event_queue_capacity.max(1),
            binary_queue_capacity: config.binary_queue_capacity.max(1),
            connection_generation: AtomicU64::new(0),
            control_waiter_generation: AtomicU64::new(0),
            reconnect_active: AtomicBool::new(false),
        }
    }
}

struct ControlWaiterGuard {
    inner: Shared<TransportInner>,
    id: u64,
}

impl Drop for ControlWaiterGuard {
    fn drop(&mut self) {
        if let Ok(mut waiters) = self.inner.control_waiters.lock() {
            waiters.retain(|waiter| waiter.id != self.id);
        }
    }
}

struct ReconnectGuard<'a>(&'a AtomicBool);

impl Drop for ReconnectGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug)]
enum WireMessage {
    Text(String),
    Binary(Vec<u8>),
    Closed,
}

#[derive(Clone)]
pub struct DirectWebSocketTransport {
    config: RemoteClientConfig,
    inner: Shared<TransportInner>,
}

impl fmt::Debug for DirectWebSocketTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectWebSocketTransport")
            .field("config", &self.config)
            .field("state", &self.state())
            .finish()
    }
}

impl DirectWebSocketTransport {
    pub fn new(config: RemoteClientConfig) -> BackendResult<Self> {
        config.validate()?;
        Ok(Self {
            inner: Shared::new(TransportInner::new(&config)),
            config,
        })
    }

    pub fn config(&self) -> &RemoteClientConfig {
        &self.config
    }

    /// Deterministic exponential backoff with bounded jitter.  The jitter is
    /// derived from the client id and attempt, so reconnect behavior remains
    /// testable without persisting another secret or introducing an
    /// unbounded timer.
    pub fn backoff_delay(&self, attempt: u32) -> Duration {
        let initial_ms = self.config.reconnect_initial.as_millis();
        let max_ms = self.config.reconnect_max.as_millis();
        let multiplier = 1u128.checked_shl(attempt.min(20)).unwrap_or(u128::MAX);
        let base = initial_ms.saturating_mul(multiplier).min(max_ms);
        let seed = sha2::Sha256::digest(format!("{}:{attempt}", self.config.client_id).as_bytes());
        let spread = u128::from(seed[0] % 41); // 0..40 percent
        let jittered = base
            .saturating_mul(80 + spread)
            .checked_div(100)
            .unwrap_or(base)
            .min(max_ms);
        Duration::from_millis(u64::try_from(jittered).unwrap_or(u64::MAX))
    }

    pub fn reconnect(&self) -> BackendFuture<'_, RemoteServerInfoV2> {
        Box::pin(async move {
            if self.inner.reconnect_active.swap(true, Ordering::AcqRel) {
                return Err(BackendError::loading(
                    "remote_reconnect_in_progress",
                    "a remote reconnect attempt is already in progress",
                ));
            }
            let _guard = ReconnectGuard(&self.inner.reconnect_active);
            if matches!(
                self.state().state,
                RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
            ) {
                return Err(BackendError::permission(
                    "remote_reconnect_forbidden",
                    "remote connection cannot reconnect after revocation or protocol incompatibility",
                ));
            }
            let mut last_error = None;
            for attempt in 0..=self.config.max_reconnect_attempts {
                if attempt > 0 {
                    let delay = self.backoff_delay(attempt - 1);
                    if let Ok(mut state) = self.inner.state.lock() {
                        state.reconnect_attempt = attempt;
                        state.next_retry_at_ms = Some(
                            unix_timestamp_ms()
                                + i64::try_from(delay.as_millis()).unwrap_or(i64::MAX),
                        );
                        state.transition(RemoteConnectionState::Reconnecting);
                    }
                    sleep_for(delay).await;
                }
                match self.connect_inner().await {
                    Ok(info) => return Ok(info),
                    Err(error) => {
                        if let Ok(mut state) = self.inner.state.lock() {
                            state.record_error(&error);
                            if error.code == "remote_device_revoked" {
                                state.transition(RemoteConnectionState::Revoked);
                            } else if error.code.starts_with("remote_protocol") {
                                state.transition(RemoteConnectionState::Incompatible);
                            } else {
                                state.transition(RemoteConnectionState::Reconnecting);
                            }
                        }
                        if matches!(
                            error.code.as_str(),
                            "remote_device_revoked" | "remote_protocol_incompatible"
                        ) || error.code.starts_with("remote_protocol")
                        {
                            return Err(error);
                        }
                        last_error = Some(error);
                    }
                }
            }
            if let Ok(mut state) = self.inner.state.lock() {
                state.transition(RemoteConnectionState::Offline);
            }
            Err(last_error.unwrap_or_else(|| {
                BackendError::offline("remote_reconnect_failed", "remote reconnect failed")
            }))
        })
    }

    pub fn probe(&self) -> BackendFuture<'_, RemoteGatewayInfo> {
        Box::pin(async move {
            match self.probe_inner().await {
                Ok(info) => Ok(info),
                Err(error) => {
                    if let Ok(mut state) = self.inner.state.lock() {
                        state.record_error(&error);
                        if error.code == "remote_protocol_incompatible" {
                            state.transition(RemoteConnectionState::Incompatible);
                        } else {
                            state.transition(RemoteConnectionState::Offline);
                        }
                    }
                    Err(error)
                }
            }
        })
    }

    /// Probe the bounded Direct candidates used by Auto mode.  Each probe is
    /// limited to `/api/v2/info`, carries the paired server identity checks,
    /// and therefore cannot consume a pairing offer or rotate a device grant.
    pub fn probe_direct_candidates(
        &self,
        candidates: Vec<DirectCandidate>,
    ) -> BackendFuture<'_, Vec<CandidateProbeResult>> {
        Box::pin(async move {
            if candidates.is_empty() || candidates.len() > MAX_DIRECT_CANDIDATES {
                return Err(BackendError::failed(
                    "remote_candidate_count_invalid",
                    "Auto mode requires a non-empty bounded Direct candidate list",
                ));
            }
            set_state(&self.inner.state, RemoteConnectionState::Resolving, None);
            let probes = candidates.into_iter().map(|candidate| {
                let mut config = self.config.clone();
                config.base_url = candidate.url.clone();
                config.pinned_tls_certificate_der = candidate.tls_certificate_der.clone();
                async move {
                    // `Instant::now` is unavailable on wasm; measure probe
                    // latency with the host wall clock instead.
                    let started_ms = unix_timestamp_ms();
                    let transport = DirectWebSocketTransport::new(config)?;
                    let info = match timeout_future(Duration::from_secs(5), async move {
                        transport.probe_inner().await
                    })
                    .await
                    {
                        Some(result) => result?,
                        None => {
                            return Err(BackendError::offline(
                                "remote_candidate_probe_timeout",
                                "Direct candidate probe timed out",
                            ));
                        }
                    };
                    Ok(CandidateProbeResult {
                        candidate,
                        latency_ms: u32::try_from((unix_timestamp_ms() - started_ms).max(0))
                            .unwrap_or(u32::MAX),
                        info,
                    })
                }
            });
            let results = futures_util::future::join_all(probes)
                .await
                .into_iter()
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            if results.is_empty() {
                return Err(BackendError::offline(
                    "remote_candidates_unreachable",
                    "no Direct candidate passed the secure protocol probe",
                ));
            }
            Ok(results)
        })
    }

    pub fn select_direct_candidate(
        &self,
        candidates: Vec<DirectCandidate>,
    ) -> BackendFuture<'_, CandidateProbeResult> {
        Box::pin(async move {
            choose_direct_candidate(self.probe_direct_candidates(candidates).await?).ok_or_else(
                || {
                    BackendError::offline(
                        "remote_candidates_unreachable",
                        "no Direct candidate passed the secure protocol probe",
                    )
                },
            )
        })
    }

    pub fn mark_mutation_unknown(&self, request_id: &RequestId) {
        if let Ok(mut pending) = self.inner.pending_mutations.lock()
            && let Some(record) = pending.get_mut(request_id.as_str())
        {
            record.unknown = true;
        }
    }

    pub fn unknown_mutations(&self) -> Vec<String> {
        self.inner
            .pending_mutations
            .lock()
            .map(|pending| {
                pending
                    .values()
                    .filter(|value| value.unknown)
                    .map(|value| value.idempotency_key.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn probe_inner(&self) -> BackendResult<RemoteGatewayInfo> {
        let base = self.config.validate()?;
        let url = endpoint_url(&base, "api/v2/info")?;
        set_state(&self.inner.state, RemoteConnectionState::Probing, None);
        let info: RemoteGatewayInfo =
            http_json(remote_http_client_for_config(&self.config)?.get(url)).await?;
        if self
            .config
            .expected_server_id
            .as_deref()
            .is_some_and(|expected| expected != info.server_id)
        {
            return Err(BackendError::failed(
                "remote_server_identity_mismatch",
                "remote server id does not match the paired server",
            ));
        }
        if let Some(expected) = self.config.expected_server_identity_public_key.as_deref()
            && expected != info.server_identity_public_key
        {
            return Err(BackendError::failed(
                "remote_server_identity_mismatch",
                "remote server identity key does not match the paired server",
            ));
        }
        if info
            .protocol_range
            .negotiate(RemoteProtocolVersionRange::v2())
            .is_none()
        {
            set_state(&self.inner.state, RemoteConnectionState::Incompatible, None);
            return Err(BackendError::unsupported(
                "remote_protocol_incompatible",
                "remote server does not support protocol v2",
            ));
        }
        if let Ok(mut stored) = self.inner.gateway_info.lock() {
            *stored = Some(info.clone());
        }
        Ok(info)
    }

    async fn connect_inner(&self) -> BackendResult<RemoteServerInfoV2> {
        let _guard = self.inner.lifecycle.lock().await;
        match self.state().state {
            RemoteConnectionState::Revoked => {
                return Err(BackendError::permission(
                    "remote_device_revoked",
                    "remote device access has been revoked",
                ));
            }
            RemoteConnectionState::Incompatible => {
                return Err(BackendError::unsupported(
                    "remote_protocol_incompatible",
                    "remote protocol version is incompatible",
                ));
            }
            _ => {}
        }
        if let Some(info) = self.server_info()
            && self.state().state == RemoteConnectionState::Online
        {
            return Ok(info);
        }
        let has_writer = self.inner.writer.lock().await.is_some();
        if has_writer {
            // Replacing a degraded/network-switched socket first resolves all
            // old in-flight RPCs as unknown.  They are never replayed onto the
            // new session implicitly.
            invalidate_connection(&self.inner).await;
        }
        let gateway = self.probe_inner().await?;
        let identity = self.config.device_identity.clone().ok_or_else(|| {
            BackendError::permission(
                "remote_client_identity_required",
                "a paired device identity is required for protocol v2",
            )
        })?;
        if identity.device_id() != &self.config.auth.device_id {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "client identity does not match the authenticated device",
            ));
        }
        set_state(&self.inner.state, RemoteConnectionState::Connecting, None);
        let base = self.config.validate()?;
        let ticket_url = endpoint_url(&base, &gateway.ws_ticket_path)?;
        let ticket: RemoteWsTicketResponse = http_json(
            remote_http_client_for_config(&self.config)?
                .post(ticket_url)
                .json(&RemoteWsTicketRequest {
                    auth: self.config.auth.clone(),
                }),
        )
        .await?;

        let ws_url = websocket_endpoint(&base, &gateway.ws_path)?;
        let client_ephemeral = ephemeral_secret()?;
        let client_ephemeral_public =
            URL_SAFE_NO_PAD.encode(PublicKey::from(&client_ephemeral).as_bytes());
        let last_session_epoch = self.state().session_epoch;
        let cursors = self.cursors();
        let hello = RemoteHello {
            client_id: self.config.client_id.clone(),
            client_type: self.config.client_type,
            app_version: self.config.app_version.clone(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            device_id: identity.device_id().clone(),
            device_identity_public_key: identity.public_key_base64(),
            client_ephemeral_public_key: client_ephemeral_public,
            identity_proof: String::new(),
            relay_auth: None,
            transport_endpoint: None,
            permission_context_hash: None,
            capabilities: vec![
                "rpc".to_string(),
                "server_events".to_string(),
                "binary_terminal".to_string(),
                "binary_file_contract".to_string(),
            ],
            enabled_features: gateway.enabled_features.clone(),
            last_session_epoch,
            cursors,
        };
        let transcript = hello_transcript(
            &hello,
            &ticket.proof_challenge,
            &gateway.server_id,
            gateway.session_epoch,
        )?;
        let server_public = decode_public_key(
            &gateway.server_identity_public_key,
            "remote_server_identity_key_invalid",
        )?;
        let identity_shared = identity.private_secret().diffie_hellman(&server_public);
        if !identity_shared.was_contributory() {
            return Err(BackendError::permission(
                "remote_server_identity_key_invalid",
                "remote server identity key is not contributory",
            ));
        }
        let identity_key = derive_key(
            identity_shared.as_bytes(),
            b"vibex.remote.v2.identity-proof",
            &transcript,
        )?;
        let identity_proof = authentication_tag(&identity_key, &transcript)?;
        let hello = RemoteHello {
            identity_proof: URL_SAFE_NO_PAD.encode(identity_proof),
            ..hello
        };

        let socket_queue_capacity = self
            .config
            .event_queue_capacity
            .saturating_add(self.config.binary_queue_capacity)
            .min(MAX_EVENT_QUEUE_CAPACITY.saturating_add(MAX_BINARY_QUEUE_CAPACITY));
        let (mut writer, mut reader) = open_socket(
            &ws_url,
            &ticket.subprotocol,
            socket_queue_capacity,
            self.config.pinned_tls_certificate_der.as_deref(),
        )
        .await?;
        send_socket_text(
            &mut writer,
            &serde_json::to_string(&RemoteJsonMessageV2::Control(
                RemoteControlMessageV2::Hello(hello.clone()),
            ))
            .map_err(|_| {
                BackendError::failed(
                    "remote_frame_encode_failed",
                    "remote JSON frame could not be encoded",
                )
            })?,
        )
        .await?;
        set_state(
            &self.inner.state,
            RemoteConnectionState::Authenticating,
            None,
        );
        let server_info = next_server_info(&mut reader).await?;
        if server_info.server_id != gateway.server_id
            || server_info.server_identity_public_key != gateway.server_identity_public_key
        {
            return Err(BackendError::permission(
                "remote_server_identity_mismatch",
                "server identity changed during WebSocket handshake",
            ));
        }
        let selected = RemoteProtocolVersionRange::v2()
            .negotiate(server_info.protocol_range)
            .ok_or_else(|| {
                set_state(&self.inner.state, RemoteConnectionState::Incompatible, None);
                BackendError::unsupported(
                    "remote_protocol_incompatible",
                    "remote server and client protocol ranges do not overlap",
                )
            })?;
        if selected != server_info.selected_protocol {
            return Err(BackendError::unsupported(
                "remote_protocol_selection_invalid",
                "remote server selected an unexpected protocol version",
            ));
        }
        verify_session_confirmation(&client_ephemeral, &server_info, &transcript)?;

        if self
            .state()
            .session_epoch
            .is_some_and(|epoch| epoch != server_info.session_epoch)
            && let Ok(mut sync) = self.inner.sync.lock()
        {
            sync.reset_for_session_epoch();
        }

        install_socket(&self.inner, TransportSocketWriter::Direct(writer), reader).await;
        if let Ok(mut stored) = self.inner.server_info.lock() {
            *stored = Some(server_info.clone());
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.session_epoch = Some(server_info.session_epoch);
            state.transition(RemoteConnectionState::Syncing);
        }
        let subscription = self
            .subscribe(RemoteSubscribeRequestV2 {
                subscription_id: format!("subscription_{}", self.config.client_id),
                topics: vec![
                    "agent_session".to_string(),
                    "agent_notification".to_string(),
                    "sidebar".to_string(),
                    "terminal".to_string(),
                    "file".to_string(),
                    "git".to_string(),
                    "provider".to_string(),
                    "device".to_string(),
                    "runtime".to_string(),
                ],
                cursors: self.cursors(),
            })
            .await;
        let subscription = match subscription {
            Ok(subscription) => subscription,
            Err(error) => {
                invalidate_connection(&self.inner).await;
                return Err(error);
            }
        };
        for resync in subscription.resync_required {
            push_event(
                &self.inner,
                RemoteTransportEvent::Control(RemoteControlMessageV2::ResyncRequired(resync)),
            );
        }
        set_state(&self.inner.state, RemoteConnectionState::Online, None);
        Ok(server_info)
    }

    async fn disconnect_inner(&self) -> BackendResult<()> {
        let _guard = self.inner.lifecycle.lock().await;
        invalidate_connection(&self.inner).await;
        Ok(())
    }

    async fn request_inner(
        &self,
        request: RemoteRpcRequestV2,
    ) -> BackendResult<RemoteRpcResponseV2> {
        if self.state().state != RemoteConnectionState::Online {
            match self.state().state {
                RemoteConnectionState::Revoked => {
                    return Err(BackendError::permission(
                        "remote_device_revoked",
                        "remote device access has been revoked",
                    ));
                }
                RemoteConnectionState::Incompatible => {
                    return Err(BackendError::unsupported(
                        "remote_protocol_incompatible",
                        "remote protocol version is incompatible",
                    ));
                }
                _ => {}
            }
            self.connect_inner().await?;
        }
        let request_id = request.request_id.clone();
        let timeout_class = request.timeout_class;
        let key = request_id.as_str().to_string();
        if let Some(mutation) = request.mutation.as_ref()
            && (mutation.idempotency_key.trim().is_empty()
                || mutation.idempotency_key.len() > 128
                || mutation.idempotency_key.chars().any(char::is_control))
        {
            return Err(BackendError::failed(
                "remote_idempotency_key_invalid",
                "remote idempotency key must be non-empty and at most 128 bytes",
            ));
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().map_err(|_| {
                BackendError::failed(
                    "remote_pending_registry_poisoned",
                    "remote request registry is unavailable",
                )
            })?;
            if pending.contains_key(&key) {
                return Err(BackendError::conflict(
                    "remote_request_id_duplicate",
                    "remote request id is already in flight",
                ));
            }
            pending.insert(key.clone(), sender);
        }
        if let Some(mutation) = request.mutation.as_ref() {
            self.inner
                .pending_mutations
                .lock()
                .map_err(|_| {
                    BackendError::failed(
                        "remote_pending_registry_poisoned",
                        "remote request registry is unavailable",
                    )
                })?
                .insert(
                    request_id.as_str().to_string(),
                    PendingMutation {
                        idempotency_key: mutation.idempotency_key.clone(),
                        unknown: false,
                    },
                );
        }
        let send_result = self
            .send_json(RemoteJsonMessageV2::RpcRequest(request))
            .await;
        if let Err(error) = send_result {
            let _ = self
                .inner
                .pending
                .lock()
                .map(|mut pending| pending.remove(&key));
            self.mark_mutation_unknown(&request_id);
            return Err(error);
        }
        let result = timeout_future(timeout_duration(timeout_class), receiver).await;
        match result {
            Some(Ok(result)) => {
                let _ = self
                    .inner
                    .pending_mutations
                    .lock()
                    .map(|mut pending| pending.remove(&key));
                result
            }
            Some(Err(_)) => {
                let _ = self
                    .inner
                    .pending
                    .lock()
                    .map(|mut pending| pending.remove(&key));
                self.mark_mutation_unknown(&request_id);
                Err(BackendError::offline(
                    "remote_rpc_result_unknown",
                    "remote RPC result is unknown after the connection changed",
                )
                .with_recovery_hint("query the mutation result by idempotency key before retrying"))
            }
            None => {
                let _ = self
                    .inner
                    .pending
                    .lock()
                    .map(|mut pending| pending.remove(&key));
                self.mark_mutation_unknown(&request_id);
                Err(BackendError::failed(
                    "remote_rpc_timeout",
                    "remote RPC timed out without changing socket state",
                ))
            }
        }
    }

    async fn send_json(&self, message: RemoteJsonMessageV2) -> BackendResult<()> {
        let mut writer = self.inner.writer.lock().await;
        let Some(writer) = writer.as_mut() else {
            return Err(BackendError::offline(
                "remote_socket_down",
                "remote WebSocket is not connected",
            ));
        };
        send_transport_json(writer, &message).await
    }

    async fn register_control_waiter(
        &self,
        kind: ControlWaitKind,
    ) -> BackendResult<(
        u64,
        oneshot::Receiver<BackendResult<RemoteControlMessageV2>>,
    )> {
        let (sender, receiver) = oneshot::channel();
        let waiter_id = self
            .inner
            .control_waiter_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        {
            let mut queue = self.inner.control_queue.lock().map_err(|_| {
                BackendError::failed(
                    "remote_control_queue_poisoned",
                    "remote control queue is unavailable",
                )
            })?;
            if let Some(index) = queue.iter().position(|control| kind.matches(control)) {
                let _ = sender.send(Ok(queue
                    .remove(index)
                    .expect("control queue index was just found")));
                return Ok((waiter_id, receiver));
            }
            let mut waiters = self.inner.control_waiters.lock().map_err(|_| {
                BackendError::failed(
                    "remote_control_queue_poisoned",
                    "remote control queue is unavailable",
                )
            })?;
            // Recheck the queue while both locks are held.  `dispatch_control`
            // uses the same queue -> waiter order, preventing a response from
            // landing between the first check and waiter registration.
            if let Some(index) = queue.iter().position(|control| kind.matches(control)) {
                let _ = sender.send(Ok(queue
                    .remove(index)
                    .expect("control queue index was just found")));
                return Ok((waiter_id, receiver));
            }
            if self.state().state == RemoteConnectionState::Offline {
                return Err(BackendError::offline(
                    "remote_socket_down",
                    "remote WebSocket is not connected",
                ));
            }
            waiters.push_back(ControlWaiter {
                id: waiter_id,
                kind,
                sender,
            });
        }
        Ok((waiter_id, receiver))
    }

    async fn send_and_wait_control(
        &self,
        kind: ControlWaitKind,
        message: RemoteJsonMessageV2,
    ) -> BackendResult<RemoteControlMessageV2> {
        let (waiter_id, receiver) = self.register_control_waiter(kind).await?;
        let _guard = ControlWaiterGuard {
            inner: self.inner.clone(),
            id: waiter_id,
        };
        self.send_json(message).await?;
        receiver.await.map_err(|_| {
            BackendError::offline(
                "remote_socket_down",
                "remote WebSocket closed while waiting for control",
            )
        })?
    }

    async fn next_event_inner(&self) -> BackendResult<Option<RemoteTransportEvent>> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut events = self.inner.events.lock().map_err(|_| {
                BackendError::failed(
                    "remote_event_queue_poisoned",
                    "remote event queue is unavailable",
                )
            })?;
            if let Some(event) = events.pop_front() {
                return Ok(Some(event));
            }
            let mut waiters = self.inner.event_waiters.lock().map_err(|_| {
                BackendError::failed(
                    "remote_event_queue_poisoned",
                    "remote event queue is unavailable",
                )
            })?;
            // Check the state while holding the waiter lock.  Disconnect
            // drains this same queue after publishing Offline, so a waiter
            // cannot be registered after the disconnect notification.
            if matches!(
                self.state().state,
                RemoteConnectionState::Offline
                    | RemoteConnectionState::Revoked
                    | RemoteConnectionState::Incompatible
            ) {
                return Ok(None);
            }
            waiters.push_back(sender);
        }
        receiver.await.map_err(|_| {
            BackendError::offline(
                "remote_socket_down",
                "remote WebSocket closed while waiting for an event",
            )
        })
    }

    async fn next_binary_event_inner(
        &self,
        stream_id: Option<String>,
    ) -> BackendResult<Option<RemoteTransportEvent>> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut events = self.inner.binary_events.lock().map_err(|_| {
                BackendError::failed(
                    "remote_binary_queue_poisoned",
                    "remote binary event queue is unavailable",
                )
            })?;
            if let Some(index) = events
                .iter()
                .position(|event| binary_event_matches(stream_id.as_deref(), event))
            {
                let event = events
                    .remove(index)
                    .expect("binary event index was just found");
                return Ok(Some(event));
            }
            let mut waiters = self.inner.binary_event_waiters.lock().map_err(|_| {
                BackendError::failed(
                    "remote_binary_queue_poisoned",
                    "remote binary event queue is unavailable",
                )
            })?;
            if matches!(
                self.state().state,
                RemoteConnectionState::Offline
                    | RemoteConnectionState::Revoked
                    | RemoteConnectionState::Incompatible
            ) {
                return Ok(None);
            }
            waiters.push_back(BinaryEventWaiter { stream_id, sender });
        }
        receiver.await.map_err(|_| {
            BackendError::offline(
                "remote_socket_down",
                "remote WebSocket closed while waiting for a binary event",
            )
        })
    }
}

#[derive(Clone)]
pub struct RelayE2eeTransport {
    config: RelayClientConfig,
    inner: Shared<TransportInner>,
}

impl fmt::Debug for RelayE2eeTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayE2eeTransport")
            .field("config", &self.config)
            .field("state", &self.state())
            .finish()
    }
}

impl RelayE2eeTransport {
    pub fn new(config: RelayClientConfig) -> BackendResult<Self> {
        config.validate()?;
        Ok(Self {
            inner: Shared::new(TransportInner::new(&config.remote)),
            config,
        })
    }

    pub fn config(&self) -> &RelayClientConfig {
        &self.config
    }

    pub fn probe(&self) -> BackendFuture<'_, RelayEndpointInfo> {
        Box::pin(async move { self.probe_inner().await })
    }

    pub fn reconnect(&self) -> BackendFuture<'_, RemoteServerInfoV2> {
        Box::pin(async move {
            if self.inner.reconnect_active.swap(true, Ordering::AcqRel) {
                return Err(BackendError::loading(
                    "remote_reconnect_in_progress",
                    "a remote reconnect attempt is already in progress",
                ));
            }
            let _guard = ReconnectGuard(&self.inner.reconnect_active);
            let mut last_error = None;
            for attempt in 0..=self.config.remote.max_reconnect_attempts {
                if attempt > 0 {
                    let delay = relay_backoff_delay(&self.config.remote, attempt - 1);
                    if let Ok(mut state) = self.inner.state.lock() {
                        state.reconnect_attempt = attempt;
                        state.next_retry_at_ms = Some(
                            unix_timestamp_ms()
                                + i64::try_from(delay.as_millis()).unwrap_or(i64::MAX),
                        );
                        state.transition(RemoteConnectionState::Reconnecting);
                    }
                    sleep_for(delay).await;
                }
                match self.connect_inner().await {
                    Ok(info) => return Ok(info),
                    Err(error) => {
                        if matches!(
                            error.code.as_str(),
                            "remote_device_revoked" | "relay_session_revoked"
                        ) {
                            set_state(
                                &self.inner.state,
                                RemoteConnectionState::Revoked,
                                Some(&error),
                            );
                            return Err(error);
                        }
                        if error.code.contains("protocol") {
                            set_state(
                                &self.inner.state,
                                RemoteConnectionState::Incompatible,
                                Some(&error),
                            );
                            return Err(error);
                        }
                        last_error = Some(error);
                    }
                }
            }
            set_state(
                &self.inner.state,
                RemoteConnectionState::Offline,
                last_error.as_ref(),
            );
            Err(last_error.unwrap_or_else(|| {
                BackendError::offline("relay_reconnect_failed", "Relay reconnect failed")
            }))
        })
    }

    async fn probe_inner(&self) -> BackendResult<RelayEndpointInfo> {
        let base = self.config.validate()?;
        let info: RelayEndpointInfo =
            http_json(remote_http_client_for_url(&base)?.get(endpoint_url(&base, "/api/info")?))
                .await?;
        if info.protocol_version != vibex_core::RelayProtocolVersion::foundation() {
            return Err(BackendError::unsupported(
                "relay_protocol_incompatible",
                "self-hosted Relay protocol is incompatible",
            ));
        }
        if !info.features.device_websocket || !info.features.websocket_frames {
            return Err(BackendError::unsupported(
                "relay_websocket_transport_unavailable",
                "self-hosted Relay does not support full-duplex device WebSocket frames",
            ));
        }
        Ok(info)
    }

    async fn connect_inner(&self) -> BackendResult<RemoteServerInfoV2> {
        let _guard = self.inner.lifecycle.lock().await;
        if let Some(info) = self.server_info()
            && self.state().state == RemoteConnectionState::Online
        {
            return Ok(info);
        }
        if self.inner.writer.lock().await.is_some() {
            invalidate_connection(&self.inner).await;
        }
        let relay_info = self.probe_inner().await?;
        let identity = self.config.remote.device_identity.clone().ok_or_else(|| {
            BackendError::permission(
                "remote_client_identity_required",
                "a paired device identity is required for Relay transport",
            )
        })?;
        if identity.device_id() != &self.config.remote.auth.device_id {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "client identity does not match the authenticated device",
            ));
        }
        set_state(&self.inner.state, RemoteConnectionState::Connecting, None);
        let base = self.config.validate()?;
        let endpoint = canonical_relay_endpoint(&base)?;
        let ws_url = websocket_endpoint(&base, "/ws")?;
        let relay_keypair =
            RelayKeypair::from_private_key_base64url(&identity.private_key_base64())
                .map_err(BackendError::from)?;
        let pc_public_key = self.config.pc_public_key.clone().ok_or_else(|| {
            BackendError::permission(
                "relay_pc_identity_required",
                "Relay candidate must pin the PC Relay public key",
            )
        })?;
        let relay_ephemeral = RelayKeypair::generate();
        let mut hello = RelayHandshakeHello::new(
            self.config.room_id.clone(),
            self.config.local_peer_id.clone(),
            relay_keypair.public_key_base64(),
        );
        hello.role = RelayPeerRole::Device;
        hello.endpoint = Some(endpoint.clone());
        hello.capabilities = vec![
            "remote_v2".to_string(),
            "server_events".to_string(),
            "remote_binary".to_string(),
        ];
        hello.crypto_suite = Some(RELAY_CRYPTO_SUITE_V2.to_string());
        hello.remote_device_id = Some(identity.device_id().clone());
        hello.remote_device_identity_public_key = Some(identity.public_key_base64());
        let hello_proof_transcript = relay_handshake_transcript(
            hello.protocol_version,
            &endpoint,
            &hello.room_id,
            None,
            &self.config.local_peer_id,
            &relay_keypair.public_key_base64(),
            &relay_ephemeral.public_key_base64(),
            &self.config.pc_peer_id,
            &pc_public_key,
            "",
            None,
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(BackendError::from)?;
        hello.ephemeral_public_key = Some(relay_ephemeral.public_key_base64());
        hello.ephemeral_proof = Some(
            relay_handshake_authentication_tag(
                &relay_keypair,
                &pc_public_key,
                &hello_proof_transcript,
            )
            .map_err(BackendError::from)?,
        );
        let queue_capacity = self
            .config
            .remote
            .event_queue_capacity
            .saturating_add(self.config.remote.binary_queue_capacity)
            .max(1);
        let (mut socket_writer, mut raw_reader) =
            open_socket(&ws_url, "", queue_capacity, None).await?;
        let registration = serde_json::to_string(&RelayControlMessage::Hello(hello.clone()))
            .map_err(|_| {
                BackendError::failed(
                    "relay_registration_encode_failed",
                    "Relay registration frame could not be encoded",
                )
            })?;
        send_socket_text(&mut socket_writer, &registration).await?;
        let ready_message = next_relay_ready(&mut raw_reader, &self.config).await?;
        let RelayControlMessage::Ready(ready) = ready_message else {
            unreachable!();
        };
        if ready.transport_mode != RelayTransportMode::WebSocket
            || ready.crypto_suite.as_deref() != Some(RELAY_CRYPTO_SUITE_V2)
            || ready.public_key != pc_public_key
        {
            return Err(BackendError::permission(
                "relay_handshake_identity_mismatch",
                "Relay Ready did not match the pinned PC identity and v2 transport suite",
            ));
        }
        let pc_ephemeral_public_key = ready.ephemeral_public_key.clone().ok_or_else(|| {
            BackendError::permission(
                "relay_ephemeral_key_required",
                "Relay Ready omitted the PC ephemeral session key",
            )
        })?;
        let pc_ephemeral_proof = ready.ephemeral_proof.as_deref().ok_or_else(|| {
            BackendError::permission(
                "relay_ephemeral_proof_required",
                "Relay Ready omitted the PC ephemeral authentication proof",
            )
        })?;
        let proof_challenge = ready.proof_challenge.clone().ok_or_else(|| {
            BackendError::failed(
                "relay_remote_handshake_context_missing",
                "Relay Ready omitted the Remote v2 proof challenge",
            )
        })?;
        let session_epoch = ready.remote_session_epoch.ok_or_else(|| {
            BackendError::failed(
                "relay_remote_handshake_context_missing",
                "Relay Ready omitted the Remote v2 session epoch",
            )
        })?;
        let desktop_identity = ready.desktop_identity_public_key.clone().ok_or_else(|| {
            BackendError::failed(
                "relay_remote_handshake_context_missing",
                "Relay Ready omitted the desktop identity",
            )
        })?;
        if self
            .config
            .remote
            .expected_server_identity_public_key
            .as_deref()
            != Some(desktop_identity.as_str())
        {
            return Err(BackendError::permission(
                "remote_server_identity_mismatch",
                "Relay desktop identity does not match the paired server",
            ));
        }
        let permission_context_hash = ready.permission_context_hash.clone().ok_or_else(|| {
            BackendError::failed(
                "relay_remote_handshake_context_missing",
                "Relay Ready omitted the permission context",
            )
        })?;
        let expected_transcript = relay_transcript_hash_with_ephemeral(
            ready.protocol_version,
            &endpoint,
            &ready.room_id,
            &ready.session_id,
            &self.config.local_peer_id,
            &relay_keypair.public_key_base64(),
            &relay_ephemeral.public_key_base64(),
            &ready.peer_id,
            &ready.public_key,
            &pc_ephemeral_public_key,
            Some(&permission_context_hash),
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(BackendError::from)?;
        if ready.transcript_hash.as_deref() != Some(expected_transcript.as_str()) {
            return Err(BackendError::permission(
                "relay_handshake_transcript_mismatch",
                "Relay handshake transcript did not bind endpoint, identities, and permissions",
            ));
        }
        let ready_proof_transcript = relay_handshake_transcript(
            ready.protocol_version,
            &endpoint,
            &ready.room_id,
            Some(&ready.session_id),
            &self.config.local_peer_id,
            &relay_keypair.public_key_base64(),
            &relay_ephemeral.public_key_base64(),
            &ready.peer_id,
            &ready.public_key,
            &pc_ephemeral_public_key,
            Some(&permission_context_hash),
            RelayCryptoSuite::DirectionalV2,
        )
        .map_err(BackendError::from)?;
        verify_relay_handshake_authentication_tag(
            &relay_keypair,
            &ready.public_key,
            &ready_proof_transcript,
            pc_ephemeral_proof,
        )
        .map_err(BackendError::from)?;
        let session = RelaySession::establish_with_ephemeral(
            &relay_ephemeral,
            &pc_ephemeral_public_key,
            RelaySessionConfig {
                room_id: self.config.room_id.clone(),
                session_id: ready.session_id.clone(),
                local_peer_id: self.config.local_peer_id.clone(),
                remote_peer_id: ready.peer_id.clone(),
            },
            Some(&endpoint),
            &relay_keypair.public_key_base64(),
            &ready.public_key,
        )
        .map_err(BackendError::from)?;
        let session = Shared::new(AsyncMutex::new(session));
        let relay_reader = RelayReader {
            inner: raw_reader,
            room_id: self.config.room_id.clone(),
            local_peer_id: self.config.local_peer_id.clone(),
            remote_peer_id: self.config.pc_peer_id.clone(),
            session: session.clone(),
        };
        let client_ephemeral = ephemeral_secret()?;
        let client_ephemeral_public =
            URL_SAFE_NO_PAD.encode(PublicKey::from(&client_ephemeral).as_bytes());
        let mut remote_hello = RemoteHello {
            client_id: self.config.remote.client_id.clone(),
            client_type: self.config.remote.client_type,
            app_version: self.config.remote.app_version.clone(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            device_id: identity.device_id().clone(),
            device_identity_public_key: identity.public_key_base64(),
            client_ephemeral_public_key: client_ephemeral_public,
            identity_proof: String::new(),
            relay_auth: Some(self.config.remote.auth.clone()),
            transport_endpoint: Some(endpoint.clone()),
            permission_context_hash: Some(permission_context_hash),
            capabilities: vec![
                "rpc".to_string(),
                "server_events".to_string(),
                "binary_terminal".to_string(),
                "binary_file_contract".to_string(),
            ],
            enabled_features: Vec::new(),
            last_session_epoch: self.state().session_epoch,
            cursors: self.cursors(),
        };
        let transcript = hello_transcript(
            &remote_hello,
            &proof_challenge,
            self.config
                .remote
                .expected_server_id
                .as_deref()
                .ok_or_else(|| {
                    BackendError::permission(
                        "remote_server_identity_required",
                        "Relay transport requires a pinned desktop server id",
                    )
                })?,
            session_epoch,
        )?;
        let server_public =
            decode_public_key(&desktop_identity, "remote_server_identity_key_invalid")?;
        let identity_shared = identity.private_secret().diffie_hellman(&server_public);
        if !identity_shared.was_contributory() {
            return Err(BackendError::permission(
                "remote_server_identity_key_invalid",
                "desktop identity key is not contributory",
            ));
        }
        let identity_key = derive_key(
            identity_shared.as_bytes(),
            b"vibex.remote.v2.identity-proof",
            &transcript,
        )?;
        remote_hello.identity_proof =
            URL_SAFE_NO_PAD.encode(authentication_tag(&identity_key, &transcript)?);
        let writer = TransportSocketWriter::Relay {
            writer: socket_writer,
            room_id: self.config.room_id.clone(),
            local_peer_id: self.config.local_peer_id.clone(),
            remote_peer_id: self.config.pc_peer_id.clone(),
            session,
        };
        let mut writer = writer;
        send_transport_json(
            &mut writer,
            &RemoteJsonMessageV2::Control(RemoteControlMessageV2::Hello(remote_hello)),
        )
        .await?;
        set_state(
            &self.inner.state,
            RemoteConnectionState::Authenticating,
            None,
        );
        let mut relay_reader = relay_reader;
        let server_info = next_server_info(&mut relay_reader).await?;
        if self.config.remote.expected_server_id.as_deref() != Some(server_info.server_id.as_str())
            || server_info.server_identity_public_key != desktop_identity
        {
            return Err(BackendError::permission(
                "remote_server_identity_mismatch",
                "Remote v2 server identity changed during Relay handshake",
            ));
        }
        verify_session_confirmation(&client_ephemeral, &server_info, &transcript)?;
        if self
            .state()
            .session_epoch
            .is_some_and(|epoch| epoch != server_info.session_epoch)
            && let Ok(mut sync) = self.inner.sync.lock()
        {
            sync.reset_for_session_epoch();
        }
        install_socket(&self.inner, writer, relay_reader).await;
        if let Ok(mut stored) = self.inner.server_info.lock() {
            *stored = Some(server_info.clone());
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.session_epoch = Some(server_info.session_epoch);
            state.transition(RemoteConnectionState::Syncing);
        }
        let subscription = self
            .subscribe(RemoteSubscribeRequestV2 {
                subscription_id: format!("subscription_{}", self.config.remote.client_id),
                topics: vec![
                    "agent_session".to_string(),
                    "agent_notification".to_string(),
                    "sidebar".to_string(),
                    "terminal".to_string(),
                    "file".to_string(),
                    "git".to_string(),
                    "provider".to_string(),
                    "device".to_string(),
                    "runtime".to_string(),
                ],
                cursors: self.cursors(),
            })
            .await?;
        for resync in subscription.resync_required {
            push_event(
                &self.inner,
                RemoteTransportEvent::Control(RemoteControlMessageV2::ResyncRequired(resync)),
            );
        }
        set_state(&self.inner.state, RemoteConnectionState::Online, None);
        let _ = relay_info;
        Ok(server_info)
    }
}

fn relay_backoff_delay(config: &RemoteClientConfig, attempt: u32) -> Duration {
    let initial_ms = config.reconnect_initial.as_millis();
    let max_ms = config.reconnect_max.as_millis();
    let multiplier = 1u128.checked_shl(attempt.min(20)).unwrap_or(u128::MAX);
    Duration::from_millis(
        u64::try_from(initial_ms.saturating_mul(multiplier).min(max_ms)).unwrap_or(u64::MAX),
    )
}

fn canonical_relay_endpoint(base: &Url) -> BackendResult<String> {
    let mut url = base.clone();
    match url.scheme() {
        "ws" => url.set_scheme("http"),
        "wss" => url.set_scheme("https"),
        "http" | "https" => Ok(()),
        _ => Err(()),
    }
    .map_err(|_| BackendError::failed("relay_url_invalid", "Relay URL scheme is invalid"))?;
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

async fn next_relay_ready<R>(
    reader: &mut R,
    config: &RelayClientConfig,
) -> BackendResult<RelayControlMessage>
where
    R: WireReader,
{
    loop {
        match reader.next_wire().await? {
            Some(WireMessage::Text(text)) => {
                if let Ok(peer) = serde_json::from_str::<RelayPeerMessage>(&text) {
                    if peer.room_id != config.room_id
                        || peer.sender_peer_id != config.pc_peer_id
                        || peer.recipient_peer_id != config.local_peer_id
                    {
                        return Err(BackendError::permission(
                            "relay_peer_route_mismatch",
                            "Relay Ready routing metadata did not match the candidate",
                        ));
                    }
                    match peer.message {
                        RelayControlMessage::Ready(ready) => {
                            if ready.room_id != config.room_id || ready.peer_id != config.pc_peer_id
                            {
                                return Err(BackendError::permission(
                                    "relay_handshake_identity_mismatch",
                                    "Relay Ready room or PC peer did not match",
                                ));
                            }
                            return Ok(RelayControlMessage::Ready(ready));
                        }
                        RelayControlMessage::Error(error) => return Err(map_relay_error(error)),
                        _ => continue,
                    }
                }
                if let Ok(RelayControlMessage::Error(error)) =
                    serde_json::from_str::<RelayControlMessage>(&text)
                {
                    return Err(map_relay_error(error));
                }
            }
            Some(WireMessage::Closed) | None => {
                return Err(BackendError::offline(
                    "relay_socket_closed",
                    "Relay WebSocket closed during handshake",
                ));
            }
            Some(WireMessage::Binary(_)) => {
                return Err(BackendError::failed(
                    "relay_handshake_binary_invalid",
                    "Relay sent binary data before Ready",
                ));
            }
        }
    }
}

impl RemoteTransport for DirectWebSocketTransport {
    fn state(&self) -> RemoteConnectionSnapshot {
        self.inner
            .state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| {
                let mut state = RemoteConnectionSnapshot::default();
                state.transition(RemoteConnectionState::Degraded);
                state
            })
    }

    fn server_info(&self) -> Option<RemoteServerInfoV2> {
        self.inner
            .server_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    fn gateway_info(&self) -> Option<RemoteGatewayInfo> {
        self.inner
            .gateway_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    fn connect(&self) -> BackendFuture<'_, RemoteServerInfoV2> {
        Box::pin(async move {
            match self.connect_inner().await {
                Ok(info) => Ok(info),
                Err(error) => {
                    if let Ok(mut state) = self.inner.state.lock() {
                        state.record_error(&error);
                        if error.code == "remote_device_revoked" {
                            state.transition(RemoteConnectionState::Revoked);
                        } else if error.code.starts_with("remote_protocol") {
                            state.transition(RemoteConnectionState::Incompatible);
                        } else {
                            state.transition(RemoteConnectionState::Offline);
                        }
                    }
                    Err(error)
                }
            }
        })
    }

    fn disconnect(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move { self.disconnect_inner().await })
    }

    fn request(&self, request: RemoteRpcRequestV2) -> BackendFuture<'_, RemoteRpcResponseV2> {
        Box::pin(async move { self.request_inner(request).await })
    }

    fn subscribe(
        &self,
        request: RemoteSubscribeRequestV2,
    ) -> BackendFuture<'_, RemoteSubscriptionAcceptedV2> {
        Box::pin(async move {
            let subscription_id = request.subscription_id.clone();
            match self
                .send_and_wait_control(
                    ControlWaitKind::Subscribe(subscription_id),
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::Subscribe(request)),
                )
                .await?
            {
                RemoteControlMessageV2::Subscribed(accepted) => Ok(accepted),
                RemoteControlMessageV2::ResyncRequired(resync) => Err(BackendError::conflict(
                    "remote_resync_required",
                    resync.reason,
                )),
                _ => Err(BackendError::failed(
                    "remote_control_response_invalid",
                    "remote server returned an unexpected subscription response",
                )),
            }
        })
    }

    fn attach(
        &self,
        request: RemoteAttachRequestV2,
    ) -> BackendFuture<'_, RemoteAttachmentAcceptedV2> {
        Box::pin(async move {
            let attachment_id = request.attachment_id.clone();
            match self
                .send_and_wait_control(
                    ControlWaitKind::Attach(attachment_id),
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::Attach(request)),
                )
                .await?
            {
                RemoteControlMessageV2::Attached(accepted) => Ok(accepted),
                RemoteControlMessageV2::ResyncRequired(resync) => Err(BackendError::conflict(
                    "remote_resync_required",
                    resync.reason,
                )),
                _ => Err(BackendError::failed(
                    "remote_control_response_invalid",
                    "remote server returned an unexpected attachment response",
                )),
            }
        })
    }

    fn detach(&self, attachment_id: String) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let waiter_attachment_id = attachment_id.clone();
            match self
                .send_and_wait_control(
                    ControlWaitKind::Detach(waiter_attachment_id),
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::Detach(
                        vibex_core::RemoteDetachRequestV2 { attachment_id },
                    )),
                )
                .await?
            {
                RemoteControlMessageV2::Detached(_) => Ok(()),
                _ => Err(BackendError::failed(
                    "remote_control_response_invalid",
                    "remote server returned an unexpected detach response",
                )),
            }
        })
    }

    fn send_binary(&self, frame: RemoteBinaryFrame) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let encoded = frame.encode().map_err(BackendError::from)?;
            let mut writer = self.inner.writer.lock().await;
            let Some(writer) = writer.as_mut() else {
                return Err(BackendError::offline(
                    "remote_socket_down",
                    "remote WebSocket is not connected",
                ));
            };
            send_transport_binary(writer, encoded).await
        })
    }

    fn next_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move { self.next_event_inner().await })
    }

    fn next_domain_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move { self.next_event_inner().await })
    }

    fn next_binary_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move { self.next_binary_event_inner(None).await })
    }

    fn next_binary_event_for(
        &self,
        stream_id: Option<String>,
    ) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move { self.next_binary_event_inner(stream_id).await })
    }

    fn heartbeat(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let nonce = RequestId::new();
            let ping = RemotePing {
                nonce: nonce.as_str().bytes().fold(0u64, |value, byte| {
                    value.wrapping_mul(31).wrapping_add(u64::from(byte))
                }),
                sent_at_ms: unix_timestamp_ms(),
            };
            let ping_nonce = ping.nonce;
            let transport = self.clone();
            match timeout_future(Duration::from_secs(10), async move {
                transport
                    .send_and_wait_control(
                        ControlWaitKind::Heartbeat(ping_nonce),
                        RemoteJsonMessageV2::Control(RemoteControlMessageV2::Ping(ping)),
                    )
                    .await
            })
            .await
            {
                Some(Ok(RemoteControlMessageV2::Pong(_))) => Ok(()),
                Some(Ok(_)) => Err(BackendError::failed(
                    "remote_heartbeat_response_invalid",
                    "remote heartbeat returned an unexpected control frame",
                )),
                Some(Err(error)) => Err(error),
                None => Err(BackendError::offline(
                    "remote_heartbeat_timeout",
                    "remote heartbeat did not receive a pong",
                )),
            }
        })
    }

    fn apply_lifecycle_signal(&self, signal: RemoteLifecycleSignal) {
        let mut should_reconnect = false;
        if let Ok(mut state) = self.inner.state.lock() {
            match signal {
                RemoteLifecycleSignal::VisibilityChanged { visible } => {
                    if visible
                        && matches!(
                            state.state,
                            RemoteConnectionState::Offline | RemoteConnectionState::Degraded
                        )
                    {
                        state.transition(RemoteConnectionState::Reconnecting);
                        should_reconnect = true;
                    }
                }
                RemoteLifecycleSignal::AppResumed => {
                    if !matches!(
                        state.state,
                        RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
                    ) {
                        state.transition(RemoteConnectionState::Reconnecting);
                        should_reconnect = true;
                    }
                }
                RemoteLifecycleSignal::NetworkChanged | RemoteLifecycleSignal::ComputerResumed => {
                    if !matches!(
                        state.state,
                        RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
                    ) {
                        state.transition(RemoteConnectionState::Reconnecting);
                        should_reconnect = true;
                    }
                }
                RemoteLifecycleSignal::ComputerSuspended
                | RemoteLifecycleSignal::AppBackgrounded => {
                    if state.state == RemoteConnectionState::Online {
                        state.transition(RemoteConnectionState::Degraded);
                    }
                }
            }
        }
        if should_reconnect {
            let transport = self.clone();
            spawn_background(async move {
                let _ = transport.reconnect().await;
            });
        }
    }

    fn cursors(&self) -> Vec<RemoteStreamCursor> {
        self.inner
            .sync
            .lock()
            .map(|sync| sync.cursors())
            .unwrap_or_default()
    }

    fn seed_cursors(&self, cursors: Vec<RemoteStreamCursor>) {
        if let Ok(mut sync) = self.inner.sync.lock() {
            sync.seed_cursors(&cursors);
        }
    }

    fn clear_unknown_mutation(&self, request_id: &RequestId) {
        if let Ok(mut pending) = self.inner.pending_mutations.lock() {
            pending.remove(request_id.as_str());
        }
    }
}

impl RemoteTransport for RelayE2eeTransport {
    fn state(&self) -> RemoteConnectionSnapshot {
        self.inner
            .state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| {
                let mut state = RemoteConnectionSnapshot::default();
                state.transition(RemoteConnectionState::Degraded);
                state
            })
    }

    fn server_info(&self) -> Option<RemoteServerInfoV2> {
        self.inner
            .server_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    fn gateway_info(&self) -> Option<RemoteGatewayInfo> {
        self.inner
            .gateway_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    fn connect(&self) -> BackendFuture<'_, RemoteServerInfoV2> {
        Box::pin(async move {
            match self.connect_inner().await {
                Ok(info) => Ok(info),
                Err(error) => {
                    let state = if matches!(
                        error.code.as_str(),
                        "remote_device_revoked" | "relay_session_revoked"
                    ) {
                        RemoteConnectionState::Revoked
                    } else if error.code.contains("protocol") {
                        RemoteConnectionState::Incompatible
                    } else {
                        RemoteConnectionState::Offline
                    };
                    set_state(&self.inner.state, state, Some(&error));
                    Err(error)
                }
            }
        })
    }

    fn disconnect(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let _guard = self.inner.lifecycle.lock().await;
            invalidate_connection(&self.inner).await;
            Ok(())
        })
    }

    fn request(&self, request: RemoteRpcRequestV2) -> BackendFuture<'_, RemoteRpcResponseV2> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move { direct.request_inner(request).await })
    }

    fn subscribe(
        &self,
        request: RemoteSubscribeRequestV2,
    ) -> BackendFuture<'_, RemoteSubscriptionAcceptedV2> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move {
            let subscription_id = request.subscription_id.clone();
            match direct
                .send_and_wait_control(
                    ControlWaitKind::Subscribe(subscription_id),
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::Subscribe(request)),
                )
                .await?
            {
                RemoteControlMessageV2::Subscribed(accepted) => Ok(accepted),
                RemoteControlMessageV2::ResyncRequired(resync) => Err(BackendError::conflict(
                    "remote_resync_required",
                    resync.reason,
                )),
                _ => Err(BackendError::failed(
                    "remote_control_response_invalid",
                    "remote server returned an unexpected subscription response",
                )),
            }
        })
    }

    fn attach(
        &self,
        request: RemoteAttachRequestV2,
    ) -> BackendFuture<'_, RemoteAttachmentAcceptedV2> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move {
            let attachment_id = request.attachment_id.clone();
            match direct
                .send_and_wait_control(
                    ControlWaitKind::Attach(attachment_id),
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::Attach(request)),
                )
                .await?
            {
                RemoteControlMessageV2::Attached(accepted) => Ok(accepted),
                RemoteControlMessageV2::ResyncRequired(resync) => Err(BackendError::conflict(
                    "remote_resync_required",
                    resync.reason,
                )),
                _ => Err(BackendError::failed(
                    "remote_control_response_invalid",
                    "remote server returned an unexpected attachment response",
                )),
            }
        })
    }

    fn detach(&self, attachment_id: String) -> BackendFuture<'_, ()> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move {
            let waiter_id = attachment_id.clone();
            match direct
                .send_and_wait_control(
                    ControlWaitKind::Detach(waiter_id),
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::Detach(
                        vibex_core::RemoteDetachRequestV2 { attachment_id },
                    )),
                )
                .await?
            {
                RemoteControlMessageV2::Detached(_) => Ok(()),
                _ => Err(BackendError::failed(
                    "remote_control_response_invalid",
                    "remote server returned an unexpected detach response",
                )),
            }
        })
    }

    fn send_binary(&self, frame: RemoteBinaryFrame) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let encoded = frame.encode().map_err(BackendError::from)?;
            let mut writer = self.inner.writer.lock().await;
            let Some(writer) = writer.as_mut() else {
                return Err(BackendError::offline(
                    "remote_socket_down",
                    "Relay WebSocket is not connected",
                ));
            };
            send_transport_binary(writer, encoded).await
        })
    }

    fn next_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move { direct.next_event_inner().await })
    }

    fn next_domain_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        self.next_event()
    }

    fn next_binary_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        self.next_binary_event_for(None)
    }

    fn next_binary_event_for(
        &self,
        stream_id: Option<String>,
    ) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move { direct.next_binary_event_inner(stream_id).await })
    }

    fn heartbeat(&self) -> BackendFuture<'_, ()> {
        let direct = DirectWebSocketTransport {
            config: self.config.remote.clone(),
            inner: self.inner.clone(),
        };
        Box::pin(async move {
            let nonce = RequestId::new().as_str().bytes().fold(0u64, |value, byte| {
                value.wrapping_mul(31).wrapping_add(u64::from(byte))
            });
            match timeout_future(Duration::from_secs(10), async move {
                direct
                    .send_and_wait_control(
                        ControlWaitKind::Heartbeat(nonce),
                        RemoteJsonMessageV2::Control(RemoteControlMessageV2::Ping(RemotePing {
                            nonce,
                            sent_at_ms: unix_timestamp_ms(),
                        })),
                    )
                    .await
            })
            .await
            {
                Some(Ok(RemoteControlMessageV2::Pong(_))) => Ok(()),
                Some(Err(error)) => Err(error),
                _ => Err(BackendError::offline(
                    "remote_heartbeat_timeout",
                    "Relay heartbeat did not receive a pong",
                )),
            }
        })
    }

    fn apply_lifecycle_signal(&self, signal: RemoteLifecycleSignal) {
        let should_reconnect = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            match signal {
                RemoteLifecycleSignal::ComputerSuspended
                | RemoteLifecycleSignal::AppBackgrounded
                | RemoteLifecycleSignal::VisibilityChanged { visible: false } => {
                    if state.state == RemoteConnectionState::Online {
                        state.transition(RemoteConnectionState::Degraded);
                    }
                    false
                }
                RemoteLifecycleSignal::NetworkChanged
                | RemoteLifecycleSignal::ComputerResumed
                | RemoteLifecycleSignal::AppResumed
                | RemoteLifecycleSignal::VisibilityChanged { visible: true } => {
                    if matches!(
                        state.state,
                        RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
                    ) {
                        false
                    } else {
                        state.transition(RemoteConnectionState::Reconnecting);
                        true
                    }
                }
            }
        };
        if should_reconnect {
            let transport = self.clone();
            spawn_background(async move {
                let _ = transport.reconnect().await;
            });
        }
    }

    fn cursors(&self) -> Vec<RemoteStreamCursor> {
        self.inner
            .sync
            .lock()
            .map(|sync| sync.cursors())
            .unwrap_or_default()
    }

    fn seed_cursors(&self, cursors: Vec<RemoteStreamCursor>) {
        if let Ok(mut sync) = self.inner.sync.lock() {
            sync.seed_cursors(&cursors);
        }
    }

    fn clear_unknown_mutation(&self, request_id: &RequestId) {
        if let Ok(mut pending) = self.inner.pending_mutations.lock() {
            pending.remove(request_id.as_str());
        }
    }
}

fn set_state(
    cell: &Shared<Mutex<RemoteConnectionSnapshot>>,
    state: RemoteConnectionState,
    error: Option<&BackendError>,
) {
    if let Ok(mut current) = cell.lock() {
        if let Some(error) = error {
            current.record_error(error);
        }
        current.transition(state);
    }
}

fn endpoint_url(base: &Url, path: &str) -> BackendResult<Url> {
    let mut url = base.clone();
    match url.scheme() {
        "ws" => url.set_scheme("http").map_err(|_| {
            BackendError::failed("remote_url_invalid", "remote HTTP URL is invalid")
        })?,
        "wss" => url.set_scheme("https").map_err(|_| {
            BackendError::failed("remote_url_invalid", "remote HTTPS URL is invalid")
        })?,
        _ => {}
    }
    if path.starts_with('/') {
        url.set_path(path);
    } else {
        let base_path = url.path().trim_end_matches('/');
        let path = path.trim_matches('/');
        url.set_path(&format!("{base_path}/{path}"));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn websocket_endpoint(base: &Url, path: &str) -> BackendResult<Url> {
    let mut url = endpoint_url(base, path)?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        "wss" => "wss",
        "ws" => "ws",
        _ => {
            return Err(BackendError::failed(
                "remote_url_invalid",
                "remote URL scheme is invalid",
            ));
        }
    };
    url.set_scheme(scheme).map_err(|_| {
        BackendError::failed("remote_url_invalid", "remote WebSocket URL is invalid")
    })?;
    Ok(url)
}

pub(crate) async fn http_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> BackendResult<T> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| {
            #[cfg(not(target_family = "wasm"))]
            let is_connect = error.is_connect();
            #[cfg(target_family = "wasm")]
            let is_connect = false;
            let code = if error.is_timeout() {
                "remote_http_timeout"
            } else if cfg!(target_family = "wasm") {
                "remote_browser_network_policy"
            } else if is_connect {
                #[cfg(target_family = "wasm")]
                {
                    "remote_browser_network_policy"
                }
                #[cfg(not(target_family = "wasm"))]
                {
                    "remote_http_unreachable"
                }
            } else {
                "remote_http_request_failed"
            };
            BackendError::offline(code, "remote HTTP request could not be completed")
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = bounded_response_bytes(response)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        return Err(map_http_error(status.as_u16(), body));
    }
    let bytes = bounded_response_bytes(response).await.map_err(|_| {
        BackendError::failed(
            "remote_http_payload_too_large",
            "remote HTTP response exceeded the bounded JSON payload limit",
        )
    })?;
    serde_json::from_slice::<T>(&bytes).map_err(|_| {
        BackendError::failed(
            "remote_http_payload_invalid",
            "remote HTTP response payload is invalid",
        )
    })
}

async fn bounded_response_bytes(response: reqwest::Response) -> Result<Vec<u8>, ()> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_JSON_BYTES as u64)
    {
        return Err(());
    }

    let mut bytes = Vec::new();
    #[cfg(not(target_family = "wasm"))]
    {
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_HTTP_JSON_BYTES {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    #[cfg(target_family = "wasm")]
    {
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ())?;
            if bytes.len().saturating_add(chunk.len()) > MAX_HTTP_JSON_BYTES {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    Ok(bytes)
}

fn map_http_error(status: u16, body: Option<JsonValue>) -> BackendError {
    let response_kind = if status == 401 || status == 403 {
        BackendErrorKind::Permission
    } else if status == 409 {
        BackendErrorKind::Conflict
    } else {
        BackendErrorKind::Failed
    };
    if let Some(error) = body
        .as_ref()
        .and_then(|body| body.get("error"))
        .and_then(|error| serde_json::from_value::<VibexError>(error.clone()).ok())
    {
        let mut error = BackendError::from(error);
        if error.kind == BackendErrorKind::Offline && status < 500 {
            error.kind = response_kind;
        }
        return error;
    }
    BackendError::new(
        response_kind,
        format!("remote_http_status_{status}"),
        "remote HTTP request was rejected",
    )
}

fn decode_public_key(value: &str, code: &'static str) -> BackendResult<PublicKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| BackendError::failed(code, "remote X25519 public key is invalid"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| BackendError::failed(code, "remote X25519 public key length is invalid"))?;
    Ok(PublicKey::from(bytes))
}

fn ephemeral_secret() -> BackendResult<StaticSecret> {
    #[cfg(not(target_family = "wasm"))]
    {
        Ok(StaticSecret::random_from_rng(rand_core::OsRng))
    }
    #[cfg(target_family = "wasm")]
    {
        let mut bytes = [0u8; 32];
        let crypto = web_sys::window()
            .and_then(|window| window.crypto().ok())
            .ok_or_else(|| {
                BackendError::failed(
                    "remote_ephemeral_randomness_unavailable",
                    "browser crypto is unavailable",
                )
            })?;
        crypto
            .get_random_values_with_u8_array(&mut bytes)
            .map_err(|_| {
                BackendError::failed(
                    "remote_ephemeral_randomness_failed",
                    "browser crypto failed",
                )
            })?;
        Ok(StaticSecret::from(bytes))
    }
}

fn hello_transcript(
    hello: &RemoteHello,
    proof_challenge: &str,
    server_id: &str,
    session_epoch: u64,
) -> BackendResult<Vec<u8>> {
    vibex_core::canonical_json_vec(&serde_json::json!({
        "protocol": "vibex.remote.v2",
        "proofChallenge": proof_challenge,
        "serverId": server_id,
        "sessionEpoch": session_epoch,
        "clientId": hello.client_id,
        "clientType": hello.client_type,
        "appVersion": hello.app_version,
        "protocolRange": hello.protocol_range,
        "deviceId": hello.device_id,
        "deviceIdentityPublicKey": hello.device_identity_public_key,
        "clientEphemeralPublicKey": hello.client_ephemeral_public_key,
        "relayMode": hello.relay_auth.is_some(),
        "transportEndpoint": hello.transport_endpoint,
        "permissionContextHash": hello.permission_context_hash,
        "capabilities": hello.capabilities,
        "enabledFeatures": hello.enabled_features,
        "lastSessionEpoch": hello.last_session_epoch,
        "cursors": hello.cursors,
    }))
    .map_err(|_| {
        BackendError::failed(
            "remote_hello_transcript_invalid",
            "remote hello transcript could not be encoded",
        )
    })
}

fn derive_key(shared_secret: &[u8], label: &[u8], transcript: &[u8]) -> BackendResult<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(Some(label), shared_secret);
    let mut key = [0u8; 32];
    hkdf.expand(transcript, &mut key).map_err(|_| {
        BackendError::failed(
            "remote_session_key_derivation_failed",
            "remote session key derivation failed",
        )
    })?;
    Ok(key)
}

fn authentication_tag(key: &[u8], message: &[u8]) -> BackendResult<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| {
        BackendError::failed(
            "remote_identity_proof_setup_failed",
            "remote identity proof setup failed",
        )
    })?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn verify_session_confirmation(
    client_ephemeral: &StaticSecret,
    server_info: &RemoteServerInfoV2,
    transcript: &[u8],
) -> BackendResult<()> {
    let server_ephemeral = decode_public_key(
        &server_info.server_ephemeral_public_key,
        "remote_server_ephemeral_key_invalid",
    )?;
    let shared = client_ephemeral.diffie_hellman(&server_ephemeral);
    if !shared.was_contributory() {
        return Err(BackendError::failed(
            "remote_server_ephemeral_key_invalid",
            "remote server ephemeral key is invalid",
        ));
    }
    let key = derive_key(
        shared.as_bytes(),
        b"vibex.remote.v2.session-key",
        transcript,
    )?;
    let mut message = transcript.to_vec();
    message.extend_from_slice(server_ephemeral.as_bytes());
    let supplied = URL_SAFE_NO_PAD
        .decode(&server_info.session_key_confirmation)
        .map_err(|_| {
            BackendError::failed(
                "remote_session_confirmation_invalid",
                "remote session key confirmation is invalid",
            )
        })?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| {
        BackendError::failed(
            "remote_session_confirmation_invalid",
            "remote session key confirmation is invalid",
        )
    })?;
    mac.update(&message);
    mac.verify_slice(&supplied).map_err(|_| {
        BackendError::permission(
            "remote_session_confirmation_invalid",
            "remote session key confirmation did not match",
        )
    })
}

async fn next_server_info<R>(reader: &mut R) -> BackendResult<RemoteServerInfoV2>
where
    R: WireReader,
{
    loop {
        match reader.next_wire().await? {
            Some(WireMessage::Text(text)) => {
                let message: RemoteJsonMessageV2 = serde_json::from_str(&text).map_err(|_| {
                    BackendError::failed(
                        "remote_frame_invalid",
                        "remote handshake frame is invalid",
                    )
                })?;
                if let RemoteJsonMessageV2::Control(RemoteControlMessageV2::ServerInfo(info)) =
                    message
                {
                    return Ok(info);
                }
            }
            Some(WireMessage::Closed) | None => {
                return Err(BackendError::offline(
                    "remote_socket_closed",
                    "remote WebSocket closed during handshake",
                ));
            }
            Some(WireMessage::Binary(_)) => {
                return Err(BackendError::failed(
                    "remote_handshake_binary_invalid",
                    "remote server sent binary data before server_info",
                ));
            }
        }
    }
}

#[cfg(not(target_family = "wasm"))]
trait WireReader: Send {
    fn next_wire(&mut self) -> BackendFuture<'_, Option<WireMessage>>;
}

#[cfg(target_family = "wasm")]
trait WireReader {
    fn next_wire(&mut self) -> BackendFuture<'_, Option<WireMessage>>;
}

#[cfg(not(target_family = "wasm"))]
struct NativeReader {
    stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
}

#[cfg(not(target_family = "wasm"))]
impl WireReader for NativeReader {
    fn next_wire(&mut self) -> BackendFuture<'_, Option<WireMessage>> {
        Box::pin(async move {
            match self.stream.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    Ok(Some(WireMessage::Text(text.to_string())))
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) => {
                    Ok(Some(WireMessage::Binary(bytes.to_vec())))
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                    Ok(Some(WireMessage::Closed))
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(_)))
                | Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_))) => Ok(None),
                Some(Err(error)) => Err(BackendError::offline(
                    "remote_socket_read_failed",
                    format!("remote WebSocket read failed: {error}"),
                )),
                Some(Ok(tokio_tungstenite::tungstenite::Message::Frame(_))) => Ok(None),
            }
        })
    }
}

#[cfg(target_family = "wasm")]
struct BrowserReader {
    receiver: mpsc::Receiver<WireMessage>,
}

#[cfg(target_family = "wasm")]
impl WireReader for BrowserReader {
    fn next_wire(&mut self) -> BackendFuture<'_, Option<WireMessage>> {
        Box::pin(async move { Ok(self.receiver.next().await) })
    }
}

struct RelayReader<R> {
    inner: R,
    room_id: RelayRoomId,
    local_peer_id: RelayPeerId,
    remote_peer_id: RelayPeerId,
    session: Shared<AsyncMutex<RelaySession>>,
}

impl<R> WireReader for RelayReader<R>
where
    R: WireReader,
{
    fn next_wire(&mut self) -> BackendFuture<'_, Option<WireMessage>> {
        Box::pin(async move {
            loop {
                match self.inner.next_wire().await? {
                    Some(WireMessage::Text(text)) => {
                        if text.is_empty() {
                            continue;
                        }
                        let peer: RelayPeerMessage = serde_json::from_str(&text).map_err(|_| {
                            BackendError::failed(
                                "relay_peer_frame_invalid",
                                "relay peer frame was invalid",
                            )
                        })?;
                        if peer.room_id != self.room_id
                            || peer.sender_peer_id != self.remote_peer_id
                            || peer.recipient_peer_id != self.local_peer_id
                        {
                            return Err(BackendError::permission(
                                "relay_peer_route_mismatch",
                                "relay peer routing metadata did not match the session",
                            ));
                        }
                        match peer.message {
                            RelayControlMessage::Encrypted(frame) => {
                                let plaintext = self
                                    .session
                                    .lock()
                                    .await
                                    .open_json(&frame)
                                    .map_err(BackendError::from)?;
                                if let Some(bytes) =
                                    decode_relay_binary(&plaintext.business_payload_json)?
                                {
                                    return Ok(Some(WireMessage::Binary(bytes)));
                                }
                                let text = serde_json::to_string(&plaintext.business_payload_json)
                                    .map_err(|_| {
                                        BackendError::failed(
                                            "relay_plaintext_invalid",
                                            "relay plaintext could not be decoded",
                                        )
                                    })?;
                                return Ok(Some(WireMessage::Text(text)));
                            }
                            RelayControlMessage::Error(error) => {
                                return Err(map_relay_error(error));
                            }
                            RelayControlMessage::Heartbeat(_)
                            | RelayControlMessage::HeartbeatAck(_)
                            | RelayControlMessage::Hello(_)
                            | RelayControlMessage::Ready(_) => continue,
                        }
                    }
                    Some(WireMessage::Binary(_)) => {
                        return Err(BackendError::failed(
                            "relay_binary_route_invalid",
                            "relay business binary data must remain inside E2EE frames",
                        ));
                    }
                    Some(WireMessage::Closed) | None => {
                        return Ok(Some(WireMessage::Closed));
                    }
                }
            }
        })
    }
}

fn decode_relay_binary(value: &JsonValue) -> BackendResult<Option<Vec<u8>>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.get("encoding").and_then(JsonValue::as_str) != Some("remote_binary_base64url") {
        return Ok(None);
    }
    let encoded = object
        .get("bytes")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            BackendError::failed(
                "relay_binary_envelope_invalid",
                "relay binary envelope was invalid",
            )
        })?;
    URL_SAFE_NO_PAD.decode(encoded).map(Some).map_err(|_| {
        BackendError::failed(
            "relay_binary_envelope_invalid",
            "relay binary envelope was invalid",
        )
    })
}

#[cfg(target_os = "android")]
fn android_websocket_connector() -> BackendResult<tokio_tungstenite::Connector> {
    let mut roots = rustls::RootCertStore::empty();
    let (added, _) =
        roots.add_parsable_certificates(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
    if added == 0 {
        return Err(BackendError::failed(
            "remote_tls_root_certificate_invalid",
            "bundled remote TLS root certificate set is empty",
        ));
    }
    let config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|_| {
        BackendError::failed(
            "remote_tls_config_invalid",
            "remote WebSocket TLS configuration is invalid",
        )
    })?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
}

#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
struct PinnedCertificateVerifier {
    pinned: rustls::pki_types::CertificateDer<'static>,
    roots: rustls::RootCertStore,
    signature_algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

#[cfg(not(target_family = "wasm"))]
impl rustls::client::danger::ServerCertVerifier for PinnedCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if !intermediates.is_empty() || end_entity.as_ref() != self.pinned.as_ref() {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        let certificate = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &certificate,
            &self.roots,
            intermediates,
            now,
            self.signature_algorithms.all,
        )?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.signature_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.signature_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.signature_algorithms.supported_schemes()
    }
}

#[cfg(not(target_family = "wasm"))]
fn pinned_websocket_connector(
    encoded_certificate: &str,
) -> BackendResult<tokio_tungstenite::Connector> {
    let certificate = rustls::pki_types::CertificateDer::from(decode_pinned_tls_certificate(
        encoded_certificate,
    )?);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.clone()).map_err(|_| {
        BackendError::failed(
            "remote_tls_certificate_invalid",
            "pinned local network TLS certificate is invalid",
        )
    })?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let verifier = PinnedCertificateVerifier {
        pinned: certificate,
        roots,
        signature_algorithms: provider.signature_verification_algorithms,
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|_| {
            BackendError::failed(
                "remote_tls_config_invalid",
                "pinned local network WebSocket TLS configuration is invalid",
            )
        })?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
}

#[cfg(not(target_family = "wasm"))]
async fn open_socket(
    url: &Url,
    subprotocol: &str,
    _queue_capacity: usize,
    pinned_tls_certificate_der: Option<&str>,
) -> BackendResult<(NativeSocketWriter, NativeReader)> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::{
        HeaderValue, ORIGIN, SEC_WEBSOCKET_PROTOCOL,
    };
    let mut request = url.as_str().into_client_request().map_err(|_| {
        BackendError::failed(
            "remote_ws_request_invalid",
            "remote WebSocket request is invalid",
        )
    })?;
    let origin = format!(
        "{}://{}{}",
        if url.scheme() == "wss" {
            "https"
        } else {
            "http"
        },
        url.host_str().unwrap_or_default(),
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(&origin).map_err(|_| {
            BackendError::failed(
                "remote_origin_invalid",
                "remote WebSocket origin is invalid",
            )
        })?,
    );
    if !subprotocol.trim().is_empty() {
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(subprotocol).map_err(|_| {
                BackendError::failed(
                    "remote_ws_subprotocol_invalid",
                    "remote WebSocket subprotocol is invalid",
                )
            })?,
        );
    }
    let connector = if let Some(certificate) = pinned_tls_certificate_der {
        Some(pinned_websocket_connector(certificate)?)
    } else {
        #[cfg(target_os = "android")]
        {
            Some(android_websocket_connector()?)
        }
        #[cfg(not(target_os = "android"))]
        {
            None
        }
    };
    let socket_result =
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector).await;
    let (stream, _) = socket_result.map_err(|error| {
        BackendError::offline(
            "remote_ws_connect_failed",
            format!("remote WebSocket connection failed: {error}"),
        )
    })?;
    let (sink, stream) = stream.split();
    Ok((NativeSocketWriter { sink }, NativeReader { stream }))
}

#[cfg(target_family = "wasm")]
async fn open_socket(
    url: &Url,
    subprotocol: &str,
    queue_capacity: usize,
    pinned_tls_certificate_der: Option<&str>,
) -> BackendResult<(BrowserSocketWriter, BrowserReader)> {
    use js_sys::Array;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen::closure::Closure;
    if pinned_tls_certificate_der.is_some() {
        return Err(BackendError::unsupported(
            "remote_pinned_tls_unsupported",
            "pinned local network TLS is unavailable in browser transports",
        ));
    }
    let socket = if subprotocol.trim().is_empty() {
        web_sys::WebSocket::new(url.as_str())
    } else {
        let protocols = Array::new();
        protocols.push(&JsValue::from_str(REMOTE_V2_SUBPROTOCOL));
        let ticket_protocol = subprotocol
            .split(',')
            .map(str::trim)
            .find(|value| value.starts_with(REMOTE_V2_TICKET_PREFIX))
            .ok_or_else(|| {
                BackendError::failed(
                    "remote_ws_ticket_invalid",
                    "WebSocket ticket subprotocol is missing",
                )
            })?;
        protocols.push(&JsValue::from_str(ticket_protocol));
        web_sys::WebSocket::new_with_str_sequence(url.as_str(), &protocols)
    }
    .map_err(|_| {
        BackendError::offline(
            "remote_ws_connect_failed",
            "browser WebSocket connection failed",
        )
    })?;
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
    let (sender, receiver) = mpsc::channel::<WireMessage>(queue_capacity.max(1));
    let mut open_sender = sender.clone();
    let on_open = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let _ = open_sender.try_send(WireMessage::Text(String::new()));
    }) as Box<dyn FnMut(_)>);
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    let mut message_sender = sender.clone();
    let on_message = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        let data = event.data();
        if let Some(text) = data.as_string() {
            let _ = message_sender.try_send(WireMessage::Text(text));
        } else if data.is_instance_of::<js_sys::ArrayBuffer>() {
            let bytes = js_sys::Uint8Array::new(&data).to_vec();
            let _ = message_sender.try_send(WireMessage::Binary(bytes));
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    let mut error_sender = sender.clone();
    let on_error = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let _ = error_sender.try_send(WireMessage::Closed);
    }) as Box<dyn FnMut(_)>);
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    let mut close_sender = sender;
    let on_close = Closure::wrap(Box::new(move |_event: web_sys::CloseEvent| {
        let _ = close_sender.try_send(WireMessage::Closed);
    }) as Box<dyn FnMut(_)>);
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    let mut reader = BrowserReader { receiver };
    loop {
        match reader.next_wire().await? {
            Some(WireMessage::Text(text)) if text.is_empty() => break,
            Some(WireMessage::Closed) | None => {
                return Err(BackendError::offline(
                    "remote_ws_connect_failed",
                    "browser WebSocket closed before opening",
                ));
            }
            _ => {}
        }
    }
    Ok((
        BrowserSocketWriter {
            socket,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        },
        reader,
    ))
}

async fn send_socket_text(writer: &mut SocketWriter, text: &str) -> BackendResult<()> {
    #[cfg(not(target_family = "wasm"))]
    {
        writer
            .sink
            .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
            .await
            .map_err(|error| {
                BackendError::offline(
                    "remote_socket_write_failed",
                    format!("remote WebSocket write failed: {error}"),
                )
            })
    }
    #[cfg(target_family = "wasm")]
    {
        writer.socket.send_with_str(&text).map_err(|_| {
            BackendError::offline(
                "remote_socket_write_failed",
                "browser WebSocket write failed",
            )
        })
    }
}

async fn send_writer_binary(writer: &mut SocketWriter, bytes: Vec<u8>) -> BackendResult<()> {
    #[cfg(not(target_family = "wasm"))]
    {
        writer
            .sink
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                bytes.into(),
            ))
            .await
            .map_err(|error| {
                BackendError::offline(
                    "remote_socket_write_failed",
                    format!("remote WebSocket write failed: {error}"),
                )
            })
    }
    #[cfg(target_family = "wasm")]
    {
        writer.socket.send_with_u8_array(&bytes).map_err(|_| {
            BackendError::offline(
                "remote_socket_write_failed",
                "browser WebSocket binary write failed",
            )
        })
    }
}

async fn send_transport_json(
    writer: &mut TransportSocketWriter,
    message: &RemoteJsonMessageV2,
) -> BackendResult<()> {
    match writer {
        TransportSocketWriter::Direct(writer) => {
            let text = serde_json::to_string(message).map_err(|_| {
                BackendError::failed(
                    "remote_frame_encode_failed",
                    "remote JSON frame could not be encoded",
                )
            })?;
            send_socket_text(writer, &text).await
        }
        TransportSocketWriter::Relay {
            writer,
            room_id,
            local_peer_id,
            remote_peer_id,
            session,
        } => {
            let payload = serde_json::to_value(message).map_err(|_| {
                BackendError::failed(
                    "remote_frame_encode_failed",
                    "remote JSON frame could not be encoded",
                )
            })?;
            let correlation_id = relay_correlation_for_json(message);
            let encrypted = session
                .lock()
                .await
                .seal_json(RELAY_REMOTE_JSON_KIND, correlation_id, payload)
                .map_err(BackendError::from)?;
            let peer = RelayPeerMessage {
                room_id: room_id.clone(),
                sender_peer_id: local_peer_id.clone(),
                recipient_peer_id: remote_peer_id.clone(),
                message: RelayControlMessage::Encrypted(encrypted),
            };
            let text = serde_json::to_string(&peer).map_err(|_| {
                BackendError::failed(
                    "relay_frame_encode_failed",
                    "relay peer frame could not be encoded",
                )
            })?;
            send_socket_text(writer, &text).await
        }
    }
}

async fn send_transport_binary(
    writer: &mut TransportSocketWriter,
    bytes: Vec<u8>,
) -> BackendResult<()> {
    match writer {
        TransportSocketWriter::Direct(writer) => send_writer_binary(writer, bytes).await,
        TransportSocketWriter::Relay {
            writer,
            room_id,
            local_peer_id,
            remote_peer_id,
            session,
        } => {
            let payload = serde_json::json!({
                "encoding": "remote_binary_base64url",
                "bytes": URL_SAFE_NO_PAD.encode(bytes),
            });
            let encrypted = session
                .lock()
                .await
                .seal_json(RelayFrameKind::Event, None, payload)
                .map_err(BackendError::from)?;
            let peer = RelayPeerMessage {
                room_id: room_id.clone(),
                sender_peer_id: local_peer_id.clone(),
                recipient_peer_id: remote_peer_id.clone(),
                message: RelayControlMessage::Encrypted(encrypted),
            };
            let text = serde_json::to_string(&peer).map_err(|_| {
                BackendError::failed(
                    "relay_frame_encode_failed",
                    "relay binary envelope could not be encoded",
                )
            })?;
            send_socket_text(writer, &text).await
        }
    }
}

fn relay_correlation_for_json(message: &RemoteJsonMessageV2) -> Option<CorrelationId> {
    match message {
        RemoteJsonMessageV2::RpcRequest(request) => request
            .correlation_id
            .clone()
            .or_else(|| CorrelationId::parse(request.request_id.as_str()).ok()),
        RemoteJsonMessageV2::RpcResponse(response) => response.correlation_id.clone(),
        RemoteJsonMessageV2::Event(event) => event.correlation_id.clone(),
        RemoteJsonMessageV2::Control(_) | RemoteJsonMessageV2::Unknown => None,
    }
}

fn map_relay_error(error: RelayError) -> BackendError {
    let mut mapped = match error.code {
        RelayErrorCode::UnsupportedProtocol => BackendError::unsupported(
            error.code.as_str(),
            "relay protocol or crypto suite is incompatible",
        ),
        RelayErrorCode::SessionRevoked => {
            BackendError::permission(error.code.as_str(), "relay session was revoked")
        }
        RelayErrorCode::RateLimit
        | RelayErrorCode::BandwidthLimit
        | RelayErrorCode::QueueLimit
        | RelayErrorCode::ConnectionLimit => BackendError::offline(
            error.code.as_str(),
            "self-hosted relay rejected the connection at a configured limit",
        ),
        _ => BackendError::offline(error.code.as_str(), "relay transport rejected a frame"),
    };
    mapped.correlation_id = error.correlation_id;
    mapped
}

fn close_requires_reselection(reason: &vibex_core::RemoteCloseReason) -> bool {
    !matches!(
        reason.code,
        RemoteCloseCode::AuthenticationRequired
            | RemoteCloseCode::DeviceRevoked
            | RemoteCloseCode::UnsupportedVersion
    )
}

fn auto_lifecycle_reselection(
    signal: &RemoteLifecycleSignal,
    state: RemoteConnectionState,
    resumed_from_background: bool,
) -> Option<bool> {
    match signal {
        RemoteLifecycleSignal::NetworkChanged => Some(true),
        RemoteLifecycleSignal::AppResumed
            if resumed_from_background
                && !matches!(
                    state,
                    RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
                ) =>
        {
            Some(true)
        }
        RemoteLifecycleSignal::ComputerResumed
        | RemoteLifecycleSignal::AppResumed
        | RemoteLifecycleSignal::VisibilityChanged { visible: true }
            if !matches!(
                state,
                RemoteConnectionState::Online
                    | RemoteConnectionState::Revoked
                    | RemoteConnectionState::Incompatible
            ) =>
        {
            Some(false)
        }
        _ => None,
    }
}

async fn close_writer(writer: &mut SocketWriter) {
    #[cfg(not(target_family = "wasm"))]
    {
        // A peer that vanished during a network handoff may never complete the
        // WebSocket close handshake. Route selection must not hold its
        // transition lock indefinitely while waiting for that acknowledgement.
        let _ = tokio::time::timeout(Duration::from_secs(1), writer.sink.close()).await;
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = writer.socket.close();
    }
}

async fn close_transport_writer(writer: &mut TransportSocketWriter) {
    match writer {
        TransportSocketWriter::Direct(writer) | TransportSocketWriter::Relay { writer, .. } => {
            close_writer(writer).await;
        }
    }
}

async fn install_socket<R>(
    inner: &Shared<TransportInner>,
    new_writer: TransportSocketWriter,
    reader: R,
) where
    R: WireReader + 'static,
{
    // A new reader gets a monotonically increasing generation.  The writer is
    // installed synchronously before returning so the first subscribe/RPC
    // cannot race a background writer-install task.  Replacing a connection
    // also closes the old writer before its dispatcher can affect the new
    // connection.
    let generation = inner
        .connection_generation
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);
    let mut writer = inner.writer.lock().await;
    if let Some(old_writer) = writer.as_mut() {
        close_transport_writer(old_writer).await;
    }
    *writer = Some(new_writer);
    drop(writer);
    if let Ok(mut controls) = inner.control_queue.lock() {
        controls.clear();
    }
    if let Ok(mut events) = inner.events.lock() {
        events.clear();
    }
    if let Ok(mut events) = inner.binary_events.lock() {
        events.clear();
    }
    spawn_dispatcher(inner.clone(), reader, generation);
}

fn spawn_dispatcher<R>(inner: Shared<TransportInner>, mut reader: R, generation: u64)
where
    R: WireReader + 'static,
{
    spawn_background(async move {
        loop {
            if inner.connection_generation.load(Ordering::Acquire) != generation {
                break;
            }
            match reader.next_wire().await {
                Ok(Some(WireMessage::Text(text))) => dispatch_text(&inner, &text),
                Ok(Some(WireMessage::Binary(bytes))) => dispatch_binary(&inner, &bytes),
                Ok(Some(WireMessage::Closed)) => break,
                Err(error) => {
                    if let Ok(mut state) = inner.state.lock() {
                        if matches!(
                            error.code.as_str(),
                            "remote_device_revoked" | "relay_session_revoked"
                        ) {
                            state.record_error(&error);
                            state.transition(RemoteConnectionState::Revoked);
                        } else if error.code == "remote_protocol_incompatible"
                            || error.code == "relay_unsupported_protocol"
                        {
                            state.record_error(&error);
                            state.transition(RemoteConnectionState::Incompatible);
                        }
                    }
                    break;
                }
                // Native tungstenite uses `Ok(None)` for protocol-level ping /
                // pong frames.  Keep reading instead of treating those as a
                // socket close.
                Ok(None) => {}
            }
        }
        notify_disconnect_generation(&inner, generation).await;
    });
}

fn dispatch_text(inner: &Shared<TransportInner>, text: &str) {
    let Ok(message) = serde_json::from_str::<RemoteJsonMessageV2>(text) else {
        return;
    };
    match message {
        RemoteJsonMessageV2::RpcResponse(response) => {
            let sender = inner
                .pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(response.request_id.as_str()));
            if let Some(sender) = sender {
                let result = if let Some(error) = response.error.clone() {
                    Err(BackendError::from(error.error))
                } else {
                    Ok(response)
                };
                let _ = sender.send(result);
            }
        }
        RemoteJsonMessageV2::Event(event) => {
            let decision = inner
                .sync
                .lock()
                .map(|mut sync| {
                    if is_projection_invalidation_channel(&event.channel) {
                        sync.observe_invalidation(event.clone())
                    } else {
                        sync.observe(event.clone())
                    }
                })
                .unwrap_or(SyncDecision::Resync {
                    domain: event.channel.clone(),
                    generation: event.generation,
                    reason: "sync state unavailable".to_string(),
                    authoritative_operation: event.channel.clone(),
                });
            push_event(
                inner,
                RemoteTransportEvent::Event(RemoteInboundEvent { event, decision }),
            );
        }
        RemoteJsonMessageV2::Control(control) => dispatch_control(inner, control),
        RemoteJsonMessageV2::Unknown | RemoteJsonMessageV2::RpcRequest(_) => {}
    }
}

fn is_projection_invalidation_channel(channel: &str) -> bool {
    matches!(channel, "file" | "git" | "provider" | "device" | "sidebar")
}

fn projection_invalidation_channel(event: &RemoteTransportEvent) -> Option<&str> {
    let RemoteTransportEvent::Event(inbound) = event else {
        return None;
    };
    is_projection_invalidation_channel(&inbound.event.channel)
        .then_some(inbound.event.channel.as_str())
}

fn dispatch_binary(inner: &Shared<TransportInner>, bytes: &[u8]) {
    if let Ok(frame) = RemoteBinaryFrame::decode(bytes) {
        push_event(inner, RemoteTransportEvent::Binary(frame));
    }
}

fn push_event(inner: &Shared<TransportInner>, event: RemoteTransportEvent) {
    match event {
        RemoteTransportEvent::Binary(_) => push_binary_event(inner, event),
        RemoteTransportEvent::Closed => {
            push_domain_event(inner, RemoteTransportEvent::Closed);
            push_binary_event(inner, RemoteTransportEvent::Closed);
        }
        event => push_domain_event(inner, event),
    }
}

fn push_domain_event(inner: &Shared<TransportInner>, event: RemoteTransportEvent) {
    // Keep the same lock order as `next_event_inner`: events, then waiters.
    // This makes queue inspection and waiter registration atomic with respect
    // to an inbound frame.
    if let Ok(mut queue) = inner.events.lock()
        && let Ok(mut waiters) = inner.event_waiters.lock()
    {
        if matches!(
            event,
            RemoteTransportEvent::Control(RemoteControlMessageV2::Close(_))
                | RemoteTransportEvent::Closed
        ) {
            queue.clear();
            if !waiters.is_empty() {
                for waiter in waiters.drain(..) {
                    let _ = waiter.send(Some(event.clone()));
                }
            } else {
                queue.push_back(event);
            }
            return;
        }
        if !waiters.is_empty() {
            for waiter in waiters.drain(..) {
                let _ = waiter.send(Some(event.clone()));
            }
            return;
        }
        if let Some(channel) = projection_invalidation_channel(&event) {
            if let Some(index) = queue
                .iter()
                .position(|queued| projection_invalidation_channel(queued) == Some(channel))
            {
                queue[index] = event;
            } else {
                debug_assert!(
                    queue
                        .iter()
                        .filter_map(projection_invalidation_channel)
                        .count()
                        < PROJECTION_INVALIDATION_CHANNEL_COUNT
                );
                queue.push_back(event);
            }
            return;
        }
        let retained_business_events = queue
            .iter()
            .filter(|queued| projection_invalidation_channel(queued).is_none())
            .count();
        if retained_business_events >= inner.event_queue_capacity {
            // A domain event was received but could not be retained.  The
            // cursor was already observed at the wire boundary, so retain a
            // single explicit resync marker instead of allowing later queued
            // events to look contiguous to a slow consumer. Projection-only
            // invalidations live in coalesced reserve slots and survive this
            // business-stream recovery marker.
            if let RemoteTransportEvent::Event(inbound) = &event {
                if let Ok(mut sync) = inner.sync.lock() {
                    sync.pause_for_resync();
                }
                let projections = queue
                    .iter()
                    .filter(|queued| projection_invalidation_channel(queued).is_some())
                    .cloned()
                    .collect::<Vec<_>>();
                queue.clear();
                queue.push_back(RemoteTransportEvent::Event(RemoteInboundEvent {
                    event: inbound.event.clone(),
                    decision: SyncDecision::Resync {
                        domain: inbound.event.channel.clone(),
                        generation: inbound.event.generation,
                        reason: "bounded domain event queue overflow".to_string(),
                        authoritative_operation: inbound.event.channel.clone(),
                    },
                }));
                queue.extend(projections);
                return;
            }
            if let Some(index) = queue
                .iter()
                .position(|queued| projection_invalidation_channel(queued).is_none())
            {
                queue.remove(index);
            }
        }
        queue.push_back(event);
    }
}

fn push_binary_event(inner: &Shared<TransportInner>, event: RemoteTransportEvent) {
    if let Ok(mut queue) = inner.binary_events.lock()
        && let Ok(mut waiters) = inner.binary_event_waiters.lock()
    {
        if !waiters.is_empty() {
            let mut delivered = false;
            let mut remaining = VecDeque::with_capacity(waiters.len());
            for waiter in waiters.drain(..) {
                if binary_event_matches(waiter.stream_id.as_deref(), &event) {
                    let _ = waiter.sender.send(Some(event.clone()));
                    delivered = true;
                } else {
                    remaining.push_back(waiter);
                }
            }
            *waiters = remaining;
            if delivered {
                return;
            }
        }
        if queue.len() >= inner.binary_queue_capacity {
            queue.pop_front();
        }
        queue.push_back(event);
    }
}

fn binary_event_matches(stream_id: Option<&str>, event: &RemoteTransportEvent) -> bool {
    match (stream_id, event) {
        (_, RemoteTransportEvent::Closed) => true,
        (None, RemoteTransportEvent::Binary(_)) => true,
        (Some(stream_id), RemoteTransportEvent::Binary(frame)) => {
            frame.header.stream_id == stream_id
        }
        _ => false,
    }
}

fn dispatch_control(inner: &Shared<TransportInner>, control: RemoteControlMessageV2) {
    if let RemoteControlMessageV2::Ping(ping) = &control {
        let writer = inner.writer.clone();
        let pong = RemoteJsonMessageV2::Control(RemoteControlMessageV2::Pong(RemotePing {
            nonce: ping.nonce,
            sent_at_ms: unix_timestamp_ms(),
        }));
        spawn_background(async move {
            let mut writer = writer.lock().await;
            if let Some(writer) = writer.as_mut() {
                let _ = send_transport_json(writer, &pong).await;
            }
        });
        return;
    }
    if deliver_control_to_waiter(inner, &control) {
        return;
    }
    // A Pong is meaningful only to a matching heartbeat waiter.  Unsolicited
    // pongs are liveness noise and must not occupy the bounded control queue.
    if matches!(control, RemoteControlMessageV2::Pong(_)) {
        return;
    }
    if let RemoteControlMessageV2::Close(reason) = &control {
        if matches!(
            reason.code,
            vibex_core::RemoteCloseCode::AuthenticationRequired
                | vibex_core::RemoteCloseCode::DeviceRevoked
        ) {
            if let Ok(mut state) = inner.state.lock() {
                state.transition(RemoteConnectionState::Revoked);
            }
        } else if matches!(reason.code, vibex_core::RemoteCloseCode::UnsupportedVersion)
            && let Ok(mut state) = inner.state.lock()
        {
            state.transition(RemoteConnectionState::Incompatible);
        } else if let Ok(mut state) = inner.state.lock() {
            state.transition(RemoteConnectionState::Offline);
        }
    }
    push_domain_event(inner, RemoteTransportEvent::Control(control.clone()));
    if let Ok(mut queue) = inner.control_queue.lock() {
        let capacity = inner.event_queue_capacity.max(16);
        if queue.len() >= capacity {
            queue.pop_front();
        }
        queue.push_back(control);
    }
}

fn deliver_control_to_waiter(
    inner: &Shared<TransportInner>,
    control: &RemoteControlMessageV2,
) -> bool {
    let Ok(_queue_guard) = inner.control_queue.lock() else {
        return false;
    };
    let waiter = inner.control_waiters.lock().ok().and_then(|mut waiters| {
        waiters
            .iter()
            .position(|waiter| waiter.kind.matches(control))
            .map(|index| waiters.remove(index).expect("waiter index was found"))
    });
    if let Some(waiter) = waiter {
        let _ = waiter.sender.send(Ok(control.clone()));
        true
    } else {
        false
    }
}

async fn notify_disconnect_generation(inner: &Shared<TransportInner>, generation: u64) {
    if inner.connection_generation.load(Ordering::Acquire) != generation {
        return;
    }
    let mut writer = inner.writer.lock().await;
    if inner.connection_generation.load(Ordering::Acquire) != generation {
        return;
    }
    if let Some(writer) = writer.as_mut() {
        close_transport_writer(writer).await;
    }
    *writer = None;
    drop(writer);
    mark_disconnected(inner);
}

async fn invalidate_connection(inner: &Shared<TransportInner>) {
    inner.connection_generation.fetch_add(1, Ordering::AcqRel);
    let mut writer = inner.writer.lock().await;
    if let Some(writer) = writer.as_mut() {
        close_transport_writer(writer).await;
    }
    *writer = None;
    drop(writer);
    mark_disconnected(inner);
}

fn mark_disconnected(inner: &Shared<TransportInner>) {
    if let Ok(mut state) = inner.state.lock()
        && !matches!(
            state.state,
            RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
        )
    {
        state.transition(RemoteConnectionState::Offline);
    }
    if let Ok(mut sync) = inner.sync.lock() {
        sync.reset_for_reconnect();
    }
    let pending = inner
        .pending
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default();
    for (request_id, sender) in pending {
        if let Ok(mut mutations) = inner.pending_mutations.lock()
            && let Some(mutation) = mutations.get_mut(&request_id)
        {
            mutation.unknown = true;
        }
        let _ = sender.send(Err(BackendError::offline(
            "remote_rpc_result_unknown",
            "remote RPC result is unknown after the socket closed",
        )));
    }
    if let Ok(mut queue) = inner.control_queue.lock() {
        queue.clear();
        if let Ok(mut waiters) = inner.control_waiters.lock() {
            for waiter in waiters.drain(..) {
                let _ = waiter.sender.send(Err(BackendError::offline(
                    "remote_socket_down",
                    "remote WebSocket closed while waiting for control",
                )));
            }
        }
    }
    if let Ok(mut queue) = inner.events.lock()
        && let Ok(mut waiters) = inner.event_waiters.lock()
    {
        for sender in waiters.drain(..) {
            let _ = sender.send(Some(RemoteTransportEvent::Closed));
        }
        if queue.len() >= inner.event_queue_capacity {
            queue.pop_front();
        }
        queue.push_back(RemoteTransportEvent::Closed);
    }
    if let Ok(mut queue) = inner.binary_events.lock()
        && let Ok(mut waiters) = inner.binary_event_waiters.lock()
    {
        for waiter in waiters.drain(..) {
            let _ = waiter.sender.send(Some(RemoteTransportEvent::Closed));
        }
        if queue.len() >= inner.binary_queue_capacity {
            queue.pop_front();
        }
        queue.push_back(RemoteTransportEvent::Closed);
    }
}

#[cfg(not(target_family = "wasm"))]
fn spawn_background<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(future);
}

async fn sleep_for(duration: Duration) {
    #[cfg(not(target_family = "wasm"))]
    {
        tokio::time::sleep(duration).await;
    }
    #[cfg(target_family = "wasm")]
    {
        use wasm_bindgen::JsCast;
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let callback = wasm_bindgen::closure::Closure::once_into_js(move || {
                let _ = resolve.call0(&wasm_bindgen::JsValue::UNDEFINED);
            });
            let _ = web_sys::window().and_then(|window| {
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        callback.unchecked_ref(),
                        i32::try_from(duration.as_millis()).unwrap_or(i32::MAX),
                    )
                    .ok()
            });
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

fn timeout_duration(class: RemoteTimeoutClass) -> Duration {
    match class {
        RemoteTimeoutClass::Interactive => Duration::from_secs(10),
        RemoteTimeoutClass::Standard | RemoteTimeoutClass::Unknown => Duration::from_secs(30),
        RemoteTimeoutClass::LongRunning => Duration::from_secs(120),
    }
}

#[cfg(not(target_family = "wasm"))]
async fn timeout_future<F, T>(duration: Duration, future: F) -> Option<T>
where
    F: std::future::Future<Output = T> + Send,
{
    tokio::time::timeout(duration, future).await.ok()
}

#[cfg(target_family = "wasm")]
async fn timeout_future<F, T>(duration: Duration, future: F) -> Option<T>
where
    F: std::future::Future<Output = T> + 'static,
{
    use futures_util::future::{Either, select};
    match select(Box::pin(future), Box::pin(sleep_for(duration))).await {
        Either::Left((value, _)) => Some(value),
        Either::Right((_unit, _)) => None,
    }
}

#[cfg(target_family = "wasm")]
fn spawn_background<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

/// Candidate metadata used by Auto mode.  Probing only calls `/api/v2/info`;
/// it never consumes a pairing offer or changes the device grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectCandidate {
    pub url: String,
    pub label: String,
    pub priority: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_certificate_der: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateProbeResult {
    pub candidate: DirectCandidate,
    pub latency_ms: u32,
    pub info: RemoteGatewayInfo,
}

pub fn choose_direct_candidate(
    mut candidates: Vec<CandidateProbeResult>,
) -> Option<CandidateProbeResult> {
    candidates.sort_by_key(|result| (result.latency_ms, result.candidate.priority));
    candidates.into_iter().next()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveRemoteRoute {
    None,
    Direct,
    Relay,
}

#[derive(Clone)]
pub struct AutoRemoteTransport {
    config: AutoRemoteTransportConfig,
    active: Shared<Mutex<ActiveRemoteRoute>>,
    selected: Shared<AsyncMutex<Option<Shared<dyn RemoteTransport>>>>,
    transition: Shared<AsyncMutex<()>>,
    state: Shared<Mutex<RemoteConnectionSnapshot>>,
    last_server_info: Shared<Mutex<Option<RemoteServerInfoV2>>>,
    last_gateway_info: Shared<Mutex<Option<RemoteGatewayInfo>>>,
    cursors: Shared<Mutex<Vec<RemoteStreamCursor>>>,
    reselection_active: Shared<AtomicBool>,
    reselection_enabled: Shared<AtomicBool>,
    reselection_generation: Shared<AtomicU64>,
    app_backgrounded: Shared<AtomicBool>,
}

impl fmt::Debug for AutoRemoteTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AutoRemoteTransport")
            .field("config", &self.config)
            .field("active_route", &self.active_route())
            .field("state", &self.state())
            .finish()
    }
}

impl AutoRemoteTransport {
    pub fn new(config: AutoRemoteTransportConfig) -> BackendResult<Self> {
        config.remote.validate()?;
        if config.direct_candidates.len() > MAX_DIRECT_CANDIDATES
            || (config.direct_candidates.is_empty() && config.relay.is_none())
        {
            return Err(BackendError::failed(
                "remote_candidate_count_invalid",
                "Auto mode requires a bounded Direct and/or self-hosted Relay candidate",
            ));
        }
        if let Some(relay) = &config.relay {
            relay.validate()?;
        }
        Ok(Self {
            config,
            active: Shared::new(Mutex::new(ActiveRemoteRoute::None)),
            selected: Shared::new(AsyncMutex::new(None)),
            transition: Shared::new(AsyncMutex::new(())),
            state: Shared::new(Mutex::new(RemoteConnectionSnapshot::default())),
            last_server_info: Shared::new(Mutex::new(None)),
            last_gateway_info: Shared::new(Mutex::new(None)),
            cursors: Shared::new(Mutex::new(Vec::new())),
            reselection_active: Shared::new(AtomicBool::new(false)),
            reselection_enabled: Shared::new(AtomicBool::new(false)),
            reselection_generation: Shared::new(AtomicU64::new(0)),
            app_backgrounded: Shared::new(AtomicBool::new(false)),
        })
    }

    pub fn active_route(&self) -> ActiveRemoteRoute {
        self.active
            .lock()
            .map(|route| *route)
            .unwrap_or(ActiveRemoteRoute::None)
    }

    pub fn config(&self) -> &AutoRemoteTransportConfig {
        &self.config
    }

    async fn selected(&self) -> BackendResult<Shared<dyn RemoteTransport>> {
        self.selected.lock().await.clone().ok_or_else(|| {
            BackendError::offline(
                "remote_transport_not_selected",
                "Auto mode has not selected a reachable transport",
            )
        })
    }

    async fn selected_for_operation(&self) -> BackendResult<Shared<dyn RemoteTransport>> {
        if let Ok(state) = self.state.lock()
            && matches!(
                state.state,
                RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
            )
        {
            return Err(match state.state {
                RemoteConnectionState::Revoked => BackendError::permission(
                    "remote_device_revoked",
                    "remote device access has been revoked",
                ),
                RemoteConnectionState::Incompatible => BackendError::unsupported(
                    "remote_protocol_incompatible",
                    "remote protocol version is incompatible",
                ),
                _ => unreachable!(),
            });
        }
        if let Some(selected) = self.selected.lock().await.clone() {
            let snapshot = selected.state();
            if snapshot.state == RemoteConnectionState::Online {
                return Ok(selected);
            }
            if snapshot.state == RemoteConnectionState::Revoked {
                return Err(BackendError::permission(
                    "remote_device_revoked",
                    "remote device access has been revoked",
                ));
            }
            if snapshot.state == RemoteConnectionState::Incompatible {
                return Err(BackendError::unsupported(
                    "remote_protocol_incompatible",
                    "remote protocol version is incompatible",
                ));
            }
        }
        let generation = self.reselection_generation.load(Ordering::Acquire);
        self.reselect_with_backoff(false, generation).await?;
        self.selected().await
    }

    async fn reselect_inner(&self, force: bool) -> BackendResult<RemoteServerInfoV2> {
        let _guard = self.transition.lock().await;
        if let Ok(state) = self.state.lock()
            && matches!(
                state.state,
                RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
            )
        {
            return Err(match state.state {
                RemoteConnectionState::Revoked => BackendError::permission(
                    "remote_device_revoked",
                    "remote device access has been revoked",
                ),
                RemoteConnectionState::Incompatible => BackendError::unsupported(
                    "remote_protocol_incompatible",
                    "remote protocol version is incompatible",
                ),
                _ => unreachable!(),
            });
        }
        let selected = { self.selected.lock().await.clone() };
        if let Some(selected) = selected {
            let snapshot = selected.state();
            if !force
                && snapshot.state == RemoteConnectionState::Online
                && let Some(info) = selected.server_info()
            {
                return Ok(info);
            }
            if snapshot.state == RemoteConnectionState::Revoked {
                return Err(BackendError::permission(
                    "remote_device_revoked",
                    "remote device access has been revoked",
                ));
            }
            if snapshot.state == RemoteConnectionState::Incompatible {
                return Err(BackendError::unsupported(
                    "remote_protocol_incompatible",
                    "remote protocol version is incompatible",
                ));
            }
            if let Ok(mut cursors) = self.cursors.lock() {
                *cursors = selected.cursors();
            }
            let _ = selected.disconnect().await;
            *self.selected.lock().await = None;
        }
        if let Ok(mut active) = self.active.lock() {
            *active = ActiveRemoteRoute::None;
        }
        self.connect_route().await
    }

    async fn install_selected(
        &self,
        route: ActiveRemoteRoute,
        transport: Shared<dyn RemoteTransport>,
        info: RemoteServerInfoV2,
    ) -> BackendResult<RemoteServerInfoV2> {
        *self.selected.lock().await = Some(transport.clone());
        if let Ok(mut active) = self.active.lock() {
            *active = route;
        }
        if let Ok(mut cursors) = self.cursors.lock() {
            *cursors = transport.cursors();
        }
        if let Ok(mut server_info) = self.last_server_info.lock() {
            *server_info = Some(info.clone());
        }
        if let Ok(mut gateway_info) = self.last_gateway_info.lock() {
            *gateway_info = transport.gateway_info();
        }
        if let Ok(mut state) = self.state.lock() {
            state.session_epoch = Some(info.session_epoch);
            state.transition(RemoteConnectionState::Online);
        }
        Ok(info)
    }

    async fn connect_route(&self) -> BackendResult<RemoteServerInfoV2> {
        set_state(&self.state, RemoteConnectionState::Probing, None);
        let handoff_cursors = self
            .cursors
            .lock()
            .map(|cursors| cursors.clone())
            .unwrap_or_default();
        if !self.config.direct_candidates.is_empty() {
            let probe = DirectWebSocketTransport::new(self.config.remote.clone())?;
            if let Ok(candidate) = probe
                .select_direct_candidate(self.config.direct_candidates.clone())
                .await
            {
                let mut config = self.config.remote.clone();
                config.base_url = candidate.candidate.url;
                config.pinned_tls_certificate_der = candidate.candidate.tls_certificate_der;
                let transport = Shared::new(DirectWebSocketTransport::new(config)?)
                    as Shared<dyn RemoteTransport>;
                transport.seed_cursors(handoff_cursors.clone());
                match transport.connect().await {
                    Ok(info) => {
                        return self
                            .install_selected(ActiveRemoteRoute::Direct, transport, info)
                            .await;
                    }
                    Err(error)
                        if matches!(
                            error.code.as_str(),
                            "remote_device_revoked" | "remote_protocol_incompatible"
                        ) =>
                    {
                        return Err(error);
                    }
                    Err(_) => {}
                }
            }
        }
        let relay = self.config.relay.clone().ok_or_else(|| {
            BackendError::offline(
                "remote_candidates_unreachable",
                "Direct candidates were unreachable and no self-hosted Relay was configured",
            )
        })?;
        let transport = Shared::new(RelayE2eeTransport::new(relay)?) as Shared<dyn RemoteTransport>;
        transport.seed_cursors(handoff_cursors);
        let info = transport.connect().await?;
        self.install_selected(ActiveRemoteRoute::Relay, transport, info)
            .await
    }

    async fn connect_inner(&self) -> BackendResult<RemoteServerInfoV2> {
        // Initial selection and post-disconnect selection use one path so an
        // offline route is always closed, its cursors are handed off, and only
        // one replacement can be installed.
        self.reselect_inner(false).await
    }

    fn reselection_backoff_delay(&self, attempt: u32) -> Duration {
        let initial_ms = self.config.remote.reconnect_initial.as_millis();
        let max_ms = self.config.remote.reconnect_max.as_millis();
        let multiplier = 1u128.checked_shl(attempt.min(20)).unwrap_or(u128::MAX);
        let base = initial_ms.saturating_mul(multiplier).min(max_ms);
        let seed = Sha256::digest(format!("{}:{attempt}", self.config.remote.client_id).as_bytes());
        let spread = u128::from(seed[0] % 41);
        let jittered = base
            .saturating_mul(80 + spread)
            .checked_div(100)
            .unwrap_or(base)
            .min(max_ms);
        Duration::from_millis(u64::try_from(jittered).unwrap_or(u64::MAX))
    }

    async fn reselect_with_backoff(
        &self,
        force: bool,
        generation: u64,
    ) -> BackendResult<RemoteServerInfoV2> {
        let mut last_error = None;
        for attempt in 0..=self.config.remote.max_reconnect_attempts {
            if !self.reselection_enabled.load(Ordering::Acquire)
                || self.reselection_generation.load(Ordering::Acquire) != generation
            {
                return Err(BackendError::offline(
                    "remote_reselection_canceled",
                    "automatic remote route recovery was canceled",
                ));
            }
            if attempt > 0 {
                let delay = self.reselection_backoff_delay(attempt - 1);
                if let Ok(mut state) = self.state.lock() {
                    state.reconnect_attempt = attempt;
                    state.next_retry_at_ms = Some(
                        unix_timestamp_ms() + i64::try_from(delay.as_millis()).unwrap_or(i64::MAX),
                    );
                    state.transition(RemoteConnectionState::Reconnecting);
                }
                sleep_for(delay).await;
                if !self.reselection_enabled.load(Ordering::Acquire)
                    || self.reselection_generation.load(Ordering::Acquire) != generation
                {
                    return Err(BackendError::offline(
                        "remote_reselection_canceled",
                        "automatic remote route recovery was canceled",
                    ));
                }
            }
            match self.reselect_inner(force && attempt == 0).await {
                Ok(info) => return Ok(info),
                Err(error) => {
                    let terminal = if matches!(
                        error.code.as_str(),
                        "remote_device_revoked" | "relay_session_revoked"
                    ) {
                        Some(RemoteConnectionState::Revoked)
                    } else if error.code.contains("protocol") {
                        Some(RemoteConnectionState::Incompatible)
                    } else {
                        None
                    };
                    if let Ok(mut state) = self.state.lock() {
                        state.record_error(&error);
                        state.transition(terminal.unwrap_or(RemoteConnectionState::Reconnecting));
                    }
                    if terminal.is_some() {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
            }
        }
        let error = last_error.unwrap_or_else(|| {
            BackendError::offline(
                "remote_reselection_failed",
                "automatic remote route recovery failed",
            )
        });
        set_state(&self.state, RemoteConnectionState::Offline, Some(&error));
        Err(error)
    }

    fn schedule_reselection(&self, force: bool) {
        if !self.reselection_enabled.load(Ordering::Acquire)
            || self.reselection_active.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let transport = self.clone();
        let generation = self.reselection_generation.load(Ordering::Acquire);
        spawn_background(async move {
            let _guard = ReconnectGuard(transport.reselection_active.as_ref());
            if let Ok(selected) = transport.selected().await
                && matches!(
                    selected.state().state,
                    RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
                )
            {
                return;
            }
            let _ = transport.reselect_with_backoff(force, generation).await;
        });
    }
}

impl RemoteTransport for AutoRemoteTransport {
    fn state(&self) -> RemoteConnectionSnapshot {
        // The selected transport owns its live connection lifecycle. Keep the
        // Auto wrapper's snapshot for probing/fallback only, so terminal
        // states such as a revoked device are never hidden by a stale Online
        // wrapper state.
        if let Ok(state) = self.state.lock()
            && matches!(
                state.state,
                RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
            )
        {
            return state.clone();
        }
        if let Some(selected) = self.selected.try_lock()
            && let Some(transport) = selected.as_ref()
        {
            return transport.state();
        }
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    fn server_info(&self) -> Option<RemoteServerInfoV2> {
        self.last_server_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    fn gateway_info(&self) -> Option<RemoteGatewayInfo> {
        self.last_gateway_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    fn connect(&self) -> BackendFuture<'_, RemoteServerInfoV2> {
        Box::pin(async move {
            self.reselection_enabled.store(true, Ordering::Release);
            match self.connect_inner().await {
                Ok(info) => Ok(info),
                Err(error) => {
                    set_state(&self.state, RemoteConnectionState::Offline, Some(&error));
                    Err(error)
                }
            }
        })
    }

    fn disconnect(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            self.reselection_enabled.store(false, Ordering::Release);
            self.reselection_generation.fetch_add(1, Ordering::AcqRel);
            let _guard = self.transition.lock().await;
            if let Some(selected) = self.selected.lock().await.take() {
                if let Ok(mut cursors) = self.cursors.lock() {
                    *cursors = selected.cursors();
                }
                selected.disconnect().await?;
            }
            if let Ok(mut active) = self.active.lock() {
                *active = ActiveRemoteRoute::None;
            }
            set_state(&self.state, RemoteConnectionState::Offline, None);
            Ok(())
        })
    }

    fn request(&self, request: RemoteRpcRequestV2) -> BackendFuture<'_, RemoteRpcResponseV2> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.request(request).await;
            if let Err(error) = &result
                && error.kind == BackendErrorKind::Offline
            {
                self.schedule_reselection(false);
            }
            result
        })
    }

    fn subscribe(
        &self,
        request: RemoteSubscribeRequestV2,
    ) -> BackendFuture<'_, RemoteSubscriptionAcceptedV2> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.subscribe(request).await;
            if let Err(error) = &result
                && error.kind == BackendErrorKind::Offline
            {
                self.schedule_reselection(false);
            }
            result
        })
    }

    fn attach(
        &self,
        request: RemoteAttachRequestV2,
    ) -> BackendFuture<'_, RemoteAttachmentAcceptedV2> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.attach(request).await;
            if let Err(error) = &result
                && error.kind == BackendErrorKind::Offline
            {
                self.schedule_reselection(false);
            }
            result
        })
    }

    fn detach(&self, attachment_id: String) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.detach(attachment_id).await;
            if let Err(error) = &result
                && error.kind == BackendErrorKind::Offline
            {
                self.schedule_reselection(false);
            }
            result
        })
    }

    fn send_binary(&self, frame: RemoteBinaryFrame) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.send_binary(frame).await;
            if let Err(error) = &result
                && error.kind == BackendErrorKind::Offline
            {
                self.schedule_reselection(false);
            }
            result
        })
    }

    fn next_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.next_event().await;
            match &result {
                Ok(Some(RemoteTransportEvent::Closed)) | Ok(None) => {
                    self.schedule_reselection(false)
                }
                Ok(Some(RemoteTransportEvent::Control(RemoteControlMessageV2::Close(reason))))
                    if close_requires_reselection(reason) =>
                {
                    self.schedule_reselection(false)
                }
                Ok(Some(RemoteTransportEvent::Control(RemoteControlMessageV2::Close(reason)))) => {
                    let state = match reason.code {
                        RemoteCloseCode::UnsupportedVersion => RemoteConnectionState::Incompatible,
                        RemoteCloseCode::AuthenticationRequired
                        | RemoteCloseCode::DeviceRevoked => RemoteConnectionState::Revoked,
                        _ => RemoteConnectionState::Offline,
                    };
                    set_state(&self.state, state, None);
                }
                Err(error) if error.kind == BackendErrorKind::Offline => {
                    self.schedule_reselection(false)
                }
                _ => {}
            }
            result
        })
    }

    fn next_domain_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.next_domain_event().await;
            match &result {
                Ok(Some(RemoteTransportEvent::Closed)) | Ok(None) => {
                    self.schedule_reselection(false)
                }
                Ok(Some(RemoteTransportEvent::Control(RemoteControlMessageV2::Close(reason))))
                    if close_requires_reselection(reason) =>
                {
                    self.schedule_reselection(false)
                }
                Ok(Some(RemoteTransportEvent::Control(RemoteControlMessageV2::Close(reason)))) => {
                    let state = match reason.code {
                        RemoteCloseCode::UnsupportedVersion => RemoteConnectionState::Incompatible,
                        RemoteCloseCode::AuthenticationRequired
                        | RemoteCloseCode::DeviceRevoked => RemoteConnectionState::Revoked,
                        _ => RemoteConnectionState::Offline,
                    };
                    set_state(&self.state, state, None);
                }
                Err(error) if error.kind == BackendErrorKind::Offline => {
                    self.schedule_reselection(false)
                }
                _ => {}
            }
            result
        })
    }

    fn next_binary_event(&self) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.next_binary_event().await;
            match &result {
                Ok(Some(RemoteTransportEvent::Closed)) | Ok(None) => {
                    self.schedule_reselection(false)
                }
                Err(error) if error.kind == BackendErrorKind::Offline => {
                    self.schedule_reselection(false)
                }
                _ => {}
            }
            result
        })
    }

    fn next_binary_event_for(
        &self,
        stream_id: Option<String>,
    ) -> BackendFuture<'_, Option<RemoteTransportEvent>> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.next_binary_event_for(stream_id).await;
            match &result {
                Ok(Some(RemoteTransportEvent::Closed)) | Ok(None) => {
                    self.schedule_reselection(false)
                }
                Err(error) if error.kind == BackendErrorKind::Offline => {
                    self.schedule_reselection(false)
                }
                _ => {}
            }
            result
        })
    }

    fn heartbeat(&self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let selected = self.selected_for_operation().await?;
            let result = selected.heartbeat().await;
            if let Err(error) = &result
                && error.kind == BackendErrorKind::Offline
            {
                self.schedule_reselection(false);
            }
            result
        })
    }

    fn apply_lifecycle_signal(&self, signal: RemoteLifecycleSignal) {
        let resumed_from_background = match &signal {
            RemoteLifecycleSignal::AppBackgrounded => {
                self.app_backgrounded.store(true, Ordering::Release);
                false
            }
            RemoteLifecycleSignal::AppResumed => {
                self.app_backgrounded.swap(false, Ordering::AcqRel)
            }
            _ => false,
        };
        let is_recovery_signal = matches!(
            &signal,
            RemoteLifecycleSignal::NetworkChanged
                | RemoteLifecycleSignal::ComputerResumed
                | RemoteLifecycleSignal::AppResumed
                | RemoteLifecycleSignal::VisibilityChanged { visible: true }
        );
        if is_recovery_signal {
            if let Some(force) =
                auto_lifecycle_reselection(&signal, self.state().state, resumed_from_background)
            {
                // A real network change probes Direct before Relay. Resume
                // signals only recover a degraded route, so an initial or
                // duplicate visibility event cannot invalidate an in-flight
                // mutation on a healthy socket.
                self.schedule_reselection(force);
            }
            return;
        }
        let transport = self.clone();
        spawn_background(async move {
            if let Ok(selected) = transport.selected().await {
                // Background/suspend signals only degrade the active route;
                // forwarding a reconnecting signal and then replacing the
                // route would start two competing reconnect loops.
                selected.apply_lifecycle_signal(signal);
            }
        });
    }

    fn cursors(&self) -> Vec<RemoteStreamCursor> {
        self.cursors
            .lock()
            .map(|cursors| cursors.clone())
            .unwrap_or_default()
    }

    fn seed_cursors(&self, cursors: Vec<RemoteStreamCursor>) {
        if let Ok(mut stored) = self.cursors.lock() {
            *stored = cursors;
        }
    }

    fn clear_unknown_mutation(&self, request_id: &RequestId) {
        let transport = self.clone();
        let request_id = request_id.clone();
        spawn_background(async move {
            if let Ok(selected) = transport.selected().await {
                selected.clear_unknown_mutation(&request_id);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        DeviceId, ErrorCategory, RemoteProtocolError, RemoteProtocolVersion,
        RemoteRpcResultMetadata,
    };

    fn test_config(url: &str) -> RemoteClientConfig {
        RemoteClientConfig::new(
            url,
            RemoteAuthProof {
                device_id: DeviceId::new(),
                auth_token: "token".to_string(),
            },
        )
    }

    #[test]
    fn remote_client_defaults_to_the_installed_mobile_product_identity() {
        let config = test_config("https://desktop.example");
        assert_eq!(config.client_id, "vibex-mobile");
        assert_eq!(config.client_type, vibex_core::RemoteClientType::Mobile);
    }

    #[test]
    fn secure_context_policy_allows_only_explicit_loopback_exception() {
        let auth = test_config("https://desktop.example").auth;
        let config = RemoteClientConfig::new("http://192.168.1.5:8080", auth.clone());
        assert_eq!(
            config.validate().unwrap_err().code,
            "remote_secure_context_required"
        );
        let mut local = RemoteClientConfig::new("http://localhost:8080", auth);
        assert!(local.validate().is_err());
        local.allow_insecure_local_dev = true;
        assert!(local.validate().is_ok());

        let query = test_config("https://desktop.example?authToken=secret");
        assert_eq!(
            query.validate().unwrap_err().code,
            "remote_url_secret_boundary_invalid"
        );
    }

    #[test]
    fn endpoint_paths_normalize_ws_schemes_and_absolute_gateway_paths() {
        let base = Url::parse("wss://desktop.example/prefix").unwrap();
        assert_eq!(
            endpoint_url(&base, "/api/v2/ws-ticket").unwrap().as_str(),
            "https://desktop.example/api/v2/ws-ticket"
        );
        assert_eq!(
            websocket_endpoint(&base, "/ws/v2").unwrap().as_str(),
            "wss://desktop.example/ws/v2"
        );
    }

    #[test]
    fn rejected_http_pairing_error_does_not_masquerade_as_offline() {
        let body = serde_json::json!({
            "error": VibexError::new(
                ErrorCategory::Remote,
                "remote_pairing_offer_already_claimed",
                "pairing offer has already been claimed",
            )
        });

        let error = map_http_error(400, Some(body));

        assert_eq!(error.kind, BackendErrorKind::Failed);
        assert_eq!(error.code, "remote_pairing_offer_already_claimed");
    }

    #[test]
    fn rejected_relay_pairing_error_does_not_masquerade_as_offline() {
        let request_id = RequestId::new();
        let correlation_id = CorrelationId::new();
        let value = serde_json::to_value(RemoteRpcResponseV2 {
            request_id: request_id.clone(),
            correlation_id: Some(correlation_id.clone()),
            payload: None,
            error: Some(RemoteProtocolError::from_error(VibexError::new(
                ErrorCategory::Remote,
                "remote_pairing_offer_already_claimed",
                "pairing offer has already been claimed",
            ))),
            metadata: RemoteRpcResultMetadata::default(),
            completed_at_ms: unix_timestamp_ms(),
        })
        .unwrap();

        let error =
            decode_relay_pairing_claim_response(value, &request_id, &correlation_id).unwrap_err();

        assert_eq!(error.kind, BackendErrorKind::Failed);
        assert_eq!(error.code, "remote_pairing_offer_already_claimed");
    }

    #[test]
    fn control_waiters_do_not_accept_unrelated_pongs() {
        let pong = RemoteControlMessageV2::Pong(RemotePing {
            nonce: 7,
            sent_at_ms: 0,
        });
        assert!(!ControlWaitKind::Subscribe("subscription".to_string()).matches(&pong));
        assert!(!ControlWaitKind::Heartbeat(8).matches(&pong));
        assert!(ControlWaitKind::Heartbeat(7).matches(&pong));
    }

    #[test]
    fn inbound_events_fan_out_to_all_waiting_consumers() {
        let config = test_config("https://desktop.example");
        let inner = Shared::new(TransportInner::new(&config));
        let (first_sender, first_receiver) = oneshot::channel();
        let (second_sender, second_receiver) = oneshot::channel();
        inner
            .event_waiters
            .lock()
            .unwrap()
            .extend([first_sender, second_sender]);
        push_event(&inner, RemoteTransportEvent::Closed);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert!(matches!(
            runtime.block_on(first_receiver).unwrap(),
            Some(RemoteTransportEvent::Closed)
        ));
        assert!(matches!(
            runtime.block_on(second_receiver).unwrap(),
            Some(RemoteTransportEvent::Closed)
        ));
    }

    #[test]
    fn binary_queue_selects_the_requested_stream_without_stealing_another() {
        let config = test_config("https://desktop.example");
        let transport = DirectWebSocketTransport::new(config).unwrap();
        let first = RemoteBinaryFrame {
            header: vibex_core::RemoteBinaryFrameHeader {
                protocol_version: RemoteProtocolVersionRange::v2().max,
                kind: vibex_core::RemoteBinaryFrameKind::TerminalOutput,
                stream_id: "terminal-a".to_string(),
                request_id: None,
                generation: 1,
                sequence: 1,
                offset: 0,
                total_size: None,
                snapshot: false,
                end_of_stream: false,
                checksum_sha256: None,
                payload_length: 1,
            },
            payload: vec![b'a'],
        };
        let second = RemoteBinaryFrame {
            header: vibex_core::RemoteBinaryFrameHeader {
                stream_id: "terminal-b".to_string(),
                sequence: 1,
                ..first.header.clone()
            },
            payload: vec![b'b'],
        };
        push_event(
            &transport.inner,
            RemoteTransportEvent::Binary(first.clone()),
        );
        push_event(
            &transport.inner,
            RemoteTransportEvent::Binary(second.clone()),
        );

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let selected = runtime
            .block_on(transport.next_binary_event_for(Some("terminal-b".to_string())))
            .unwrap();
        assert!(matches!(
            selected,
            Some(RemoteTransportEvent::Binary(frame)) if frame.header.stream_id == "terminal-b"
        ));
        let remaining = runtime
            .block_on(transport.next_binary_event_for(Some("terminal-a".to_string())))
            .unwrap();
        assert!(matches!(
            remaining,
            Some(RemoteTransportEvent::Binary(frame)) if frame.header.stream_id == "terminal-a"
        ));
    }

    #[test]
    fn failed_control_send_removes_waiter() {
        let config = test_config("https://desktop.example");
        let transport = DirectWebSocketTransport::new(config).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(transport.send_and_wait_control(
            ControlWaitKind::Heartbeat(42),
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Ping(RemotePing {
                nonce: 42,
                sent_at_ms: 0,
            })),
        ));
        assert_eq!(result.unwrap_err().code, "remote_socket_down");
        assert!(transport.inner.control_waiters.lock().unwrap().is_empty());
    }

    #[test]
    fn domain_queue_overflow_publishes_resync_instead_of_a_false_contiguous_event() {
        let mut config = test_config("https://desktop.example");
        config.event_queue_capacity = 1;
        let transport = DirectWebSocketTransport::new(config).unwrap();
        let make_event = |sequence| {
            RemoteTransportEvent::Event(RemoteInboundEvent {
                event: RemoteEventV2 {
                    event_id: vibex_core::EventId::new(),
                    channel: "agent_session".to_string(),
                    generation: 1,
                    sequence,
                    correlation_id: None,
                    payload: None,
                    emitted_at_ms: 0,
                },
                decision: SyncDecision::Apply,
            })
        };
        push_event(&transport.inner, make_event(1));
        push_event(&transport.inner, make_event(2));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let event = runtime
            .block_on(transport.next_domain_event())
            .unwrap()
            .expect("resync marker");
        assert!(matches!(
            event,
            RemoteTransportEvent::Event(RemoteInboundEvent {
                decision: SyncDecision::Resync { .. },
                ..
            })
        ));
    }

    #[test]
    fn projection_invalidation_burst_coalesces_without_pausing_agent_events() {
        let mut config = test_config("https://desktop.example");
        config.event_queue_capacity = 1;
        let transport = DirectWebSocketTransport::new(config).unwrap();
        let dispatch = |channel: &str, sequence: u64| {
            let message = RemoteJsonMessageV2::Event(RemoteEventV2 {
                event_id: vibex_core::EventId::new(),
                channel: channel.to_string(),
                generation: 1,
                sequence,
                correlation_id: None,
                payload: None,
                emitted_at_ms: 0,
            });
            dispatch_text(&transport.inner, &serde_json::to_string(&message).unwrap());
        };

        dispatch("agent_session", 1);
        dispatch("file", 1);
        dispatch("file", 3);
        dispatch("sidebar", 2);
        dispatch("sidebar", 4);
        dispatch("git", 7);

        assert!(!transport.inner.sync.lock().unwrap().is_paused());
        let queued = transport.inner.events.lock().unwrap();
        assert_eq!(queued.len(), 4);
        let channels = queued
            .iter()
            .filter_map(|event| match event {
                RemoteTransportEvent::Event(inbound) => {
                    Some((inbound.event.channel.as_str(), inbound.event.sequence))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            channels,
            vec![
                ("agent_session", 1),
                ("file", 3),
                ("sidebar", 4),
                ("git", 7)
            ]
        );
    }

    #[test]
    fn auto_candidate_selection_prefers_latency_then_priority() {
        let info = RemoteGatewayInfo {
            server_id: "server_1".to_string(),
            server_identity_public_key: "key".to_string(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            ws_path: "/ws/v2".to_string(),
            pairing_claim_path: "/api/v2/pairing/claim".to_string(),
            ws_ticket_path: "/api/v2/ws-ticket".to_string(),
            deployment_mode: "lan".to_string(),
            tls_policy: "trusted_https_proxy".to_string(),
            session_epoch: 1,
            enabled_features: vec![],
            proof_challenge: String::new(),
        };
        let result = choose_direct_candidate(vec![
            CandidateProbeResult {
                candidate: DirectCandidate {
                    url: "https://tailnet".to_string(),
                    label: "tailnet".to_string(),
                    priority: 0,
                    tls_certificate_der: None,
                },
                latency_ms: 12,
                info: info.clone(),
            },
            CandidateProbeResult {
                candidate: DirectCandidate {
                    url: "https://lan".to_string(),
                    label: "lan".to_string(),
                    priority: 5,
                    tls_certificate_der: None,
                },
                latency_ms: 4,
                info: info.clone(),
            },
        ])
        .unwrap();
        assert_eq!(result.candidate.label, "lan");
        assert_eq!(
            RemoteProtocolVersion { major: 2, minor: 0 },
            info.protocol_range.max
        );
    }

    #[test]
    fn backoff_is_deterministic_and_bounded() {
        let mut config = test_config("https://desktop.example");
        config.reconnect_initial = Duration::from_millis(100);
        config.reconnect_max = Duration::from_millis(500);
        let transport = DirectWebSocketTransport::new(config).unwrap();
        let first = transport.backoff_delay(4);
        assert_eq!(first, transport.backoff_delay(4));
        assert!(first <= Duration::from_millis(500));
        assert!(transport.backoff_delay(0) >= Duration::from_millis(80));
    }

    #[test]
    fn auto_lifecycle_keeps_online_socket_for_resume_signals() {
        for signal in [
            RemoteLifecycleSignal::VisibilityChanged { visible: true },
            RemoteLifecycleSignal::ComputerResumed,
            RemoteLifecycleSignal::AppResumed,
        ] {
            assert_eq!(
                auto_lifecycle_reselection(&signal, RemoteConnectionState::Online, false),
                None
            );
            assert_eq!(
                auto_lifecycle_reselection(&signal, RemoteConnectionState::Degraded, false),
                Some(false)
            );
        }
        assert_eq!(
            auto_lifecycle_reselection(
                &RemoteLifecycleSignal::NetworkChanged,
                RemoteConnectionState::Online,
                false,
            ),
            Some(true)
        );
    }

    #[test]
    fn auto_lifecycle_reselects_online_route_after_mobile_background() {
        assert_eq!(
            auto_lifecycle_reselection(
                &RemoteLifecycleSignal::AppResumed,
                RemoteConnectionState::Online,
                true,
            ),
            Some(true)
        );
        assert_eq!(
            auto_lifecycle_reselection(
                &RemoteLifecycleSignal::AppResumed,
                RemoteConnectionState::Revoked,
                true,
            ),
            None
        );
    }

    #[test]
    fn relay_transport_requires_mobile_protocol_capabilities() {
        let mut info = RelayEndpointInfo {
            service_name: "Relay".to_string(),
            server_version: "0.1.0-rc.1".to_string(),
            protocol_version: vibex_core::RelayProtocolVersion::foundation(),
            features: RelayEndpointFeatures {
                pc_websocket: true,
                device_websocket: true,
                websocket_frames: true,
                http_pair_bridge: true,
                http_command_bridge: true,
            },
            limits: RelayEndpointLimits {
                max_total_connections: 10,
                max_body_bytes: 1024,
                max_queue_bytes_per_connection: 1024,
                max_bandwidth_bytes_per_window: 1024,
            },
        };
        info.validate_transport_capabilities().unwrap();

        info.features.websocket_frames = false;
        assert_eq!(
            info.validate_transport_capabilities().unwrap_err().code,
            "relay_transport_unavailable"
        );
    }
}

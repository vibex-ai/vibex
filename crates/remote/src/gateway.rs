use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    CONTENT_TYPE, HOST, ORIGIN, SEC_WEBSOCKET_PROTOCOL, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand_core::OsRng;
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use url::Url;
pub use vibex_core::WebBuildDescriptor as RemoteGatewayWebBuildDescriptor;
use vibex_core::{
    CorrelationId, DeviceId, ErrorCategory, EventId, REMOTE_V2_MAX_BINARY_PAYLOAD_BYTES,
    RelayRemoteHandshakeContext, RemoteActionClass, RemoteAttachmentAcceptedV2,
    RemoteAttachmentKind, RemoteAuthContext, RemoteAuthProof, RemoteBinaryFrame,
    RemoteBinaryFrameHeader, RemoteBinaryFrameKind, RemoteCancelPairingOfferRequest,
    RemoteClaimPairingOfferRequest, RemoteClaimPairingOfferResponse, RemoteCloseCode,
    RemoteCloseReason, RemoteControlMessageV2, RemoteCreatePairingOfferRequest,
    RemoteCreatePairingOfferResponse, RemoteDeviceListResponse, RemoteDevicePermissionLevel,
    RemoteDeviceRequest, RemoteEventV2, RemoteHello, RemoteJsonMessageV2, RemoteMutationContract,
    RemoteOperationKind, RemotePairingCandidate, RemotePairingOfferSummary, RemotePairingTransport,
    RemotePing, RemoteProtocolError, RemoteProtocolVersion, RemoteProtocolVersionRange,
    RemoteResyncRequired, RemoteRetryClass, RemoteRpcRequestV2, RemoteRpcResponseV2,
    RemoteRpcResultMetadata, RemoteServerInfoV2, RemoteSubscribeRequestV2,
    RemoteSubscriptionAcceptedV2, RemoteTimeoutClass, RemoteWsTicketRequest,
    RemoteWsTicketResponse, RequestId, TerminalId, VibexError, VibexResult, WorkspaceId,
    remote_permissions_for_level, unix_timestamp_ms,
};
use vibex_db::RemotePairingOfferRepository;
use vibex_terminal::TerminalManager;
use x25519_dalek::{PublicKey, StaticSecret};

use super::pairing_v2::secure_secret;
use super::{
    RemoteDispatcher, RemoteIdentity, RemoteIdentityStore, RemoteRequestEnvelope,
    RemoteResponseEnvelope, RemoteServiceConfig, RemoteTrustService, build_router_with_dispatcher,
    file_service_for_workspace, open_migrated_database, remote_timeline_event,
};

const DEFAULT_WS_TICKET_TTL_MS: u32 = 30_000;
const MAX_WS_TICKET_TTL_MS: u32 = 60_000;
const DEFAULT_OUTBOUND_QUEUE_CAPACITY: usize = 128;
const DEFAULT_MAX_CONNECTIONS: usize = 16;
const DEFAULT_MAX_IN_FLIGHT_RPCS_PER_CONNECTION: usize = 32;
const MAX_IDEMPOTENCY_CACHE_ENTRIES: usize = 1024;
const MAX_RELAY_ATTACHMENT_TASKS: usize = 32;
const DOMAIN_EVENT_QUEUE_CAPACITY: usize = 256;
const IDEMPOTENCY_CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const REMOTE_V2_SUBPROTOCOL: &str = "vibex-v2";
const REMOTE_V2_TICKET_PREFIX: &str = "vibex-ticket.";
const MAX_FILE_TRANSFER_BYTES: usize = 64 * 1024 * 1024;
const FILE_TRANSFER_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteGatewayDeploymentMode {
    Loopback,
    Lan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteGatewayTlsPolicy {
    LoopbackHttp,
    TrustedHttpsProxy,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct RemoteGatewayPairingRoutes {
    pub direct_candidates: Vec<RemotePairingCandidate>,
    pub relay_candidate: Option<RemotePairingCandidate>,
}

impl std::fmt::Debug for RemoteGatewayPairingRoutes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteGatewayPairingRoutes")
            .field("direct_candidate_count", &self.direct_candidates.len())
            .field("has_relay_candidate", &self.relay_candidate.is_some())
            .finish()
    }
}

impl RemoteGatewayPairingRoutes {
    fn validated(&self) -> VibexResult<Self> {
        if self.direct_candidates.len() > 8 {
            return Err(VibexError::validation(
                "remote_pairing_candidates_too_many",
                "RemoteGateway has too many advertised Direct candidates",
            ));
        }
        let direct_candidates = self
            .direct_candidates
            .iter()
            .cloned()
            .map(super::pairing_v2::validate_pairing_candidate)
            .collect::<VibexResult<Vec<_>>>()?;
        if direct_candidates.iter().any(|candidate| {
            !matches!(
                candidate.transport,
                RemotePairingTransport::Direct | RemotePairingTransport::Tailnet
            )
        }) {
            return Err(VibexError::validation(
                "remote_pairing_direct_candidate_invalid",
                "RemoteGateway Direct routes must use Direct or Tailnet transport",
            ));
        }
        let relay_candidate = self
            .relay_candidate
            .clone()
            .map(super::pairing_v2::validate_pairing_candidate)
            .transpose()?;
        if relay_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.transport != RemotePairingTransport::SelfHostedRelay)
        {
            return Err(VibexError::validation(
                "remote_pairing_relay_candidate_invalid",
                "RemoteGateway Relay route must use self-hosted Relay transport",
            ));
        }
        Ok(Self {
            direct_candidates,
            relay_candidate,
        })
    }

    fn is_empty(&self) -> bool {
        self.direct_candidates.is_empty() && self.relay_candidate.is_none()
    }
}

fn create_pairing_offer_with_routes(
    connection: &vibex_db::DbConnection,
    identity: &RemoteIdentity,
    mut request: RemoteCreatePairingOfferRequest,
    routes: &Arc<Mutex<RemoteGatewayPairingRoutes>>,
) -> VibexResult<RemoteCreatePairingOfferResponse> {
    let routes = routes.lock().map_err(|_| gateway_state_error())?.clone();
    if routes.is_empty() {
        return Err(VibexError::capability(
            "remote_pairing_routes_unavailable",
            "RemoteGateway has no server-owned Direct or self-hosted Relay pairing route",
        ));
    }
    request.direct_candidates = routes.direct_candidates;
    request.relay_candidate = routes.relay_candidate;
    RemoteTrustService::create_pairing_offer(connection, identity, request)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteGatewayConfig {
    pub service: RemoteServiceConfig,
    pub deployment_mode: RemoteGatewayDeploymentMode,
    pub tls_policy: RemoteGatewayTlsPolicy,
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub static_dir: Option<PathBuf>,
    pub web_build: Option<RemoteGatewayWebBuildDescriptor>,
    pub pairing_routes: RemoteGatewayPairingRoutes,
    pub max_connections: usize,
    pub max_in_flight_rpcs_per_connection: usize,
    pub outbound_queue_capacity: usize,
    pub ws_ticket_ttl_ms: u32,
}

impl Default for RemoteGatewayConfig {
    fn default() -> Self {
        Self {
            service: RemoteServiceConfig::loopback_disabled(),
            deployment_mode: RemoteGatewayDeploymentMode::Loopback,
            tls_policy: RemoteGatewayTlsPolicy::LoopbackHttp,
            allowed_hosts: vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string(),
            ],
            allowed_origins: vec![
                "http://localhost".to_string(),
                "http://127.0.0.1".to_string(),
                "http://[::1]".to_string(),
                "https://localhost".to_string(),
                "https://127.0.0.1".to_string(),
                "https://[::1]".to_string(),
            ],
            static_dir: None,
            web_build: None,
            pairing_routes: RemoteGatewayPairingRoutes::default(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_in_flight_rpcs_per_connection: DEFAULT_MAX_IN_FLIGHT_RPCS_PER_CONNECTION,
            outbound_queue_capacity: DEFAULT_OUTBOUND_QUEUE_CAPACITY,
            ws_ticket_ttl_ms: DEFAULT_WS_TICKET_TTL_MS,
        }
    }
}

impl RemoteGatewayConfig {
    pub fn loopback_enabled(bind_addr: impl Into<String>) -> Self {
        let mut config = Self::default();
        config.service.enabled = true;
        config.service.bind_addr = bind_addr.into();
        if let Ok(address) = config.service.bind_addr.parse::<SocketAddr>() {
            let host = match address.ip() {
                IpAddr::V4(address) => address.to_string(),
                IpAddr::V6(address) => format!("[{address}]"),
            };
            if address.port() != 0 {
                config
                    .allowed_origins
                    .push(format!("http://{host}:{}", address.port()));
                config
                    .allowed_origins
                    .push(format!("https://{host}:{}", address.port()));
            }
        }
        config
    }

    pub fn validate(&self) -> VibexResult<SocketAddr> {
        let bind_addr = self.service.bind_addr.parse::<SocketAddr>().map_err(|_| {
            VibexError::validation(
                "remote_gateway_bind_addr_invalid",
                "RemoteGateway bind address must be an IP socket address",
            )
        })?;
        if self.max_connections == 0
            || self.max_in_flight_rpcs_per_connection == 0
            || self.outbound_queue_capacity == 0
            || self.allowed_hosts.is_empty()
            || self
                .allowed_hosts
                .iter()
                .any(|host| normalize_host(host).is_none())
        {
            return Err(VibexError::validation(
                "remote_gateway_config_invalid",
                "RemoteGateway limits and Host allowlist must be valid",
            ));
        }
        if self.ws_ticket_ttl_ms == 0 || self.ws_ticket_ttl_ms > MAX_WS_TICKET_TTL_MS {
            return Err(VibexError::validation(
                "remote_gateway_ticket_ttl_invalid",
                "RemoteGateway WS ticket TTL is invalid",
            ));
        }
        if self.deployment_mode == RemoteGatewayDeploymentMode::Loopback
            && !bind_addr.ip().is_loopback()
        {
            return Err(VibexError::validation(
                "remote_gateway_lan_requires_opt_in",
                "RemoteGateway requires explicit LAN mode before binding a non-loopback address",
            ));
        }
        if self.deployment_mode == RemoteGatewayDeploymentMode::Lan
            && self.tls_policy != RemoteGatewayTlsPolicy::TrustedHttpsProxy
        {
            return Err(VibexError::validation(
                "remote_gateway_lan_tls_required",
                "LAN RemoteGateway requires a trusted HTTPS/WSS proxy or Tailscale Serve",
            ));
        }
        if self.deployment_mode == RemoteGatewayDeploymentMode::Lan
            && self
                .allowed_hosts
                .iter()
                .filter_map(|host| normalize_host(host))
                .any(|host| host == "localhost" || is_loopback_host(&host))
        {
            return Err(VibexError::validation(
                "remote_gateway_lan_host_allowlist_invalid",
                "LAN RemoteGateway Host allowlist must contain only explicit deployment hosts",
            ));
        }
        for origin in &self.allowed_origins {
            validate_origin_value(origin)?;
        }
        if let Some(static_dir) = &self.static_dir
            && static_dir.as_os_str().is_empty()
        {
            return Err(VibexError::validation(
                "remote_gateway_static_dir_invalid",
                "RemoteGateway static asset directory is invalid",
            ));
        }
        let pairing_routes = self.pairing_routes.validated()?;
        if !pairing_routes.direct_candidates.is_empty() && !self.service.enabled {
            return Err(VibexError::validation(
                "remote_pairing_direct_gateway_disabled",
                "advertised Direct pairing routes require the RemoteGateway listener",
            ));
        }
        Ok(bind_addr)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteGatewayStatus {
    pub running: bool,
    pub bound_addr: Option<SocketAddr>,
    pub session_epoch: u64,
    pub active_connections: usize,
}

#[derive(Clone)]
pub struct RemoteGateway {
    inner: Arc<RemoteGatewayInner>,
}

struct RemoteGatewayInner {
    config: RemoteGatewayConfig,
    config_state: RwLock<RemoteGatewayConfig>,
    config_guard: Mutex<()>,
    dispatcher: RemoteDispatcher,
    db_path: PathBuf,
    identity_store: RemoteIdentityStore,
    lifecycle: Mutex<GatewayLifecycle>,
    lifecycle_guard: tokio::sync::Mutex<()>,
    tickets: Arc<Mutex<HashMap<String, WsTicketRecord>>>,
    registry: ConnectionRegistry,
    idempotency: Arc<Mutex<HashMap<IdempotencyCacheKey, CachedRpcResponse>>>,
    domain_events: GatewayDomainEvents,
    pairing_routes: Arc<Mutex<RemoteGatewayPairingRoutes>>,
    session_epoch: AtomicU64,
}

#[derive(Clone)]
struct GatewayDomainEvents {
    sender: broadcast::Sender<RemoteEventV2>,
    sequences: Arc<Mutex<HashMap<(u64, String), u64>>>,
}

impl Default for GatewayDomainEvents {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(DOMAIN_EVENT_QUEUE_CAPACITY);
        Self {
            sender,
            sequences: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl GatewayDomainEvents {
    fn subscribe(&self) -> broadcast::Receiver<RemoteEventV2> {
        self.sender.subscribe()
    }

    fn publish(
        &self,
        channel: &str,
        generation: u64,
        correlation_id: Option<CorrelationId>,
    ) -> VibexResult<()> {
        let sequence = {
            let mut sequences = self.sequences.lock().map_err(|_| gateway_state_error())?;
            let sequence = sequences
                .entry((generation, channel.to_string()))
                .or_insert(0);
            *sequence = sequence.saturating_add(1).max(1);
            *sequence
        };
        let _ = self.sender.send(RemoteEventV2 {
            event_id: EventId::new(),
            channel: channel.to_string(),
            generation,
            sequence,
            correlation_id,
            payload: None,
            emitted_at_ms: unix_timestamp_ms(),
        });
        Ok(())
    }
}

#[derive(Default)]
struct GatewayLifecycle {
    bound_addr: Option<SocketAddr>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl RemoteGateway {
    pub fn new(
        config: RemoteGatewayConfig,
        dispatcher: RemoteDispatcher,
        db_path: impl Into<PathBuf>,
        identity_path: impl Into<PathBuf>,
    ) -> Self {
        let initial_config = config.clone();
        let pairing_routes = config.pairing_routes.clone();
        Self {
            inner: Arc::new(RemoteGatewayInner {
                config,
                config_state: RwLock::new(initial_config),
                config_guard: Mutex::new(()),
                dispatcher,
                db_path: db_path.into(),
                identity_store: RemoteIdentityStore::new(identity_path),
                lifecycle: Mutex::new(GatewayLifecycle::default()),
                lifecycle_guard: tokio::sync::Mutex::new(()),
                tickets: Arc::new(Mutex::new(HashMap::new())),
                registry: ConnectionRegistry::default(),
                idempotency: Arc::new(Mutex::new(HashMap::new())),
                domain_events: GatewayDomainEvents::default(),
                pairing_routes: Arc::new(Mutex::new(pairing_routes)),
                session_epoch: AtomicU64::new(0),
            }),
        }
    }

    pub fn config(&self) -> &RemoteGatewayConfig {
        &self.inner.config
    }

    /// Returns the configuration used by the current listener epoch.  The
    /// original `config()` accessor remains for compatibility with callers
    /// that only need the construction-time defaults.
    pub fn current_config(&self) -> RemoteGatewayConfig {
        self.inner
            .config_state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn publish_provider_invalidation(&self) -> VibexResult<()> {
        self.inner.domain_events.publish(
            "provider",
            self.inner.session_epoch.load(Ordering::Acquire),
            None,
        )
    }

    /// Replace the validated Gateway configuration while the listener is
    /// stopped.  A running epoch owns an immutable router snapshot; callers
    /// must stop/restart explicitly when they need a new listener config.
    pub async fn apply_config_while_stopped(
        &self,
        mut config: RemoteGatewayConfig,
    ) -> VibexResult<()> {
        let _guard = self.inner.lifecycle_guard.lock().await;
        if self.status().running {
            return Err(VibexError::conflict(
                "remote_gateway_config_running",
                "RemoteGateway configuration can only change while stopped",
            ));
        }
        config.pairing_routes = config.pairing_routes.validated()?;
        config.validate()?;
        let _config_guard = self
            .inner
            .config_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut routes = self
            .inner
            .pairing_routes
            .lock()
            .map_err(|_| gateway_state_error())?;
        let mut state = self
            .inner
            .config_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *routes = config.pairing_routes.clone();
        *state = config;
        Ok(())
    }

    /// Atomically replace server-owned pairing routes without changing the
    /// inbound listener.  This is used by Relay-only mode, where the Gateway
    /// remains disabled but the trust service still needs a route candidate.
    pub fn set_pairing_routes(&self, routes: RemoteGatewayPairingRoutes) -> VibexResult<()> {
        let routes = routes.validated()?;
        let _config_guard = self
            .inner
            .config_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next_config = self.current_config();
        next_config.pairing_routes = routes.clone();
        next_config.validate()?;
        let mut current = self
            .inner
            .pairing_routes
            .lock()
            .map_err(|_| gateway_state_error())?;
        let mut config = self
            .inner
            .config_state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current = routes;
        *config = next_config;
        Ok(())
    }

    pub fn identity(&self) -> VibexResult<RemoteIdentity> {
        self.inner.identity_store.load_or_create()
    }

    pub fn pairing_routes_available(&self) -> bool {
        self.inner
            .pairing_routes
            .lock()
            .map(|routes| !routes.is_empty())
            .unwrap_or(false)
    }

    pub fn create_pairing_offer(
        &self,
        request: RemoteCreatePairingOfferRequest,
    ) -> VibexResult<RemoteCreatePairingOfferResponse> {
        let connection = open_migrated_database(&self.inner.db_path)?;
        let identity = self.identity()?;
        create_pairing_offer_with_routes(
            &connection,
            &identity,
            request,
            &self.inner.pairing_routes,
        )
    }

    pub fn pairing_offer_status(
        &self,
        offer_id: &RequestId,
    ) -> VibexResult<RemotePairingOfferSummary> {
        let connection = open_migrated_database(&self.inner.db_path)?;
        RemotePairingOfferRepository::get(&connection, offer_id)?
            .map(|record| record.summary)
            .ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Remote,
                    "remote_pairing_offer_unknown",
                    "pairing offer is unknown",
                )
            })
    }

    pub fn cancel_pairing_offer(
        &self,
        request: RemoteCancelPairingOfferRequest,
    ) -> VibexResult<RemotePairingOfferSummary> {
        let connection = open_migrated_database(&self.inner.db_path)?;
        RemoteTrustService::cancel_pairing_offer(&connection, request)
    }

    pub fn relay_transport_private_key(&self) -> VibexResult<[u8; 32]> {
        self.identity()?.relay_transport_private_key()
    }

    /// Claims a short-lived pairing offer on behalf of the Relay pairing-only
    /// path. The request is received only after the Relay session has been
    /// decrypted by the PC; the Relay server never sees the challenge or grant.
    pub fn relay_claim_pairing_offer(
        &self,
        request: RemoteClaimPairingOfferRequest,
    ) -> VibexResult<RemoteClaimPairingOfferResponse> {
        let connection = open_migrated_database(&self.inner.db_path)?;
        RemoteTrustService::claim_pairing_offer(&connection, request)
    }

    pub fn relay_handshake_context(
        &self,
        device_id: &DeviceId,
        device_identity_public_key: &str,
        transport_endpoint: &str,
    ) -> VibexResult<RelayRemoteHandshakeContext> {
        let identity = self.identity()?;
        let connection = open_migrated_database(&self.inner.db_path)?;
        let device =
            vibex_db::RemoteDeviceRepository::get(&connection, device_id)?.ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Remote,
                    "remote_device_unknown",
                    "remote device is unknown",
                )
            })?;
        if device.detail.status == vibex_core::RemoteDeviceStatus::Revoked {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_device_revoked",
                "remote device is revoked",
            ));
        }
        if device.detail.public_key.as_deref() != Some(device_identity_public_key) {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_device_identity_mismatch",
                "relay device identity does not match the paired device",
            ));
        }
        let session_epoch = self.ensure_session_epoch();
        let proof_challenge = secure_secret("relay-proof");
        let permission_context_hash = relay_permission_context_hash(
            device_id,
            device.detail.grant_revision,
            device.detail.permission_level,
            session_epoch,
        )?;
        Ok(RelayRemoteHandshakeContext {
            device_id: device_id.clone(),
            device_identity_public_key: device_identity_public_key.to_string(),
            desktop_identity_public_key: identity.public_key_base64(),
            proof_challenge,
            session_epoch,
            permission_context_hash,
            transport_endpoint: transport_endpoint.trim().to_string(),
        })
    }

    pub fn relay_server_info(
        &self,
        context: &RelayRemoteHandshakeContext,
        hello: &RemoteHello,
    ) -> VibexResult<RemoteServerInfoV2> {
        if hello.device_id != context.device_id
            || hello.device_identity_public_key != context.device_identity_public_key
            || context.session_epoch != self.ensure_session_epoch()
            || hello.transport_endpoint.as_deref() != Some(context.transport_endpoint.as_str())
            || hello.permission_context_hash.as_deref()
                != Some(context.permission_context_hash.as_str())
        {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_relay_handshake_context_mismatch",
                "relay Remote v2 handshake context did not match the client hello",
            ));
        }
        let identity = self.identity()?;
        if identity.public_key_base64() != context.desktop_identity_public_key {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_server_identity_mismatch",
                "relay Remote v2 desktop identity changed",
            ));
        }
        let connection = open_migrated_database(&self.inner.db_path)?;
        let device = vibex_db::RemoteDeviceRepository::get(&connection, &context.device_id)?
            .ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Remote,
                    "remote_device_unknown",
                    "remote device is unknown",
                )
            })?;
        if device.detail.status == vibex_core::RemoteDeviceStatus::Revoked {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_device_revoked",
                "remote device is revoked",
            ));
        }
        let permission_hash = relay_permission_context_hash(
            &context.device_id,
            device.detail.grant_revision,
            device.detail.permission_level,
            context.session_epoch,
        )?;
        if permission_hash != context.permission_context_hash {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_permission_context_changed",
                "remote device permissions changed during Relay handshake",
            ));
        }
        let state = GatewayState {
            config: Arc::new(self.current_config()),
            dispatcher: self.inner.dispatcher.clone(),
            db_path: self.inner.db_path.clone(),
            identity,
            tickets: self.inner.tickets.clone(),
            registry: self.inner.registry.clone(),
            idempotency: self.inner.idempotency.clone(),
            domain_events: self.inner.domain_events.clone(),
            pairing_routes: self.inner.pairing_routes.clone(),
            session_epoch: context.session_epoch,
        };
        let auth = RemoteAuthContext {
            device_id: device.detail.device_id,
            display_name: device.detail.display_name,
            permission_level: device.detail.permission_level,
            authenticated_at_ms: unix_timestamp_ms(),
        };
        let ticket = WsTicketRecord {
            proof: RemoteAuthProof {
                device_id: auth.device_id.clone(),
                auth_token: String::new(),
            },
            auth: auth.clone(),
            expires_at_ms: i64::MAX,
            proof_challenge: context.proof_challenge.clone(),
            relay_authenticated: true,
        };
        let session_crypto = verify_hello_device_identity(&state, hello, &ticket)?;
        let selected_protocol = RemoteProtocolVersionRange::v2()
            .negotiate(hello.protocol_range)
            .ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Remote,
                    "remote_protocol_incompatible",
                    "client and server protocol ranges are incompatible",
                )
            })?;
        Ok(RemoteServerInfoV2 {
            server_id: state.identity.server_id().to_string(),
            server_identity_public_key: state.identity.public_key_base64(),
            desktop_version: state.config.service.server_version.clone(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            selected_protocol,
            server_ephemeral_public_key: session_crypto.server_ephemeral_public_key,
            session_key_confirmation: session_crypto.session_key_confirmation,
            capabilities: gateway_capabilities(),
            enabled_features: gateway_features(&state),
            device_permissions: remote_permissions_for_level(auth.permission_level),
            session_epoch: context.session_epoch,
            connection_id: RequestId::new(),
            server_time_ms: unix_timestamp_ms(),
        })
    }

    pub async fn relay_process_json(
        &self,
        context: &RelayRemoteHandshakeContext,
        message: RemoteJsonMessageV2,
        subscriptions: &Arc<Mutex<HashSet<String>>>,
        authenticated_proof: Option<&RemoteAuthProof>,
        session_outbound: Option<&mpsc::Sender<RelayRemoteOutbound>>,
        attachment_tasks: &mut RelayAttachmentTasks,
    ) -> VibexResult<Vec<RemoteJsonMessageV2>> {
        let (state, ticket) = self.relay_active_state(context)?;
        match message {
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Hello(hello)) => {
                let proof = hello.relay_auth.as_ref().ok_or_else(|| {
                    VibexError::new(
                        ErrorCategory::Permission,
                        "remote_relay_auth_required",
                        "relay Remote v2 hello requires an E2EE device grant",
                    )
                })?;
                if proof.device_id != context.device_id {
                    return Err(VibexError::new(
                        ErrorCategory::Permission,
                        "remote_relay_auth_mismatch",
                        "relay Remote v2 grant belongs to another device",
                    ));
                }
                let connection = open_migrated_database(&self.inner.db_path)?;
                RemoteTrustService::authenticate(&connection, proof.clone())?;
                Ok(vec![RemoteJsonMessageV2::Control(
                    RemoteControlMessageV2::ServerInfo(self.relay_server_info(context, &hello)?),
                )])
            }
            RemoteJsonMessageV2::RpcRequest(request) => {
                let proof = self.relay_authenticated_proof(context, authenticated_proof)?;
                Ok(vec![RemoteJsonMessageV2::RpcResponse(
                    process_rpc(&state, proof, request).await,
                )])
            }
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Ping(ping)) => {
                self.relay_authenticated_proof(context, authenticated_proof)?;
                Ok(vec![RemoteJsonMessageV2::Control(
                    RemoteControlMessageV2::Pong(RemotePing {
                        nonce: ping.nonce,
                        sent_at_ms: unix_timestamp_ms(),
                    }),
                )])
            }
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Subscribe(request)) => {
                self.relay_authenticated_proof(context, authenticated_proof)?;
                Ok(vec![RemoteJsonMessageV2::Control(
                    RemoteControlMessageV2::Subscribed(update_subscriptions(
                        subscriptions,
                        request,
                        ticket.auth.permission_level,
                    )),
                )])
            }
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Attach(request)) => {
                self.relay_authenticated_proof(context, authenticated_proof)?;
                let action = match request.kind {
                    RemoteAttachmentKind::AgentTimeline => RemoteActionClass::ReadAgentSession,
                    RemoteAttachmentKind::Terminal
                    | RemoteAttachmentKind::FileTransfer
                    | RemoteAttachmentKind::Git => RemoteActionClass::ReadProject,
                    RemoteAttachmentKind::Provider => RemoteActionClass::ReadProviderSettings,
                    RemoteAttachmentKind::Unknown => {
                        return Ok(vec![RemoteJsonMessageV2::Control(
                            RemoteControlMessageV2::ResyncRequired(RemoteResyncRequired {
                                domain: request.attachment_id,
                                generation: context.session_epoch,
                                reason: "attachment kind is not supported".to_string(),
                                authoritative_operation: "info".to_string(),
                            }),
                        )]);
                    }
                };
                authorize_live_action(&state, &ticket, action)?;
                attachment_tasks.detach(&request.attachment_id);
                let mut accepted = RemoteAttachmentAcceptedV2 {
                    attachment_id: request.attachment_id.clone(),
                    generation: context.session_epoch,
                    next_sequence: request.after_sequence.max(1),
                    snapshot_required: request.generation != context.session_epoch,
                };
                match request.kind {
                    RemoteAttachmentKind::Terminal => {
                        if let Some(outbound) = session_outbound
                            && let Some(manager) = terminal_manager(&state.dispatcher)
                            && let Ok(terminal_id) = TerminalId::parse(request.resource_id.clone())
                        {
                            let task = spawn_relay_terminal_stream(
                                manager,
                                terminal_id,
                                request.resource_id,
                                request.after_sequence,
                                context.session_epoch,
                                outbound.clone(),
                            );
                            attachment_tasks.insert(request.attachment_id.clone(), task)?;
                        } else {
                            accepted.snapshot_required = true;
                        }
                    }
                    RemoteAttachmentKind::FileTransfer => {
                        let (workspace_id, bytes) = prepare_file_download(
                            &state,
                            request.scope_id.as_deref(),
                            &request.resource_id,
                        )?;
                        if let Some(outbound) = session_outbound {
                            let task = spawn_relay_file_download_stream(
                                bytes,
                                workspace_id,
                                request.attachment_id.clone(),
                                context.session_epoch,
                                request.after_sequence,
                                outbound.clone(),
                            );
                            attachment_tasks.insert(request.attachment_id.clone(), task)?;
                        } else {
                            accepted.snapshot_required = true;
                        }
                    }
                    RemoteAttachmentKind::AgentTimeline
                    | RemoteAttachmentKind::Git
                    | RemoteAttachmentKind::Provider
                    | RemoteAttachmentKind::Unknown => {}
                }
                Ok(vec![RemoteJsonMessageV2::Control(
                    RemoteControlMessageV2::Attached(accepted),
                )])
            }
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Detach(request)) => {
                self.relay_authenticated_proof(context, authenticated_proof)?;
                attachment_tasks.detach(&request.attachment_id);
                Ok(vec![RemoteJsonMessageV2::Control(
                    RemoteControlMessageV2::Detached(request),
                )])
            }
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Close(_)) => {
                self.relay_authenticated_proof(context, authenticated_proof)?;
                Ok(Vec::new())
            }
            RemoteJsonMessageV2::Control(_)
            | RemoteJsonMessageV2::RpcResponse(_)
            | RemoteJsonMessageV2::Event(_)
            | RemoteJsonMessageV2::Unknown => Err(VibexError::validation(
                "remote_relay_frame_direction_invalid",
                "relay client sent a server-only or unknown Remote v2 frame",
            )),
        }
    }

    fn relay_authenticated_proof<'a>(
        &self,
        context: &RelayRemoteHandshakeContext,
        authenticated_proof: Option<&'a RemoteAuthProof>,
    ) -> VibexResult<&'a RemoteAuthProof> {
        let proof = authenticated_proof.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                "remote_relay_auth_required",
                "relay Remote v2 frame requires an authenticated inner session",
            )
        })?;
        if proof.device_id != context.device_id {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                "remote_relay_auth_mismatch",
                "relay Remote v2 grant belongs to another device",
            ));
        }
        let connection = open_migrated_database(&self.inner.db_path)?;
        RemoteTrustService::authenticate(&connection, proof.clone())?;
        Ok(proof)
    }

    pub async fn relay_process_binary(
        &self,
        context: &RelayRemoteHandshakeContext,
        encoded: &[u8],
        sequences: &mut HashMap<(String, u64), u64>,
        authenticated_proof: Option<&RemoteAuthProof>,
    ) -> VibexResult<Vec<RelayRemoteOutbound>> {
        let (state, ticket) = self.relay_active_state(context)?;
        self.relay_authenticated_proof(context, authenticated_proof)?;
        let (outbound, mut receiver) = mpsc::channel(8);
        handle_active_binary(&state, &ticket, encoded, &outbound, sequences).await?;
        drop(outbound);
        let mut messages = Vec::new();
        while let Some(frame) = receiver.recv().await {
            match frame {
                OutboundFrame::Text(text) => {
                    if let Ok(message) = serde_json::from_str(&text) {
                        messages.push(RelayRemoteOutbound::Json(message));
                    }
                }
                OutboundFrame::Binary(bytes) => {
                    messages.push(RelayRemoteOutbound::Binary(bytes));
                }
                OutboundFrame::Close(reason) => {
                    messages.push(RelayRemoteOutbound::Json(RemoteJsonMessageV2::Control(
                        RemoteControlMessageV2::Close(reason),
                    )));
                }
            }
        }
        Ok(messages)
    }

    pub fn relay_outbound(
        &self,
        context: &RelayRemoteHandshakeContext,
        subscriptions: Arc<Mutex<HashSet<String>>>,
    ) -> VibexResult<(
        mpsc::Sender<RelayRemoteOutbound>,
        mpsc::Receiver<RelayRemoteOutbound>,
    )> {
        let (state, ticket) = self.relay_active_state(context)?;
        let (outbound, receiver) = mpsc::channel(state.config.outbound_queue_capacity);
        let (frames, mut frame_receiver) = mpsc::channel(state.config.outbound_queue_capacity);
        let connection_id = RequestId::new();
        let mut disconnect = state.registry.register(
            connection_id.clone(),
            context.device_id.clone(),
            state.config.max_connections,
        )?;
        let event_tasks = spawn_gateway_events(
            &state,
            frames.clone(),
            subscriptions,
            ticket.auth.permission_level,
        );
        let gateway = self.clone();
        let context = context.clone();
        let registry = state.registry.clone();
        let sender = outbound.clone();
        tokio::spawn(async move {
            let mut validation = tokio::time::interval(Duration::from_millis(500));
            loop {
                let message = tokio::select! {
                    changed = disconnect.changed() => {
                        let reason = changed
                            .ok()
                            .and_then(|_| disconnect.borrow().clone())
                            .unwrap_or_else(|| relay_close_reason(
                                "remote_relay_session_closed",
                                "Relay Remote v2 session closed",
                            ));
                        Some(RelayRemoteOutbound::Json(RemoteJsonMessageV2::Control(
                            RemoteControlMessageV2::Close(reason),
                        )))
                    }
                    _ = validation.tick() => match gateway.relay_active_state(&context) {
                        Ok(_) => None,
                        Err(error) => Some(RelayRemoteOutbound::Json(
                            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Close(
                                relay_close_reason(&error.code, &error.message),
                            )),
                        )),
                    },
                    frame = frame_receiver.recv() => match frame {
                        Some(OutboundFrame::Text(text)) => serde_json::from_str(&text)
                            .ok()
                            .map(RelayRemoteOutbound::Json),
                        Some(OutboundFrame::Binary(bytes)) => {
                            Some(RelayRemoteOutbound::Binary(bytes))
                        }
                        Some(OutboundFrame::Close(reason)) => Some(RelayRemoteOutbound::Json(
                            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Close(reason)),
                        )),
                        None => break,
                    },
                };
                if let Some(message) = message {
                    let closes = matches!(
                        message,
                        RelayRemoteOutbound::Json(RemoteJsonMessageV2::Control(
                            RemoteControlMessageV2::Close(_)
                        ))
                    );
                    if outbound.send(message).await.is_err() || closes {
                        break;
                    }
                }
            }
            event_tasks.abort();
            registry.unregister(&connection_id);
        });
        Ok((sender, receiver))
    }

    fn relay_active_state(
        &self,
        context: &RelayRemoteHandshakeContext,
    ) -> VibexResult<(GatewayState, WsTicketRecord)> {
        if context.session_epoch != self.ensure_session_epoch() {
            return Err(VibexError::conflict(
                "remote_relay_session_epoch_stale",
                "relay Remote v2 session belongs to an old desktop epoch",
            ));
        }
        let connection = open_migrated_database(&self.inner.db_path)?;
        let device = vibex_db::RemoteDeviceRepository::get(&connection, &context.device_id)?
            .ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Remote,
                    "remote_device_unknown",
                    "remote device is unknown",
                )
            })?;
        if device.detail.status == vibex_core::RemoteDeviceStatus::Revoked
            || device.detail.public_key.as_deref()
                != Some(context.device_identity_public_key.as_str())
        {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                "remote_device_revoked",
                "remote device identity is no longer authorized",
            ));
        }
        let permission_hash = relay_permission_context_hash(
            &context.device_id,
            device.detail.grant_revision,
            device.detail.permission_level,
            context.session_epoch,
        )?;
        if permission_hash != context.permission_context_hash {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                "remote_permission_context_changed",
                "remote device permissions changed; establish a new Relay session",
            ));
        }
        let identity = self.identity()?;
        let auth = RemoteAuthContext {
            device_id: device.detail.device_id,
            display_name: device.detail.display_name,
            permission_level: device.detail.permission_level,
            authenticated_at_ms: unix_timestamp_ms(),
        };
        Ok((
            GatewayState {
                config: Arc::new(self.current_config()),
                dispatcher: self.inner.dispatcher.clone(),
                db_path: self.inner.db_path.clone(),
                identity,
                tickets: self.inner.tickets.clone(),
                registry: self.inner.registry.clone(),
                idempotency: self.inner.idempotency.clone(),
                domain_events: self.inner.domain_events.clone(),
                pairing_routes: self.inner.pairing_routes.clone(),
                session_epoch: context.session_epoch,
            },
            WsTicketRecord {
                proof: RemoteAuthProof {
                    device_id: auth.device_id.clone(),
                    auth_token: String::new(),
                },
                auth,
                expires_at_ms: i64::MAX,
                proof_challenge: context.proof_challenge.clone(),
                relay_authenticated: true,
            },
        ))
    }

    pub fn status(&self) -> RemoteGatewayStatus {
        let lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        RemoteGatewayStatus {
            running: lifecycle.task.is_some(),
            bound_addr: lifecycle.bound_addr,
            session_epoch: self.inner.session_epoch.load(Ordering::Acquire),
            active_connections: self.inner.registry.active_count(),
        }
    }

    pub fn router(&self) -> VibexResult<Router> {
        let config = self.current_config();
        self.router_with_config(config)
    }

    fn router_with_config(&self, config: RemoteGatewayConfig) -> VibexResult<Router> {
        config.validate()?;
        let identity = self.identity()?;
        let epoch = self.ensure_session_epoch();
        Ok(build_gateway_router(GatewayState {
            config: Arc::new(config),
            dispatcher: self.inner.dispatcher.clone(),
            db_path: self.inner.db_path.clone(),
            identity,
            tickets: self.inner.tickets.clone(),
            registry: self.inner.registry.clone(),
            idempotency: self.inner.idempotency.clone(),
            domain_events: self.inner.domain_events.clone(),
            pairing_routes: self.inner.pairing_routes.clone(),
            session_epoch: epoch,
        }))
    }

    pub async fn start(&self) -> VibexResult<Option<SocketAddr>> {
        let _guard = self.inner.lifecycle_guard.lock().await;
        if let Some(address) = self.status().bound_addr {
            return Ok(Some(address));
        }
        let config = self.current_config();
        if !config.service.enabled {
            return Ok(None);
        }
        let bind_addr = config.validate()?;
        self.bump_session_epoch();
        let router = self.router_with_config(config)?;
        let listener = tokio::net::TcpListener::bind(bind_addr)
            .await
            .map_err(|error| {
                VibexError::process(
                    "remote_gateway_bind_failed",
                    "RemoteGateway listener could not bind",
                )
                .with_diagnostic("errorKind", format!("{:?}", error.kind()))
            })?;
        let bound_addr = listener.local_addr().map_err(|error| {
            VibexError::process(
                "remote_gateway_local_addr_failed",
                "RemoteGateway listener address is unavailable",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        let mut lifecycle = self
            .inner
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lifecycle.bound_addr = Some(bound_addr);
        lifecycle.shutdown = Some(shutdown_tx);
        lifecycle.task = Some(task);
        Ok(Some(bound_addr))
    }

    pub async fn stop(&self) -> VibexResult<()> {
        let _guard = self.inner.lifecycle_guard.lock().await;
        self.inner.registry.disconnect_all(RemoteCloseReason {
            code: RemoteCloseCode::ServerShutdown,
            message: "RemoteGateway is shutting down".to_string(),
            retry: RemoteRetryClass::Reconnect,
        });
        let (shutdown, task) = {
            let mut lifecycle = self
                .inner
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            lifecycle.bound_addr = None;
            (lifecycle.shutdown.take(), lifecycle.task.take())
        };
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = task
            && tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }

    pub async fn restart(&self) -> VibexResult<Option<SocketAddr>> {
        self.stop().await?;
        self.start().await
    }

    pub fn disconnect_device(&self, device_id: &DeviceId) {
        self.inner.registry.disconnect_device(
            device_id,
            RemoteCloseReason {
                code: RemoteCloseCode::DeviceRevoked,
                message: "Remote device was revoked".to_string(),
                retry: RemoteRetryClass::Never,
            },
        );
    }

    fn ensure_session_epoch(&self) -> u64 {
        let current = self.inner.session_epoch.load(Ordering::Acquire);
        if current != 0 {
            return current;
        }
        self.bump_session_epoch()
    }

    fn bump_session_epoch(&self) -> u64 {
        let now = u64::try_from(unix_timestamp_ms()).unwrap_or(1).max(1);
        let mut current = self.inner.session_epoch.load(Ordering::Acquire);
        loop {
            let next = now.max(current.saturating_add(1));
            match self.inner.session_epoch.compare_exchange(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum RelayRemoteOutbound {
    Json(RemoteJsonMessageV2),
    Binary(Vec<u8>),
}

#[derive(Default)]
pub struct RelayAttachmentTasks {
    tasks: HashMap<String, JoinHandle<()>>,
}

impl std::fmt::Debug for RelayAttachmentTasks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayAttachmentTasks")
            .field("active_tasks", &self.tasks.len())
            .finish()
    }
}

impl RelayAttachmentTasks {
    fn insert(&mut self, attachment_id: String, task: JoinHandle<()>) -> VibexResult<()> {
        self.tasks.retain(|_, task| !task.is_finished());
        if self.tasks.len() >= MAX_RELAY_ATTACHMENT_TASKS {
            task.abort();
            return Err(VibexError::capability(
                "remote_attachment_limit",
                "remote attachment task limit was reached",
            ));
        }
        if let Some(previous) = self.tasks.insert(attachment_id, task) {
            previous.abort();
        }
        Ok(())
    }

    fn detach(&mut self, attachment_id: &str) {
        if let Some(task) = self.tasks.remove(attachment_id) {
            task.abort();
        }
    }
}

impl Drop for RelayAttachmentTasks {
    fn drop(&mut self) {
        for (_, task) in self.tasks.drain() {
            task.abort();
        }
    }
}

fn relay_close_reason(code: &str, message: &str) -> RemoteCloseReason {
    let revoked = matches!(
        code,
        "remote_device_revoked"
            | "remote_device_unknown"
            | "remote_device_identity_mismatch"
            | "remote_permission_context_changed"
    );
    RemoteCloseReason {
        code: if revoked {
            RemoteCloseCode::DeviceRevoked
        } else {
            RemoteCloseCode::PolicyViolation
        },
        message: message.to_string(),
        retry: if revoked {
            RemoteRetryClass::Never
        } else {
            RemoteRetryClass::Reconnect
        },
    }
}

impl Drop for RemoteGatewayInner {
    fn drop(&mut self) {
        if let Ok(lifecycle) = self.lifecycle.get_mut()
            && let Some(task) = lifecycle.task.take()
        {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct GatewayState {
    config: Arc<RemoteGatewayConfig>,
    dispatcher: RemoteDispatcher,
    db_path: PathBuf,
    identity: RemoteIdentity,
    tickets: Arc<Mutex<HashMap<String, WsTicketRecord>>>,
    registry: ConnectionRegistry,
    idempotency: Arc<Mutex<HashMap<IdempotencyCacheKey, CachedRpcResponse>>>,
    domain_events: GatewayDomainEvents,
    pairing_routes: Arc<Mutex<RemoteGatewayPairingRoutes>>,
    session_epoch: u64,
}

#[derive(Clone)]
struct WsTicketRecord {
    proof: RemoteAuthProof,
    auth: RemoteAuthContext,
    expires_at_ms: i64,
    proof_challenge: String,
    relay_authenticated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IdempotencyCacheKey {
    device_id: DeviceId,
    key: String,
}

#[derive(Clone)]
struct CachedRpcResponse {
    response: RemoteRpcResponseV2,
    created_at_ms: i64,
    grant_revision: u64,
}

#[derive(Clone, Default)]
struct ConnectionRegistry {
    connections: Arc<Mutex<HashMap<RequestId, ActiveConnection>>>,
}

struct ActiveConnection {
    device_id: DeviceId,
    disconnect: watch::Sender<Option<RemoteCloseReason>>,
}

impl ConnectionRegistry {
    fn register(
        &self,
        connection_id: RequestId,
        device_id: DeviceId,
        max_connections: usize,
    ) -> VibexResult<watch::Receiver<Option<RemoteCloseReason>>> {
        let mut connections = self.connections.lock().map_err(|_| gateway_state_error())?;
        if connections.len() >= max_connections {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_gateway_connection_limit",
                "RemoteGateway connection limit was reached",
            ));
        }
        let (disconnect, receiver) = watch::channel(None);
        connections.insert(
            connection_id,
            ActiveConnection {
                device_id,
                disconnect,
            },
        );
        Ok(receiver)
    }

    fn unregister(&self, connection_id: &RequestId) {
        if let Ok(mut connections) = self.connections.lock() {
            connections.remove(connection_id);
        }
    }

    fn disconnect_device(&self, device_id: &DeviceId, reason: RemoteCloseReason) {
        if let Ok(connections) = self.connections.lock() {
            for connection in connections.values() {
                if &connection.device_id == device_id {
                    let _ = connection.disconnect.send(Some(reason.clone()));
                }
            }
        }
    }

    fn disconnect_all(&self, reason: RemoteCloseReason) {
        if let Ok(connections) = self.connections.lock() {
            for connection in connections.values() {
                let _ = connection.disconnect.send(Some(reason.clone()));
            }
        }
    }

    fn active_count(&self) -> usize {
        self.connections
            .lock()
            .map(|connections| connections.len())
            .unwrap_or_default()
    }
}

fn build_gateway_router(state: GatewayState) -> Router {
    let legacy = build_router_with_dispatcher(state.dispatcher.clone());
    let v2 = Router::new()
        .route("/api/v2/info", get(gateway_info))
        .route("/api/v2/pairing/claim", post(claim_pairing_offer))
        .route("/api/v2/ws-ticket", post(issue_ws_ticket))
        .route("/ws/v2", get(ws_v2))
        .route("/", get(static_index))
        .route("/{*path}", get(static_asset))
        .with_state(state.clone());
    v2.merge(legacy).layer(middleware::from_fn_with_state(
        state.clone(),
        security_perimeter,
    ))
}

async fn gateway_info(State(state): State<GatewayState>) -> Response {
    Json(serde_json::json!({
        "serverId": state.identity.server_id(),
        "serverIdentityPublicKey": state.identity.public_key_base64(),
        "protocolRange": RemoteProtocolVersionRange::v2(),
        "wsPath": "/ws/v2",
        "pairingClaimPath": "/api/v2/pairing/claim",
        "wsTicketPath": "/api/v2/ws-ticket",
        "deploymentMode": match state.config.deployment_mode {
            RemoteGatewayDeploymentMode::Loopback => "loopback",
            RemoteGatewayDeploymentMode::Lan => "lan",
        },
        "tlsPolicy": match state.config.tls_policy {
            RemoteGatewayTlsPolicy::LoopbackHttp => "loopback_http",
            RemoteGatewayTlsPolicy::TrustedHttpsProxy => "trusted_https_proxy",
        },
        "sessionEpoch": state.session_epoch,
        "enabledFeatures": gateway_features(&state),
        "webBuild": state.config.web_build.clone(),
    }))
    .into_response()
}

async fn claim_pairing_offer(
    State(state): State<GatewayState>,
    Json(request): Json<RemoteClaimPairingOfferRequest>,
) -> Response {
    let connection = match open_migrated_database(&state.db_path) {
        Ok(connection) => connection,
        Err(error) => return protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    match RemoteTrustService::claim_pairing_offer(&connection, request) {
        Ok(response) => Json(response).into_response(),
        Err(error) => protocol_error_response(status_for_error(&error), error),
    }
}

async fn issue_ws_ticket(
    State(state): State<GatewayState>,
    Json(request): Json<RemoteWsTicketRequest>,
) -> Response {
    let connection = match open_migrated_database(&state.db_path) {
        Ok(connection) => connection,
        Err(error) => return protocol_error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let auth = match RemoteTrustService::authenticate(&connection, request.auth.clone()) {
        Ok(auth) => auth,
        Err(error) => return protocol_error_response(StatusCode::UNAUTHORIZED, error),
    };
    let ticket = secure_secret("ws");
    let proof_challenge = secure_secret("proof");
    let expires_at_ms = unix_timestamp_ms() + i64::from(state.config.ws_ticket_ttl_ms);
    let mut tickets = match state.tickets.lock() {
        Ok(tickets) => tickets,
        Err(_) => {
            return protocol_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                gateway_state_error(),
            );
        }
    };
    tickets.retain(|_, record| record.expires_at_ms > unix_timestamp_ms());
    tickets.insert(
        ticket.clone(),
        WsTicketRecord {
            proof: request.auth,
            auth,
            expires_at_ms,
            proof_challenge: proof_challenge.clone(),
            relay_authenticated: false,
        },
    );
    Json(RemoteWsTicketResponse {
        subprotocol: format!("{REMOTE_V2_SUBPROTOCOL}, {REMOTE_V2_TICKET_PREFIX}{ticket}"),
        ticket,
        proof_challenge,
        expires_at_ms,
    })
    .into_response()
}

async fn ws_v2(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let protocols = headers
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();
    if !protocols.contains(&REMOTE_V2_SUBPROTOCOL) {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            VibexError::validation(
                "remote_ws_subprotocol_required",
                "RemoteGateway requires the vibex-v2 WebSocket subprotocol",
            ),
        );
    }
    let Some(ticket) = protocols
        .iter()
        .find_map(|protocol| protocol.strip_prefix(REMOTE_V2_TICKET_PREFIX))
    else {
        return protocol_error_response(
            StatusCode::UNAUTHORIZED,
            VibexError::new(
                ErrorCategory::Remote,
                "remote_ws_ticket_required",
                "RemoteGateway requires a one-time WebSocket ticket",
            ),
        );
    };
    let ticket_record = match consume_ws_ticket(&state, ticket) {
        Ok(record) => record,
        Err(error) => return protocol_error_response(StatusCode::UNAUTHORIZED, error),
    };

    upgrade
        .protocols([REMOTE_V2_SUBPROTOCOL])
        .on_upgrade(move |socket| run_v2_socket(socket, state, ticket_record))
        .into_response()
}

fn consume_ws_ticket(state: &GatewayState, ticket: &str) -> VibexResult<WsTicketRecord> {
    let record = state
        .tickets
        .lock()
        .map_err(|_| gateway_state_error())?
        .remove(ticket)
        .ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_ws_ticket_invalid",
                "WebSocket ticket is invalid or already used",
            )
        })?;
    if record.expires_at_ms <= unix_timestamp_ms() {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_ws_ticket_expired",
            "WebSocket ticket has expired",
        ));
    }
    let connection = open_migrated_database(&state.db_path)?;
    let auth = RemoteTrustService::authenticate(&connection, record.proof.clone())?;
    Ok(WsTicketRecord { auth, ..record })
}

#[derive(Debug)]
enum OutboundFrame {
    Text(String),
    Binary(Vec<u8>),
    Close(RemoteCloseReason),
}

async fn run_v2_socket(socket: WebSocket, state: GatewayState, ticket: WsTicketRecord) {
    let (mut writer, mut reader) = socket.split();
    let (outbound_tx, mut outbound_rx) =
        mpsc::channel::<OutboundFrame>(state.config.outbound_queue_capacity);
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            let result = match frame {
                OutboundFrame::Text(text) => writer.send(Message::Text(text.into())).await,
                OutboundFrame::Binary(bytes) => writer.send(Message::Binary(bytes.into())).await,
                OutboundFrame::Close(reason) => {
                    writer
                        .send(Message::Close(Some(CloseFrame {
                            code: websocket_close_code(reason.code),
                            reason: reason.message.into(),
                        })))
                        .await
                }
            };
            if result.is_err() {
                break;
            }
        }
    });

    let hello = match tokio::time::timeout(Duration::from_secs(10), reader.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => decode_hello(text.as_str()),
        _ => Err(VibexError::validation(
            "remote_hello_required",
            "RemoteGateway expected hello as the first WebSocket frame",
        )),
    };
    let hello = match hello {
        Ok(hello) => hello,
        Err(error) => {
            close_with_error(&outbound_tx, error).await;
            finish_writer(outbound_tx, writer_task).await;
            return;
        }
    };
    if hello.device_id != ticket.auth.device_id {
        close_with_reason(
            &outbound_tx,
            RemoteCloseReason {
                code: RemoteCloseCode::AuthenticationRequired,
                message: "hello device identity does not match the WS ticket".to_string(),
                retry: RemoteRetryClass::RefreshAuthentication,
            },
        )
        .await;
        finish_writer(outbound_tx, writer_task).await;
        return;
    }
    let session_crypto = match verify_hello_device_identity(&state, &hello, &ticket) {
        Ok(session_crypto) => session_crypto,
        Err(error) => {
            close_with_error(&outbound_tx, error).await;
            finish_writer(outbound_tx, writer_task).await;
            return;
        }
    };
    let Some(selected_protocol) = RemoteProtocolVersionRange::v2().negotiate(hello.protocol_range)
    else {
        close_with_reason(
            &outbound_tx,
            RemoteCloseReason {
                code: RemoteCloseCode::UnsupportedVersion,
                message: "client and server protocol ranges are incompatible".to_string(),
                retry: RemoteRetryClass::Never,
            },
        )
        .await;
        finish_writer(outbound_tx, writer_task).await;
        return;
    };

    let connection_id = RequestId::new();
    let mut disconnect = match state.registry.register(
        connection_id.clone(),
        ticket.auth.device_id.clone(),
        state.config.max_connections,
    ) {
        Ok(receiver) => receiver,
        Err(error) => {
            close_with_error(&outbound_tx, error).await;
            finish_writer(outbound_tx, writer_task).await;
            return;
        }
    };
    let subscriptions = Arc::new(Mutex::new(HashSet::<String>::new()));
    let server_info = RemoteServerInfoV2 {
        server_id: state.identity.server_id().to_string(),
        server_identity_public_key: state.identity.public_key_base64(),
        desktop_version: state.config.service.server_version.clone(),
        protocol_range: RemoteProtocolVersionRange::v2(),
        selected_protocol,
        server_ephemeral_public_key: session_crypto.server_ephemeral_public_key,
        session_key_confirmation: session_crypto.session_key_confirmation,
        capabilities: gateway_capabilities(),
        enabled_features: gateway_features(&state),
        device_permissions: remote_permissions_for_level(ticket.auth.permission_level),
        session_epoch: state.session_epoch,
        connection_id: connection_id.clone(),
        server_time_ms: unix_timestamp_ms(),
    };
    if send_json(
        &outbound_tx,
        RemoteJsonMessageV2::Control(RemoteControlMessageV2::ServerInfo(server_info)),
    )
    .await
    .is_err()
    {
        state.registry.unregister(&connection_id);
        finish_writer(outbound_tx, writer_task).await;
        return;
    }

    let event_tasks = spawn_gateway_events(
        &state,
        outbound_tx.clone(),
        subscriptions.clone(),
        ticket.auth.permission_level,
    );
    let mut attachment_tasks = HashMap::<String, JoinHandle<()>>::new();
    let mut binary_sequences = HashMap::<(String, u64), u64>::new();
    let rpc_slots = Arc::new(Semaphore::new(
        state.config.max_in_flight_rpcs_per_connection,
    ));

    loop {
        tokio::select! {
            changed = disconnect.changed() => {
                let reason = if changed.is_ok() {
                    disconnect.borrow().clone()
                } else {
                    None
                };
                if let Some(reason) = reason {
                    close_with_reason(&outbound_tx, reason).await;
                }
                break;
            }
            incoming = reader.next() => {
                let Some(incoming) = incoming else { break; };
                let Ok(incoming) = incoming else { break; };
                match incoming {
                    Message::Text(text) => {
                        if !handle_active_text(
                            &state,
                            &ticket,
                            text.as_str(),
                            &outbound_tx,
                            &subscriptions,
                            &mut attachment_tasks,
                            &rpc_slots,
                        ).await {
                            break;
                        }
                    }
                    Message::Binary(bytes) => {
                        if let Err(error) = handle_active_binary(
                            &state,
                            &ticket,
                            bytes.as_ref(),
                            &outbound_tx,
                            &mut binary_sequences,
                        ).await {
                            let response = rpc_error_response(RequestId::new(), None, error);
                            let _ = send_json(
                                &outbound_tx,
                                RemoteJsonMessageV2::RpcResponse(response),
                            ).await;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
        }
    }

    event_tasks.abort();
    for (_, task) in attachment_tasks {
        task.abort();
    }
    state.registry.unregister(&connection_id);
    finish_writer(outbound_tx, writer_task).await;
}

struct SessionCryptoConfirmation {
    server_ephemeral_public_key: String,
    session_key_confirmation: String,
}

fn verify_hello_device_identity(
    state: &GatewayState,
    hello: &RemoteHello,
    ticket: &WsTicketRecord,
) -> VibexResult<SessionCryptoConfirmation> {
    let connection = open_migrated_database(&state.db_path)?;
    let device = vibex_db::RemoteDeviceRepository::get(&connection, &ticket.auth.device_id)?
        .ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_device_unknown",
                "remote device is unknown",
            )
        })?;
    if device.detail.status == vibex_core::RemoteDeviceStatus::Revoked {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_device_revoked",
            "remote device is revoked",
        ));
    }
    let Some(public_key) = device.detail.public_key.as_deref() else {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_device_identity_required",
            "protocol v2 requires a device identity public key",
        ));
    };
    if public_key != hello.device_identity_public_key {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_device_identity_mismatch",
            "hello device identity does not match the paired device",
        ));
    }
    let device_public = decode_x25519_public_key(public_key, "remote_device_identity_key_invalid")?;
    let client_ephemeral = decode_x25519_public_key(
        &hello.client_ephemeral_public_key,
        "remote_client_ephemeral_key_invalid",
    )?;
    let transcript = hello_transcript(
        hello,
        &ticket.proof_challenge,
        state.identity.server_id(),
        state.session_epoch,
    )?;
    let identity_shared = state.identity.private_key().diffie_hellman(&device_public);
    if !identity_shared.was_contributory() {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_device_identity_key_invalid",
            "remote device identity public key is invalid",
        ));
    }
    let identity_key = derive_key(
        identity_shared.as_bytes(),
        b"vibex.remote.v2.identity-proof",
        &transcript,
    )?;
    let supplied_proof = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&hello.identity_proof)
        .map_err(|_| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_device_identity_proof_invalid",
                "hello device identity proof is invalid",
            )
        })?;
    if !verify_authentication_tag(&identity_key, &transcript, &supplied_proof) {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_device_identity_proof_invalid",
            "hello device identity proof is invalid",
        ));
    }

    let server_ephemeral = StaticSecret::random_from_rng(OsRng);
    let server_ephemeral_public = PublicKey::from(&server_ephemeral);
    let ephemeral_shared = server_ephemeral.diffie_hellman(&client_ephemeral);
    if !ephemeral_shared.was_contributory() {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_client_ephemeral_key_invalid",
            "remote client ephemeral public key is invalid",
        ));
    }
    let session_key = derive_key(
        ephemeral_shared.as_bytes(),
        b"vibex.remote.v2.session-key",
        &transcript,
    )?;
    let mut confirmation_message = transcript;
    confirmation_message.extend_from_slice(server_ephemeral_public.as_bytes());
    let confirmation = authentication_tag(&session_key, &confirmation_message)?;
    Ok(SessionCryptoConfirmation {
        server_ephemeral_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(server_ephemeral_public.as_bytes()),
        session_key_confirmation: base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(confirmation),
    })
}

fn decode_x25519_public_key(value: &str, code: &'static str) -> VibexResult<PublicKey> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| {
            VibexError::new(
                ErrorCategory::Remote,
                code,
                "remote X25519 public key is invalid",
            )
        })?;
    let bytes: [u8; 32] = decoded.try_into().map_err(|_| {
        VibexError::new(
            ErrorCategory::Remote,
            code,
            "remote X25519 public key length is invalid",
        )
    })?;
    Ok(PublicKey::from(bytes))
}

fn hello_transcript(
    hello: &RemoteHello,
    proof_challenge: &str,
    server_id: &str,
    session_epoch: u64,
) -> VibexResult<Vec<u8>> {
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
        VibexError::new(
            ErrorCategory::Remote,
            "remote_hello_transcript_invalid",
            "hello transcript could not be encoded",
        )
    })
}

fn derive_key(shared_secret: &[u8], label: &[u8], transcript: &[u8]) -> VibexResult<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(Some(label), shared_secret);
    let mut key = [0u8; 32];
    hkdf.expand(transcript, &mut key).map_err(|_| {
        VibexError::new(
            ErrorCategory::Remote,
            "remote_session_key_derivation_failed",
            "remote session key derivation failed",
        )
    })?;
    Ok(key)
}

fn authentication_tag(key: &[u8], message: &[u8]) -> VibexResult<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| {
        VibexError::new(
            ErrorCategory::Remote,
            "remote_identity_proof_setup_failed",
            "remote identity proof setup failed",
        )
    })?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn verify_authentication_tag(key: &[u8], message: &[u8], supplied: &[u8]) -> bool {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(message);
    mac.verify_slice(supplied).is_ok()
}

fn relay_permission_context_hash(
    device_id: &DeviceId,
    grant_revision: u64,
    permission_level: vibex_core::RemoteDevicePermissionLevel,
    session_epoch: u64,
) -> VibexResult<String> {
    use sha2::Digest as _;
    let bytes = vibex_core::canonical_json_vec(&serde_json::json!({
        "deviceId": device_id,
        "grantRevision": grant_revision,
        "permissionLevel": permission_level,
        "sessionEpoch": session_epoch,
    }))
    .map_err(|_| {
        VibexError::new(
            ErrorCategory::Remote,
            "remote_permission_context_invalid",
            "remote permission context could not be encoded",
        )
    })?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

fn decode_hello(text: &str) -> VibexResult<RemoteHello> {
    let message = serde_json::from_str::<RemoteJsonMessageV2>(text).map_err(|_| {
        VibexError::validation(
            "remote_hello_invalid",
            "RemoteGateway hello frame is invalid",
        )
    })?;
    match message {
        RemoteJsonMessageV2::Control(RemoteControlMessageV2::Hello(hello)) => Ok(hello),
        _ => Err(VibexError::validation(
            "remote_hello_required",
            "RemoteGateway expected hello as the first WebSocket frame",
        )),
    }
}

async fn handle_active_text(
    state: &GatewayState,
    ticket: &WsTicketRecord,
    text: &str,
    outbound: &mpsc::Sender<OutboundFrame>,
    subscriptions: &Arc<Mutex<HashSet<String>>>,
    attachment_tasks: &mut HashMap<String, JoinHandle<()>>,
    rpc_slots: &Arc<Semaphore>,
) -> bool {
    let message = match serde_json::from_str::<RemoteJsonMessageV2>(text) {
        Ok(message) => message,
        Err(_) => {
            close_with_reason(
                outbound,
                RemoteCloseReason {
                    code: RemoteCloseCode::ProtocolError,
                    message: "remote JSON frame is invalid".to_string(),
                    retry: RemoteRetryClass::Never,
                },
            )
            .await;
            return false;
        }
    };
    match message {
        RemoteJsonMessageV2::Control(control) => {
            handle_control(
                state,
                ticket,
                control,
                outbound,
                subscriptions,
                attachment_tasks,
            )
            .await
        }
        RemoteJsonMessageV2::RpcRequest(request) => {
            let permit = match acquire_rpc_slot(rpc_slots, &request) {
                Ok(permit) => permit,
                Err(response) => {
                    let _ = send_json(outbound, RemoteJsonMessageV2::RpcResponse(response)).await;
                    return true;
                }
            };
            let state = state.clone();
            let proof = ticket.proof.clone();
            let outbound = outbound.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let response = process_rpc(&state, &proof, request).await;
                let _ = send_json(&outbound, RemoteJsonMessageV2::RpcResponse(response)).await;
            });
            true
        }
        RemoteJsonMessageV2::Unknown => {
            close_with_reason(
                outbound,
                RemoteCloseReason {
                    code: RemoteCloseCode::ProtocolError,
                    message: "remote JSON frame kind is unknown".to_string(),
                    retry: RemoteRetryClass::Never,
                },
            )
            .await;
            false
        }
        RemoteJsonMessageV2::RpcResponse(_) | RemoteJsonMessageV2::Event(_) => {
            close_with_reason(
                outbound,
                RemoteCloseReason {
                    code: RemoteCloseCode::PolicyViolation,
                    message: "client cannot send response or server event frames".to_string(),
                    retry: RemoteRetryClass::Never,
                },
            )
            .await;
            false
        }
    }
}

#[allow(clippy::result_large_err)]
fn acquire_rpc_slot(
    rpc_slots: &Arc<Semaphore>,
    request: &RemoteRpcRequestV2,
) -> Result<tokio::sync::OwnedSemaphorePermit, RemoteRpcResponseV2> {
    rpc_slots.clone().try_acquire_owned().map_err(|_| {
        rpc_error_response(
            request.request_id.clone(),
            request.correlation_id.clone(),
            VibexError::new(
                ErrorCategory::Remote,
                "remote_rpc_concurrency_limit",
                "remote RPC concurrency limit was reached",
            ),
        )
    })
}

async fn handle_control(
    state: &GatewayState,
    ticket: &WsTicketRecord,
    control: RemoteControlMessageV2,
    outbound: &mpsc::Sender<OutboundFrame>,
    subscriptions: &Arc<Mutex<HashSet<String>>>,
    attachment_tasks: &mut HashMap<String, JoinHandle<()>>,
) -> bool {
    match control {
        RemoteControlMessageV2::Ping(ping) => {
            let _ = send_json(
                outbound,
                RemoteJsonMessageV2::Control(RemoteControlMessageV2::Pong(RemotePing {
                    nonce: ping.nonce,
                    sent_at_ms: unix_timestamp_ms(),
                })),
            )
            .await;
        }
        RemoteControlMessageV2::Pong(_) => {}
        RemoteControlMessageV2::Subscribe(request) => {
            let accepted =
                update_subscriptions(subscriptions, request, ticket.auth.permission_level);
            let _ = send_json(
                outbound,
                RemoteJsonMessageV2::Control(RemoteControlMessageV2::Subscribed(accepted)),
            )
            .await;
        }
        RemoteControlMessageV2::Attach(request) => {
            let action = match request.kind {
                RemoteAttachmentKind::AgentTimeline => Some(RemoteActionClass::ReadAgentSession),
                RemoteAttachmentKind::Terminal
                | RemoteAttachmentKind::FileTransfer
                | RemoteAttachmentKind::Git => Some(RemoteActionClass::ReadProject),
                RemoteAttachmentKind::Provider => Some(RemoteActionClass::ReadProviderSettings),
                RemoteAttachmentKind::Unknown => None,
            };
            let Some(action) = action else {
                let _ = send_json(
                    outbound,
                    RemoteJsonMessageV2::Control(RemoteControlMessageV2::ResyncRequired(
                        RemoteResyncRequired {
                            domain: request.attachment_id,
                            generation: state.session_epoch,
                            reason: "attachment kind is not supported".to_string(),
                            authoritative_operation: "info".to_string(),
                        },
                    )),
                )
                .await;
                return true;
            };
            if let Err(error) = authorize_live_action(state, ticket, action) {
                let response = rpc_error_response(RequestId::new(), None, error);
                let _ = send_json(outbound, RemoteJsonMessageV2::RpcResponse(response)).await;
                return true;
            }
            if let Some(task) = attachment_tasks.remove(&request.attachment_id) {
                task.abort();
            }
            let mut accepted = RemoteAttachmentAcceptedV2 {
                attachment_id: request.attachment_id.clone(),
                generation: state.session_epoch,
                next_sequence: request.after_sequence.max(1),
                snapshot_required: request.generation != state.session_epoch,
            };
            if request.kind == RemoteAttachmentKind::Terminal {
                match terminal_manager(&state.dispatcher) {
                    Some(manager) => {
                        if let Ok(terminal_id) = TerminalId::parse(request.resource_id.clone()) {
                            if let Ok(snapshot) = manager.raw_snapshot_from(
                                &terminal_id,
                                i64::try_from(request.after_sequence.max(1)).unwrap_or(i64::MAX),
                            ) {
                                if request.scope_id.as_deref()
                                    != Some(snapshot.session.workspace_id.as_str())
                                {
                                    let response = rpc_error_response(
                                        RequestId::new(),
                                        None,
                                        VibexError::new(
                                            ErrorCategory::Permission,
                                            "remote_terminal_scope_mismatch",
                                            "terminal attachment does not belong to the requested workspace",
                                        ),
                                    );
                                    let _ = send_json(
                                        outbound,
                                        RemoteJsonMessageV2::RpcResponse(response),
                                    )
                                    .await;
                                    return true;
                                }
                                accepted.next_sequence =
                                    u64::try_from(snapshot.next_sequence).unwrap_or(u64::MAX);
                                let first = snapshot
                                    .chunks
                                    .first()
                                    .and_then(|chunk| u64::try_from(chunk.sequence).ok());
                                accepted.snapshot_required |= request.after_sequence
                                    > accepted.next_sequence
                                    || first.is_some_and(|sequence| {
                                        sequence > request.after_sequence.max(1)
                                    });
                            }
                            let task = spawn_terminal_stream(
                                manager,
                                terminal_id,
                                request.resource_id.clone(),
                                request.after_sequence,
                                state.session_epoch,
                                outbound.clone(),
                            );
                            attachment_tasks.insert(request.attachment_id.clone(), task);
                        } else {
                            accepted.snapshot_required = true;
                        }
                    }
                    None => accepted.snapshot_required = true,
                }
            } else if request.kind == RemoteAttachmentKind::FileTransfer {
                let (workspace_id, bytes) = match prepare_file_download(
                    state,
                    request.scope_id.as_deref(),
                    &request.resource_id,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        let response = rpc_error_response(RequestId::new(), None, error);
                        let _ =
                            send_json(outbound, RemoteJsonMessageV2::RpcResponse(response)).await;
                        return true;
                    }
                };
                let task = spawn_file_download_stream(
                    bytes,
                    workspace_id,
                    request.attachment_id.clone(),
                    state.session_epoch,
                    request.after_sequence,
                    outbound.clone(),
                );
                attachment_tasks.insert(request.attachment_id.clone(), task);
            }
            let _ = send_json(
                outbound,
                RemoteJsonMessageV2::Control(RemoteControlMessageV2::Attached(accepted)),
            )
            .await;
        }
        RemoteControlMessageV2::Detach(request) => {
            if let Some(task) = attachment_tasks.remove(&request.attachment_id) {
                task.abort();
            }
            let _ = send_json(
                outbound,
                RemoteJsonMessageV2::Control(RemoteControlMessageV2::Detached(request)),
            )
            .await;
        }
        RemoteControlMessageV2::Close(_) => return false,
        RemoteControlMessageV2::Hello(_)
        | RemoteControlMessageV2::ServerInfo(_)
        | RemoteControlMessageV2::Subscribed(_)
        | RemoteControlMessageV2::Attached(_)
        | RemoteControlMessageV2::Detached(_)
        | RemoteControlMessageV2::ResyncRequired(_)
        | RemoteControlMessageV2::Unknown => {
            close_with_reason(
                outbound,
                RemoteCloseReason {
                    code: RemoteCloseCode::ProtocolError,
                    message: "control frame is invalid in the active connection state".to_string(),
                    retry: RemoteRetryClass::Never,
                },
            )
            .await;
            return false;
        }
    }
    true
}

fn authorize_live_action(
    state: &GatewayState,
    ticket: &WsTicketRecord,
    action: RemoteActionClass,
) -> VibexResult<RemoteAuthContext> {
    let auth = authenticate_active_ticket(state, ticket)?;
    let connection = open_migrated_database(&state.db_path)?;
    RemoteTrustService::authorize_action(&connection, &auth, action, None, None)?;
    Ok(auth)
}

fn authenticate_active_ticket(
    state: &GatewayState,
    ticket: &WsTicketRecord,
) -> VibexResult<RemoteAuthContext> {
    let connection = open_migrated_database(&state.db_path)?;
    if !ticket.relay_authenticated {
        return RemoteTrustService::authenticate(&connection, ticket.proof.clone());
    }
    let device = vibex_db::RemoteDeviceRepository::get(&connection, &ticket.auth.device_id)?
        .ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_device_unknown",
                "remote device is unknown",
            )
        })?;
    if device.detail.status == vibex_core::RemoteDeviceStatus::Revoked {
        return Err(VibexError::new(
            ErrorCategory::Remote,
            "remote_device_revoked",
            "remote device is revoked",
        ));
    }
    Ok(RemoteAuthContext {
        device_id: device.detail.device_id,
        display_name: device.detail.display_name,
        permission_level: device.detail.permission_level,
        authenticated_at_ms: unix_timestamp_ms(),
    })
}

fn update_subscriptions(
    subscriptions: &Arc<Mutex<HashSet<String>>>,
    request: RemoteSubscribeRequestV2,
    permission_level: RemoteDevicePermissionLevel,
) -> RemoteSubscriptionAcceptedV2 {
    let supported = gateway_topics();
    let mut accepted = Vec::new();
    let mut resync_required = Vec::new();
    if let Ok(mut active) = subscriptions.lock() {
        for topic in request.topics {
            if supported.contains(&topic.as_str()) {
                if !subscription_topic_permitted(permission_level, &topic) {
                    continue;
                }
                if let Some(cursor) = request.cursors.iter().find(|cursor| cursor.domain == topic)
                    && cursor.generation != 0
                {
                    resync_required.push(RemoteResyncRequired {
                        domain: topic.clone(),
                        generation: cursor.generation,
                        reason: "route handoff requires an authoritative projection refresh"
                            .to_string(),
                        authoritative_operation: topic.clone(),
                    });
                }
                active.insert(topic.clone());
                accepted.push(topic);
            } else {
                resync_required.push(RemoteResyncRequired {
                    domain: topic,
                    generation: 0,
                    reason: "topic is not supported by this server".to_string(),
                    authoritative_operation: "info".to_string(),
                });
            }
        }
    }
    RemoteSubscriptionAcceptedV2 {
        subscription_id: request.subscription_id,
        topics: accepted,
        resync_required,
    }
}

struct GatewayEventTasks {
    timeline: JoinHandle<()>,
    domains: JoinHandle<()>,
}

impl GatewayEventTasks {
    fn abort(self) {
        self.timeline.abort();
        self.domains.abort();
    }
}

fn spawn_gateway_events(
    state: &GatewayState,
    outbound: mpsc::Sender<OutboundFrame>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    permission_level: RemoteDevicePermissionLevel,
) -> GatewayEventTasks {
    GatewayEventTasks {
        timeline: spawn_timeline_events(
            state,
            outbound.clone(),
            subscriptions.clone(),
            permission_level,
        ),
        domains: spawn_domain_events(state, outbound, subscriptions, permission_level),
    }
}

fn spawn_timeline_events(
    state: &GatewayState,
    outbound: mpsc::Sender<OutboundFrame>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    permission_level: RemoteDevicePermissionLevel,
) -> JoinHandle<()> {
    if !permission_allows(permission_level, RemoteActionClass::ReadAgentSession) {
        return tokio::spawn(async {});
    }
    let Some(agent_manager) = state.dispatcher.state.agent_manager.clone() else {
        return tokio::spawn(async {});
    };
    let mut events = agent_manager.subscribe();
    let generation = state.session_epoch;
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let subscribed = subscriptions
                        .lock()
                        .map(|topics| topics.contains("agent_session"))
                        .unwrap_or(false);
                    if !subscribed {
                        continue;
                    }
                    let Ok(event) = remote_timeline_event(event) else {
                        continue;
                    };
                    let message = RemoteJsonMessageV2::Event(RemoteEventV2 {
                        event_id: event.event_id,
                        channel: "agent_session".to_string(),
                        generation,
                        sequence: event.sequence,
                        correlation_id: event.correlation_id,
                        payload: event.payload,
                        emitted_at_ms: event.emitted_at_ms,
                    });
                    if send_json(&outbound, message).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = send_json(
                        &outbound,
                        RemoteJsonMessageV2::Control(RemoteControlMessageV2::ResyncRequired(
                            RemoteResyncRequired {
                                domain: "agent_session".to_string(),
                                generation,
                                reason: "live event queue lagged".to_string(),
                                authoritative_operation: "agent_session".to_string(),
                            },
                        )),
                    )
                    .await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn spawn_domain_events(
    state: &GatewayState,
    outbound: mpsc::Sender<OutboundFrame>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
    permission_level: RemoteDevicePermissionLevel,
) -> JoinHandle<()> {
    let mut events = state.domain_events.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let subscribed = subscriptions
                        .lock()
                        .map(|topics| topics.contains(&event.channel))
                        .unwrap_or(false);
                    if subscribed
                        && domain_event_permitted(permission_level, &event.channel)
                        && send_json(&outbound, RemoteJsonMessageV2::Event(event))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let active = subscriptions
                        .lock()
                        .map(|topics| {
                            ["file", "git", "provider", "device"]
                                .into_iter()
                                .filter(|topic| {
                                    topics.contains(*topic)
                                        && domain_event_permitted(permission_level, topic)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    for domain in active {
                        if send_json(
                            &outbound,
                            RemoteJsonMessageV2::Control(RemoteControlMessageV2::ResyncRequired(
                                RemoteResyncRequired {
                                    domain: domain.to_string(),
                                    generation: 0,
                                    reason: "domain event queue lagged".to_string(),
                                    authoritative_operation: domain.to_string(),
                                },
                            )),
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn permission_allows(
    permission_level: RemoteDevicePermissionLevel,
    action: RemoteActionClass,
) -> bool {
    remote_permissions_for_level(permission_level).contains(&action)
}

fn subscription_topic_permitted(
    permission_level: RemoteDevicePermissionLevel,
    topic: &str,
) -> bool {
    let action = match topic {
        "agent_session" | "runtime" => RemoteActionClass::ReadAgentSession,
        "terminal" | "file" | "git" => RemoteActionClass::ReadProject,
        "provider" => RemoteActionClass::ReadProviderSettings,
        "device" => RemoteActionClass::ReadDeviceManagement,
        _ => return false,
    };
    permission_allows(permission_level, action)
}

fn domain_event_permitted(permission_level: RemoteDevicePermissionLevel, channel: &str) -> bool {
    let action = match channel {
        "file" | "git" => RemoteActionClass::ReadProject,
        "provider" => RemoteActionClass::ReadProviderSettings,
        "device" => RemoteActionClass::ReadDeviceManagement,
        _ => return false,
    };
    permission_allows(permission_level, action)
}

fn spawn_terminal_stream(
    manager: TerminalManager,
    terminal_id: TerminalId,
    stream_id: String,
    after_sequence: u64,
    generation: u64,
    outbound: mpsc::Sender<OutboundFrame>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut next_sequence = after_sequence.max(1);
        let mut dropped_chunks = 0;
        let mut interval = tokio::time::interval(Duration::from_millis(40));
        loop {
            interval.tick().await;
            let requested = i64::try_from(next_sequence).unwrap_or(i64::MAX);
            let snapshot = match manager.raw_snapshot_from(&terminal_id, requested) {
                Ok(snapshot) => snapshot,
                Err(_) => break,
            };
            let first_sequence = snapshot
                .chunks
                .first()
                .and_then(|chunk| u64::try_from(chunk.sequence).ok());
            let reset = next_sequence > u64::try_from(snapshot.next_sequence).unwrap_or_default()
                || first_sequence.is_some_and(|sequence| sequence > next_sequence)
                || snapshot.dropped_chunks > dropped_chunks;
            dropped_chunks = snapshot.dropped_chunks;
            for (index, chunk) in snapshot.chunks.into_iter().enumerate() {
                let Ok(sequence) = u64::try_from(chunk.sequence) else {
                    continue;
                };
                let frame = RemoteBinaryFrame {
                    header: RemoteBinaryFrameHeader {
                        protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                        kind: RemoteBinaryFrameKind::TerminalOutput,
                        stream_id: stream_id.clone(),
                        request_id: None,
                        generation,
                        sequence,
                        offset: 0,
                        total_size: None,
                        snapshot: reset && index == 0,
                        end_of_stream: false,
                        checksum_sha256: None,
                        payload_length: 0,
                    },
                    payload: chunk.data,
                };
                let Ok(encoded) = frame.encode() else {
                    continue;
                };
                if outbound.send(OutboundFrame::Binary(encoded)).await.is_err() {
                    return;
                }
                next_sequence = sequence.saturating_add(1);
            }
        }
    })
}

fn spawn_relay_terminal_stream(
    manager: TerminalManager,
    terminal_id: TerminalId,
    stream_id: String,
    after_sequence: u64,
    generation: u64,
    outbound: mpsc::Sender<RelayRemoteOutbound>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut next_sequence = after_sequence.max(1);
        let mut dropped_chunks = 0;
        let mut interval = tokio::time::interval(Duration::from_millis(40));
        loop {
            interval.tick().await;
            let requested = i64::try_from(next_sequence).unwrap_or(i64::MAX);
            let snapshot = match manager.raw_snapshot_from(&terminal_id, requested) {
                Ok(snapshot) => snapshot,
                Err(_) => break,
            };
            let first_sequence = snapshot
                .chunks
                .first()
                .and_then(|chunk| u64::try_from(chunk.sequence).ok());
            let reset = next_sequence > u64::try_from(snapshot.next_sequence).unwrap_or_default()
                || first_sequence.is_some_and(|sequence| sequence > next_sequence)
                || snapshot.dropped_chunks > dropped_chunks;
            dropped_chunks = snapshot.dropped_chunks;
            for (index, chunk) in snapshot.chunks.into_iter().enumerate() {
                let Ok(sequence) = u64::try_from(chunk.sequence) else {
                    continue;
                };
                let frame = RemoteBinaryFrame {
                    header: RemoteBinaryFrameHeader {
                        protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                        kind: RemoteBinaryFrameKind::TerminalOutput,
                        stream_id: stream_id.clone(),
                        request_id: None,
                        generation,
                        sequence,
                        offset: 0,
                        total_size: None,
                        snapshot: reset && index == 0,
                        end_of_stream: false,
                        checksum_sha256: None,
                        payload_length: 0,
                    },
                    payload: chunk.data,
                };
                let Ok(encoded) = frame.encode() else {
                    continue;
                };
                if outbound
                    .send(RelayRemoteOutbound::Binary(encoded))
                    .await
                    .is_err()
                {
                    return;
                }
                next_sequence = sequence.saturating_add(1);
            }
        }
    })
}

async fn handle_active_binary(
    state: &GatewayState,
    ticket: &WsTicketRecord,
    encoded: &[u8],
    outbound: &mpsc::Sender<OutboundFrame>,
    sequences: &mut HashMap<(String, u64), u64>,
) -> VibexResult<()> {
    let frame = RemoteBinaryFrame::decode(encoded)?;
    let key = (frame.header.stream_id.clone(), frame.header.generation);
    if let Some(previous) = sequences.get(&key) {
        if frame.header.sequence <= *previous {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_binary_replay_detected",
                "remote binary frame sequence was already processed",
            ));
        }
        if frame.header.sequence != previous.saturating_add(1) {
            return Err(VibexError::new(
                ErrorCategory::Remote,
                "remote_binary_sequence_gap",
                "remote binary frame sequence has a gap",
            ));
        }
    }

    match frame.header.kind {
        RemoteBinaryFrameKind::TerminalInput => {
            let terminal_id = TerminalId::parse(frame.header.stream_id.clone())?;
            if frame.header.generation != state.session_epoch {
                return Err(VibexError::conflict(
                    "remote_binary_generation_stale",
                    "remote binary frame belongs to a stale connection generation",
                ));
            }
            let connection = open_migrated_database(&state.db_path)?;
            let auth = authenticate_active_ticket(state, ticket)?;
            RemoteTrustService::authorize_action(
                &connection,
                &auth,
                RemoteActionClass::MutateTerminal,
                frame.header.request_id.clone(),
                None,
            )?;
            let write_result = terminal_manager(&state.dispatcher)
                .ok_or_else(|| {
                    VibexError::capability(
                        "remote_terminal_unavailable",
                        "remote Terminal runtime is unavailable",
                    )
                })?
                .write_bytes(&terminal_id, &frame.payload);
            audit_terminal_binary_input(
                &connection,
                &auth,
                &terminal_id,
                frame.header.request_id.clone(),
                write_result.is_ok(),
            )?;
            write_result?;
            sequences.insert(key, frame.header.sequence);
            if let Some(request_id) = frame.header.request_id {
                let response = RemoteRpcResponseV2 {
                    request_id,
                    correlation_id: None,
                    payload: Some(serde_json::json!({
                        "written": true,
                        "sequence": frame.header.sequence,
                    })),
                    error: None,
                    metadata: RemoteRpcResultMetadata {
                        generation: Some(frame.header.generation),
                        cursor: Some(frame.header.sequence),
                        ..RemoteRpcResultMetadata::default()
                    },
                    completed_at_ms: unix_timestamp_ms(),
                };
                let _ = send_json(outbound, RemoteJsonMessageV2::RpcResponse(response)).await;
            }
            Ok(())
        }
        RemoteBinaryFrameKind::FileUploadChunk => Err(VibexError::capability(
            "remote_file_binary_upload_not_enabled",
            "binary file upload is not enabled by this server",
        )),
        RemoteBinaryFrameKind::TerminalOutput
        | RemoteBinaryFrameKind::TerminalSnapshot
        | RemoteBinaryFrameKind::FileDownloadChunk
        | RemoteBinaryFrameKind::FileSnapshot
        | RemoteBinaryFrameKind::Unknown => Err(VibexError::validation(
            "remote_binary_direction_invalid",
            "client sent a server-only or unknown binary frame kind",
        )),
    }
}

fn audit_terminal_binary_input(
    connection: &vibex_db::DbConnection,
    auth: &RemoteAuthContext,
    terminal_id: &TerminalId,
    request_id: Option<RequestId>,
    succeeded: bool,
) -> VibexResult<()> {
    RemoteTrustService::insert_audit(
        connection,
        Some(auth.device_id.clone()),
        if succeeded {
            vibex_core::RemoteAuditAction::MutationAllowed
        } else {
            vibex_core::RemoteAuditAction::MutationDenied
        },
        vibex_core::RemoteAuditTargetKind::Terminal,
        Some(terminal_id.as_str().to_string()),
        if succeeded {
            vibex_core::RemoteAuditOutcome::Allowed
        } else {
            vibex_core::RemoteAuditOutcome::Failed
        },
        "Terminal binary input",
        request_id,
        None,
    )
}

async fn process_rpc(
    state: &GatewayState,
    proof: &RemoteAuthProof,
    request: RemoteRpcRequestV2,
) -> RemoteRpcResponseV2 {
    let request_id = request.request_id.clone();
    let correlation_id = request.correlation_id.clone();
    match process_rpc_inner(state, proof, request).await {
        Ok(response) => response,
        Err(error) => rpc_error_response(request_id, correlation_id, error),
    }
}

async fn process_rpc_inner(
    state: &GatewayState,
    proof: &RemoteAuthProof,
    mut request: RemoteRpcRequestV2,
) -> VibexResult<RemoteRpcResponseV2> {
    let operation = operation_from_wire(&request.operation).ok_or_else(|| {
        VibexError::capability(
            "remote_unsupported_operation",
            "remote RPC operation is not supported",
        )
        .with_diagnostic("operation", bounded_diagnostic(&request.operation))
    })?;
    let requires_idempotency = request
        .payload
        .as_ref()
        .and_then(|payload| payload.get("type"))
        .and_then(|kind| kind.as_str())
        .is_some_and(mutation_requires_idempotency);
    let event_domain = requires_idempotency
        .then(|| mutation_event_domain(operation))
        .flatten();
    let event_correlation_id = request.correlation_id.clone();
    if requires_idempotency && request.mutation.is_none() {
        return Err(VibexError::validation(
            "remote_idempotency_key_required",
            "remote mutation requires an idempotency key",
        ));
    }
    let cache_key = validate_mutation_contract(
        &mut request.payload,
        request.mutation.as_ref(),
        &proof.device_id,
    )?;
    if operation == RemoteOperationKind::DeviceManagement {
        inject_auth(&mut request.payload, proof)?;
        let (response, fresh) = process_device_management_rpc(
            state,
            request.request_id,
            request.correlation_id,
            request.payload,
            cache_key.as_ref(),
        )?;
        if fresh && let Some(cache_key) = cache_key {
            store_cached_rpc_response(state, cache_key, response.clone())?;
        }
        if fresh && let Some(domain) = event_domain {
            state
                .domain_events
                .publish(domain, state.session_epoch, event_correlation_id)?;
        }
        return Ok(response);
    }
    if let Some(cache_key) = &cache_key
        && let Some(mut cached) = cached_rpc_response(state, cache_key, proof)?
    {
        cached.request_id = request.request_id;
        cached.correlation_id = request.correlation_id;
        return Ok(cached);
    }
    inject_auth(&mut request.payload, proof)?;
    let legacy = RemoteRequestEnvelope {
        protocol_version: RemoteProtocolVersion::foundation(),
        request_id: request.request_id.clone(),
        correlation_id: request.correlation_id.clone(),
        device_id: Some(proof.device_id.clone()),
        operation,
        created_at_ms: request.created_at_ms,
        payload: request.payload,
    };
    let timeout = match request.timeout_class {
        RemoteTimeoutClass::Interactive => Duration::from_secs(10),
        RemoteTimeoutClass::Standard | RemoteTimeoutClass::Unknown => Duration::from_secs(30),
        RemoteTimeoutClass::LongRunning => Duration::from_secs(120),
    };
    let legacy_response = tokio::time::timeout(timeout, state.dispatcher.dispatch(legacy))
        .await
        .map_err(|_| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_rpc_timeout",
                "remote RPC exceeded its timeout class",
            )
        })?;
    let response = response_from_legacy(legacy_response);
    if let Some(cache_key) = cache_key {
        store_cached_rpc_response(state, cache_key, response.clone())?;
    }
    if response.error.is_none()
        && let Some(domain) = event_domain
    {
        state
            .domain_events
            .publish(domain, state.session_epoch, event_correlation_id)?;
    }
    Ok(response)
}

fn process_device_management_rpc(
    state: &GatewayState,
    request_id: RequestId,
    correlation_id: Option<vibex_core::CorrelationId>,
    payload: Option<serde_json::Value>,
    cache_key: Option<&IdempotencyCacheKey>,
) -> VibexResult<(RemoteRpcResponseV2, bool)> {
    let request = payload
        .ok_or_else(|| {
            VibexError::validation(
                "remote_device_payload_missing",
                "remote device management requires a payload",
            )
        })
        .and_then(|payload| {
            serde_json::from_value::<RemoteDeviceRequest>(payload).map_err(|_| {
                VibexError::validation(
                    "remote_device_payload_invalid",
                    "remote device management payload is invalid",
                )
            })
        })?;
    let connection = open_migrated_database(&state.db_path)?;
    let (proof, action) = match &request {
        RemoteDeviceRequest::CreatePairingOffer(request) => (
            request.auth.clone(),
            RemoteActionClass::MutateDeviceManagement,
        ),
        RemoteDeviceRequest::CancelPairingOffer(request) => (
            request.auth.clone(),
            RemoteActionClass::MutateDeviceManagement,
        ),
        RemoteDeviceRequest::ListDevices(request) => (
            request.auth.clone(),
            RemoteActionClass::ReadDeviceManagement,
        ),
        RemoteDeviceRequest::RevokeDevice(request) => (
            request.auth.clone(),
            RemoteActionClass::MutateDeviceManagement,
        ),
    };
    let auth = authorize_device_management(
        &connection,
        &proof,
        action,
        &request_id,
        correlation_id.as_ref(),
    )?;
    if let Some(cache_key) = cache_key
        && let Some(mut cached) = cached_rpc_response(state, cache_key, &proof)?
    {
        cached.request_id = request_id;
        cached.correlation_id = correlation_id;
        return Ok((cached, false));
    }
    let response = match request {
        RemoteDeviceRequest::CreatePairingOffer(request) => {
            serde_json::to_value(create_pairing_offer_with_routes(
                &connection,
                &state.identity,
                request.request,
                &state.pairing_routes,
            )?)
        }
        RemoteDeviceRequest::CancelPairingOffer(request) => serde_json::to_value(
            RemoteTrustService::cancel_pairing_offer(&connection, request.request)?,
        ),
        RemoteDeviceRequest::ListDevices(request) => {
            let _ = request;
            let devices = vibex_db::RemoteDeviceRepository::list(&connection)?
                .into_iter()
                .map(|record| {
                    let mut detail = record.detail;
                    detail.public_key = None;
                    detail
                })
                .collect();
            serde_json::to_value(RemoteDeviceListResponse { devices })
        }
        RemoteDeviceRequest::RevokeDevice(request) => {
            if auth.device_id == request.request.device_id {
                return Err(VibexError::new(
                    ErrorCategory::Permission,
                    "remote_device_self_revoke_forbidden",
                    "a remote device cannot revoke its own active connection",
                ));
            }
            let device_id = request.request.device_id.clone();
            let mut detail = RemoteTrustService::revoke_device(&connection, request.request)?;
            detail.public_key = None;
            state.registry.disconnect_device(
                &device_id,
                RemoteCloseReason {
                    code: RemoteCloseCode::DeviceRevoked,
                    message: "remote device was revoked".to_string(),
                    retry: RemoteRetryClass::Never,
                },
            );
            serde_json::to_value(detail)
        }
    }
    .map_err(|_| {
        VibexError::validation(
            "remote_device_payload_encode_failed",
            "remote device management response could not be encoded",
        )
    })?;
    Ok((
        RemoteRpcResponseV2 {
            request_id,
            correlation_id,
            payload: Some(response),
            error: None,
            metadata: RemoteRpcResultMetadata::default(),
            completed_at_ms: unix_timestamp_ms(),
        },
        true,
    ))
}

fn authorize_device_management(
    connection: &vibex_db::DbConnection,
    proof: &RemoteAuthProof,
    action: RemoteActionClass,
    request_id: &RequestId,
    correlation_id: Option<&vibex_core::CorrelationId>,
) -> VibexResult<RemoteAuthContext> {
    let auth = RemoteTrustService::authenticate(connection, proof.clone())?;
    RemoteTrustService::authorize_action(
        connection,
        &auth,
        action,
        Some(request_id.clone()),
        correlation_id.cloned(),
    )?;
    Ok(auth)
}

fn validate_mutation_contract(
    payload: &mut Option<serde_json::Value>,
    mutation: Option<&RemoteMutationContract>,
    device_id: &DeviceId,
) -> VibexResult<Option<IdempotencyCacheKey>> {
    let Some(mutation) = mutation else {
        return Ok(None);
    };
    let key = mutation.idempotency_key.trim();
    if key.is_empty() || key.len() > 128 || key.chars().any(|character| character.is_control()) {
        return Err(VibexError::validation(
            "remote_idempotency_key_invalid",
            "remote idempotency key must be non-empty and bounded",
        ));
    }
    if let Some(expected_revision) = mutation
        .expected_revision
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let request_object = payload
            .as_mut()
            .and_then(|payload| payload.get_mut("data"))
            .and_then(|data| data.get_mut("request"))
            .and_then(serde_json::Value::as_object_mut);
        if let Some(request_object) = request_object {
            if let Some(existing) = request_object
                .get("expectedRevision")
                .and_then(|value| value.as_str())
                && existing != expected_revision
            {
                return Err(VibexError::conflict(
                    "remote_revision_contract_mismatch",
                    "RPC expected revision conflicts with its typed payload",
                ));
            }
            request_object.insert(
                "expectedRevision".to_string(),
                serde_json::Value::String(expected_revision.to_string()),
            );
        }
    }
    Ok(Some(IdempotencyCacheKey {
        device_id: device_id.clone(),
        key: key.to_string(),
    }))
}

fn inject_auth(
    payload: &mut Option<serde_json::Value>,
    proof: &RemoteAuthProof,
) -> VibexResult<()> {
    let auth = serde_json::to_value(proof).map_err(|_| {
        VibexError::validation(
            "remote_auth_injection_failed",
            "remote authenticated request could not be constructed",
        )
    })?;
    let data = payload
        .as_mut()
        .and_then(|payload| payload.get_mut("data"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            VibexError::validation(
                "remote_rpc_payload_invalid",
                "remote RPC payload must be a typed request object",
            )
        })?;
    data.insert("auth".to_string(), auth);
    Ok(())
}

fn response_from_legacy(response: RemoteResponseEnvelope) -> RemoteRpcResponseV2 {
    let metadata = metadata_from_payload(response.payload.as_ref());
    RemoteRpcResponseV2 {
        request_id: response.request_id,
        correlation_id: response.correlation_id,
        payload: response.payload,
        error: response.error.map(RemoteProtocolError::from_error),
        metadata,
        completed_at_ms: response.completed_at_ms,
    }
}

fn metadata_from_payload(payload: Option<&serde_json::Value>) -> RemoteRpcResultMetadata {
    fn find<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        match value {
            serde_json::Value::Object(object) => object
                .get(key)
                .or_else(|| object.values().find_map(|value| find(value, key))),
            serde_json::Value::Array(values) => values.iter().find_map(|value| find(value, key)),
            _ => None,
        }
    }
    RemoteRpcResultMetadata {
        revision: payload
            .and_then(|payload| find(payload, "contentRevision"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        generation: payload
            .and_then(|payload| find(payload, "generation"))
            .and_then(|value| value.as_u64()),
        cursor: payload
            .and_then(|payload| find(payload, "nextSequence"))
            .and_then(|value| value.as_u64()),
        resync_required: payload
            .and_then(|payload| find(payload, "resyncRequired"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    }
}

fn cached_rpc_response(
    state: &GatewayState,
    key: &IdempotencyCacheKey,
    proof: &RemoteAuthProof,
) -> VibexResult<Option<RemoteRpcResponseV2>> {
    let connection = open_migrated_database(&state.db_path)?;
    let auth = RemoteTrustService::authenticate(&connection, proof.clone())?;
    if auth.device_id != key.device_id {
        return Err(VibexError::new(
            ErrorCategory::Permission,
            "remote_idempotency_device_mismatch",
            "remote idempotency key belongs to another device",
        ));
    }
    let grant_revision = vibex_db::RemoteDeviceRepository::get(&connection, &auth.device_id)?
        .ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_device_unknown",
                "remote device is unknown",
            )
        })?
        .detail
        .grant_revision;
    let now = unix_timestamp_ms();
    let mut cache = state
        .idempotency
        .lock()
        .map_err(|_| gateway_state_error())?;
    cache.retain(|_, value| now - value.created_at_ms <= IDEMPOTENCY_CACHE_TTL_MS);
    Ok(cache
        .get(key)
        .filter(|cached| cached.grant_revision == grant_revision)
        .map(|cached| cached.response.clone()))
}

fn store_cached_rpc_response(
    state: &GatewayState,
    key: IdempotencyCacheKey,
    response: RemoteRpcResponseV2,
) -> VibexResult<()> {
    let connection = open_migrated_database(&state.db_path)?;
    let grant_revision = vibex_db::RemoteDeviceRepository::get(&connection, &key.device_id)?
        .ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Remote,
                "remote_device_unknown",
                "remote device is unknown",
            )
        })?
        .detail
        .grant_revision;
    let mut cache = state
        .idempotency
        .lock()
        .map_err(|_| gateway_state_error())?;
    if cache.len() >= MAX_IDEMPOTENCY_CACHE_ENTRIES
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, value)| value.created_at_ms)
            .map(|(key, _)| key.clone())
    {
        cache.remove(&oldest);
    }
    cache.insert(
        key,
        CachedRpcResponse {
            response,
            created_at_ms: unix_timestamp_ms(),
            grant_revision,
        },
    );
    Ok(())
}

fn operation_from_wire(operation: &str) -> Option<RemoteOperationKind> {
    match operation {
        "agent_session" => Some(RemoteOperationKind::AgentSession),
        "workspace_file" => Some(RemoteOperationKind::WorkspaceFile),
        "git" => Some(RemoteOperationKind::Git),
        "terminal" => Some(RemoteOperationKind::Terminal),
        "provider_settings" => Some(RemoteOperationKind::ProviderSettings),
        "device_management" => Some(RemoteOperationKind::DeviceManagement),
        _ => None,
    }
}

fn mutation_requires_idempotency(kind: &str) -> bool {
    matches!(
        kind,
        "open_workspace"
            | "set_desired_runtime"
            | "cancel_runtime_switch"
            | "send_message"
            | "continue_turn"
            | "interrupt"
            | "resolve_permission"
            | "attach_runtime"
            | "detach_runtime"
            | "file_write"
            | "file_delete"
            | "file_rename"
            | "git_stage"
            | "git_unstage"
            | "git_revert"
            | "git_commit"
            | "git_branch_create"
            | "git_branch_checkout"
            | "git_remote_action"
            | "terminal_create"
            | "terminal_write"
            | "terminal_resize"
            | "terminal_kill"
            | "run_health_probes"
            | "create_pairing_offer"
            | "cancel_pairing_offer"
            | "revoke_device"
    )
}

fn mutation_event_domain(operation: RemoteOperationKind) -> Option<&'static str> {
    match operation {
        RemoteOperationKind::WorkspaceFile => Some("file"),
        RemoteOperationKind::Git => Some("git"),
        RemoteOperationKind::ProviderSettings => Some("provider"),
        RemoteOperationKind::DeviceManagement => Some("device"),
        RemoteOperationKind::Handshake
        | RemoteOperationKind::Health
        | RemoteOperationKind::Info
        | RemoteOperationKind::CatchUp
        | RemoteOperationKind::AgentSession
        | RemoteOperationKind::Terminal
        | RemoteOperationKind::Unsupported => None,
    }
}

fn rpc_error_response(
    request_id: RequestId,
    correlation_id: Option<vibex_core::CorrelationId>,
    mut error: VibexError,
) -> RemoteRpcResponseV2 {
    if let Some(correlation_id) = correlation_id.clone() {
        error = error.with_correlation_id(correlation_id);
    }
    RemoteRpcResponseV2 {
        request_id,
        correlation_id,
        payload: None,
        error: Some(RemoteProtocolError::from_error(error)),
        metadata: RemoteRpcResultMetadata::default(),
        completed_at_ms: unix_timestamp_ms(),
    }
}

fn terminal_manager(dispatcher: &RemoteDispatcher) -> Option<TerminalManager> {
    dispatcher
        .state
        .workbench
        .as_ref()
        .map(|workbench| workbench.terminals.clone())
}

fn prepare_file_download(
    state: &GatewayState,
    scope_id: Option<&str>,
    resource_id: &str,
) -> VibexResult<(WorkspaceId, Vec<u8>)> {
    let scope_id = scope_id.ok_or_else(|| {
        VibexError::validation(
            "remote_file_scope_required",
            "file attachment requires a workspace scope",
        )
    })?;
    let workspace_id = WorkspaceId::parse(scope_id.to_string()).map_err(|_| {
        VibexError::validation(
            "remote_file_scope_invalid",
            "file attachment workspace scope is invalid",
        )
    })?;
    let path = resource_id.trim();
    if path.is_empty() || path.len() > 4096 {
        return Err(VibexError::validation(
            "remote_file_path_invalid",
            "file attachment path is empty or exceeds the bounded size",
        ));
    }
    let (_connection, service) = file_service_for_workspace(&state.db_path, &workspace_id)?;
    let bytes = service.read_bytes(&workspace_id, path, MAX_FILE_TRANSFER_BYTES)?;
    Ok((workspace_id, bytes))
}

fn encoded_file_frames(
    bytes: &[u8],
    _workspace_id: &WorkspaceId,
    stream_id: &str,
    generation: u64,
    after_sequence: u64,
) -> VibexResult<Vec<Vec<u8>>> {
    if bytes.len() > MAX_FILE_TRANSFER_BYTES {
        return Err(VibexError::capability(
            "remote_file_binary_exceeds_limit",
            "file attachment exceeds the bounded transfer limit",
        ));
    }
    let chunk_size = FILE_TRANSFER_CHUNK_BYTES.min(REMOTE_V2_MAX_BINARY_PAYLOAD_BYTES);
    let total_size = u64::try_from(bytes.len()).map_err(|_| {
        VibexError::capability(
            "remote_file_binary_exceeds_limit",
            "file attachment size is not representable",
        )
    })?;
    let mut frames = Vec::new();
    let mut sequence = after_sequence;
    let mut offset = 0_u64;
    if bytes.is_empty() {
        frames.push(
            RemoteBinaryFrame {
                header: RemoteBinaryFrameHeader {
                    protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                    kind: RemoteBinaryFrameKind::FileDownloadChunk,
                    stream_id: stream_id.to_string(),
                    request_id: None,
                    generation,
                    sequence,
                    offset,
                    total_size: Some(0),
                    snapshot: true,
                    end_of_stream: true,
                    checksum_sha256: Some(hex_sha256(&[])),
                    payload_length: 0,
                },
                payload: Vec::new(),
            }
            .encode()?,
        );
        return Ok(frames);
    }
    while offset < total_size {
        let start = usize::try_from(offset).map_err(|_| {
            VibexError::capability(
                "remote_file_binary_exceeds_limit",
                "file attachment offset is not representable",
            )
        })?;
        let end = start.saturating_add(chunk_size).min(bytes.len());
        let payload = bytes[start..end].to_vec();
        let end_of_stream = end == bytes.len();
        let frame = RemoteBinaryFrame {
            header: RemoteBinaryFrameHeader {
                protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                kind: RemoteBinaryFrameKind::FileDownloadChunk,
                stream_id: stream_id.to_string(),
                request_id: None,
                generation,
                sequence,
                offset,
                total_size: Some(total_size),
                snapshot: sequence == after_sequence,
                end_of_stream,
                checksum_sha256: Some(hex_sha256(&payload)),
                payload_length: 0,
            },
            payload,
        }
        .encode()?;
        frames.push(frame);
        offset = u64::try_from(end).unwrap_or(total_size);
        sequence = sequence.saturating_add(1);
    }
    Ok(frames)
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn spawn_file_download_stream(
    bytes: Vec<u8>,
    workspace_id: WorkspaceId,
    stream_id: String,
    generation: u64,
    after_sequence: u64,
    outbound: mpsc::Sender<OutboundFrame>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(frames) = encoded_file_frames(
            &bytes,
            &workspace_id,
            &stream_id,
            generation,
            after_sequence,
        ) else {
            return;
        };
        for frame in frames {
            if outbound.send(OutboundFrame::Binary(frame)).await.is_err() {
                break;
            }
        }
    })
}

fn spawn_relay_file_download_stream(
    bytes: Vec<u8>,
    workspace_id: WorkspaceId,
    stream_id: String,
    generation: u64,
    after_sequence: u64,
    outbound: mpsc::Sender<RelayRemoteOutbound>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(frames) = encoded_file_frames(
            &bytes,
            &workspace_id,
            &stream_id,
            generation,
            after_sequence,
        ) else {
            return;
        };
        for frame in frames {
            if outbound
                .send(RelayRemoteOutbound::Binary(frame))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

async fn send_json(
    outbound: &mpsc::Sender<OutboundFrame>,
    message: RemoteJsonMessageV2,
) -> Result<(), ()> {
    let encoded = serde_json::to_string(&message).map_err(|_| ())?;
    outbound
        .send(OutboundFrame::Text(encoded))
        .await
        .map_err(|_| ())
}

async fn close_with_error(outbound: &mpsc::Sender<OutboundFrame>, error: VibexError) {
    let (code, retry) = match error.category {
        ErrorCategory::Permission => (
            RemoteCloseCode::AuthenticationRequired,
            RemoteRetryClass::RefreshAuthentication,
        ),
        ErrorCategory::Remote if error.code.contains("version") => {
            (RemoteCloseCode::UnsupportedVersion, RemoteRetryClass::Never)
        }
        _ => (RemoteCloseCode::ProtocolError, RemoteRetryClass::Never),
    };
    close_with_reason(
        outbound,
        RemoteCloseReason {
            code,
            message: error.message,
            retry,
        },
    )
    .await;
}

async fn close_with_reason(outbound: &mpsc::Sender<OutboundFrame>, reason: RemoteCloseReason) {
    let _ = send_json(
        outbound,
        RemoteJsonMessageV2::Control(RemoteControlMessageV2::Close(reason.clone())),
    )
    .await;
    let _ = outbound.send(OutboundFrame::Close(reason)).await;
}

async fn finish_writer(outbound: mpsc::Sender<OutboundFrame>, mut writer_task: JoinHandle<()>) {
    drop(outbound);
    if tokio::time::timeout(Duration::from_secs(1), &mut writer_task)
        .await
        .is_err()
    {
        writer_task.abort();
    }
}

fn websocket_close_code(code: RemoteCloseCode) -> u16 {
    match code {
        RemoteCloseCode::Normal => 1000,
        RemoteCloseCode::ServerShutdown => 1012,
        RemoteCloseCode::ProtocolError | RemoteCloseCode::Unknown => 4400,
        RemoteCloseCode::AuthenticationRequired => 4401,
        RemoteCloseCode::DeviceRevoked => 4403,
        RemoteCloseCode::UnsupportedVersion => 4406,
        RemoteCloseCode::PolicyViolation => 4408,
    }
}

async fn security_perimeter(
    State(state): State<GatewayState>,
    request: Request,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if let Err((status, error)) = validate_perimeter_request(&state, &request, origin.as_deref()) {
        return protocol_error_response(status, error);
    }
    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        apply_cors_headers(&mut response, &request, origin.as_deref());
        return response;
    }
    let request_headers = request.headers().clone();
    let mut response = next.run(request).await;
    if origin.is_some() {
        apply_cors_headers_from_headers(&mut response, &request_headers, origin.as_deref());
    }
    response
}

#[allow(clippy::result_large_err)]
fn validate_perimeter_request(
    state: &GatewayState,
    request: &Request<Body>,
    origin: Option<&str>,
) -> Result<(), (StatusCode, VibexError)> {
    let host_header = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                VibexError::validation(
                    "remote_host_required",
                    "RemoteGateway requires a valid Host header",
                ),
            )
        })?;
    let host = normalize_host(host_header).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            VibexError::validation(
                "remote_host_required",
                "RemoteGateway requires a valid Host header",
            ),
        )
    })?;
    let host_allowed = state
        .config
        .allowed_hosts
        .iter()
        .filter_map(|allowed| normalize_host(allowed))
        .any(|allowed| allowed == host);
    if !host_allowed {
        return Err((
            StatusCode::MISDIRECTED_REQUEST,
            VibexError::new(
                ErrorCategory::Remote,
                "remote_host_rejected",
                "RemoteGateway rejected the Host header",
            ),
        ));
    }
    if query_contains_sensitive_key(request.uri()) {
        return Err((
            StatusCode::BAD_REQUEST,
            VibexError::validation(
                "remote_secret_in_url_rejected",
                "RemoteGateway authentication and pairing secrets must not appear in URLs",
            ),
        ));
    }
    if request.uri().path() == "/ws/v2" && origin.is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            VibexError::new(
                ErrorCategory::Remote,
                "remote_origin_required",
                "RemoteGateway WebSocket requires an Origin header",
            ),
        ));
    }
    if let Some(origin) = origin
        && !origin_allowed(&state.config, origin, host_header)
    {
        return Err((
            StatusCode::FORBIDDEN,
            VibexError::new(
                ErrorCategory::Remote,
                "remote_origin_rejected",
                "RemoteGateway rejected the Origin header",
            ),
        ));
    }
    Ok(())
}

fn origin_allowed(config: &RemoteGatewayConfig, origin: &str, request_host: &str) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let scheme_allowed = match config.tls_policy {
        RemoteGatewayTlsPolicy::LoopbackHttp => matches!(url.scheme(), "http" | "https"),
        RemoteGatewayTlsPolicy::TrustedHttpsProxy => url.scheme() == "https",
    };
    if !scheme_allowed {
        return false;
    }
    let explicitly_allowed = normalize_origin(origin).is_some_and(|origin| {
        config
            .allowed_origins
            .iter()
            .filter_map(|allowed| normalize_origin(allowed))
            .any(|allowed| allowed == origin)
    });
    explicitly_allowed
        || origin_authority(&url)
            .zip(normalize_authority(request_host))
            .is_some_and(|(origin, request)| origin == request)
}

fn origin_authority(url: &Url) -> Option<(String, Option<u16>)> {
    Some((
        url.host_str()?.trim_end_matches('.').to_ascii_lowercase(),
        url.port(),
    ))
}

fn normalize_authority(value: &str) -> Option<(String, Option<u16>)> {
    let value = value.trim();
    if value.is_empty()
        || value.contains('/')
        || value.contains('@')
        || value.chars().any(char::is_whitespace)
    {
        return None;
    }
    if let Some(rest) = value.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?.parse::<u16>().ok()?)
        };
        return Some((host.trim_end_matches('.').to_ascii_lowercase(), port));
    }
    if let Some((host, port)) = value.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (!host.is_empty())
            .then(|| (host.trim_end_matches('.').to_ascii_lowercase(), Some(port)));
    }
    Some((value.trim_end_matches('.').to_ascii_lowercase(), None))
}

fn normalize_host(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.contains('/')
        || value.contains('@')
        || value.chars().any(char::is_whitespace)
    {
        return None;
    }
    let host = if let Some(rest) = value.strip_prefix('[') {
        rest.split_once(']')?.0
    } else if let Some((host, port)) = value.rsplit_once(':') {
        if port.parse::<u16>().is_ok() {
            host
        } else {
            value
        }
    } else {
        value
    };
    (!host.is_empty()).then(|| host.trim_end_matches('.').to_ascii_lowercase())
}

fn is_loopback_host(host: &str) -> bool {
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn validate_origin_value(origin: &str) -> VibexResult<()> {
    normalize_origin(origin).map(|_| ()).ok_or_else(|| {
        VibexError::validation(
            "remote_gateway_origin_invalid",
            "RemoteGateway Origin allowlist entry is invalid",
        )
    })
}

fn normalize_origin(origin: &str) -> Option<String> {
    let url = Url::parse(origin.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    };
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!("{}://{host}{port}", url.scheme()))
}

fn query_contains_sensitive_key(uri: &Uri) -> bool {
    uri.query().is_some_and(|query| {
        query.split('&').any(|pair| {
            let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
            let normalized = key
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            ["auth", "token", "secret", "pairing", "code", "proof"]
                .iter()
                .any(|marker| normalized.contains(marker))
        })
    })
}

fn apply_cors_headers(response: &mut Response, request: &Request, origin: Option<&str>) {
    apply_cors_headers_from_headers(response, request.headers(), origin);
}

fn apply_cors_headers_from_headers(
    response: &mut Response,
    request_headers: &HeaderMap,
    origin: Option<&str>,
) {
    let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(origin).ok()) else {
        return;
    };
    response
        .headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    response
        .headers_mut()
        .insert(VARY, HeaderValue::from_static("Origin"));
    response.headers_mut().insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    response.headers_mut().insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type, Sec-WebSocket-Protocol"),
    );
    let request_private_network = HeaderName::from_static("access-control-request-private-network");
    if request_headers
        .get(request_private_network)
        .is_some_and(|value| value == "true")
    {
        response.headers_mut().insert(
            HeaderName::from_static("access-control-allow-private-network"),
            HeaderValue::from_static("true"),
        );
    }
}

async fn static_index(State(state): State<GatewayState>) -> Response {
    serve_static_path(&state, "index.html")
}

async fn static_asset(State(state): State<GatewayState>, Path(path): Path<String>) -> Response {
    serve_static_path(&state, &path)
}

fn serve_static_path(state: &GatewayState, request_path: &str) -> Response {
    let Some(root) = state.config.static_dir.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let relative = FsPath::new(request_path.trim_start_matches('/'));
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            VibexError::validation(
                "remote_static_path_invalid",
                "RemoteGateway static asset path is invalid",
            ),
        );
    }
    let candidate = root.join(relative);
    let mut resolved = match candidate.canonicalize() {
        Ok(candidate) if candidate.starts_with(&root) => candidate,
        _ if relative.extension().is_none() => root.join("index.html"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if resolved.is_dir() {
        resolved = resolved.join("index.html");
    }
    let resolved = match resolved.canonicalize() {
        Ok(candidate) if candidate.starts_with(&root) && candidate.is_file() => candidate,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let bytes = match std::fs::read(&resolved) {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_type = content_type_for_path(&resolved);
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn content_type_for_path(path: &FsPath) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn protocol_error_response(status: StatusCode, error: VibexError) -> Response {
    (status, Json(RemoteProtocolError::from_error(error))).into_response()
}

fn status_for_error(error: &VibexError) -> StatusCode {
    match error.category {
        ErrorCategory::Validation => StatusCode::BAD_REQUEST,
        ErrorCategory::Permission => StatusCode::FORBIDDEN,
        ErrorCategory::Conflict => StatusCode::CONFLICT,
        ErrorCategory::Remote if error.code.contains("identity") => StatusCode::UNAUTHORIZED,
        ErrorCategory::Remote => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn gateway_state_error() -> VibexError {
    VibexError::process(
        "remote_gateway_state_unavailable",
        "RemoteGateway state is unavailable",
    )
}

fn gateway_capabilities() -> Vec<String> {
    [
        "full_duplex_rpc",
        "server_events",
        "binary_terminal",
        "binary_file_contract",
        "cursor_generation",
        "idempotency",
        "revision_cas",
        "resync_required",
        "single_use_ws_ticket",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn gateway_features(state: &GatewayState) -> Vec<String> {
    let mut features = [
        "agent",
        "workspace_file",
        "git",
        "git_worktree_read",
        "terminal",
        "provider_settings",
        "device_management",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if state
        .pairing_routes
        .lock()
        .map(|routes| !routes.is_empty())
        .unwrap_or(false)
    {
        features.push("device_pairing".to_string());
    }
    features
}

fn gateway_topics() -> [&'static str; 7] {
    [
        "agent_session",
        "terminal",
        "git",
        "file",
        "provider",
        "device",
        "runtime",
    ]
}

fn bounded_diagnostic(value: &str) -> String {
    value.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request as HttpRequest;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::{
        ORIGIN as WS_ORIGIN, SEC_WEBSOCKET_PROTOCOL as WS_PROTOCOL,
    };
    use tower::ServiceExt;
    use vibex_core::{
        RemoteClaimPairingCodeRequest, RemoteCreatePairingCodeRequest,
        RemoteCreatePairingOfferRequest, RemoteDeviceCancelPairingOfferRequest,
        RemoteDeviceCreatePairingOfferRequest, RemoteDeviceListRequest, RemoteDeviceListResponse,
        RemoteDevicePermissionLevel, RemoteDeviceRequest, RemoteDeviceRevokeRequest,
        RemoteMutationContract, RemotePairingOfferSummary, RemoteRevokeDeviceRequest,
    };
    use vibex_db::{RemoteDeviceRepository, apply_migrations};

    use super::*;

    fn test_gateway(
        directory: &tempfile::TempDir,
        mut config: RemoteGatewayConfig,
    ) -> RemoteGateway {
        let database_path = directory.path().join("gateway.db");
        let mut connection = open_migrated_database(&database_path).unwrap();
        apply_migrations(&mut connection).unwrap();
        config.service.enabled = true;
        let dispatcher = RemoteDispatcher::new(config.service.clone());
        RemoteGateway::new(
            config,
            dispatcher,
            &database_path,
            directory.path().join("identity.json"),
        )
    }

    #[test]
    fn gateway_config_requires_explicit_lan_tls_and_valid_allowlists() {
        let mut config = RemoteGatewayConfig::loopback_enabled("0.0.0.0:0");
        assert_eq!(
            config.validate().unwrap_err().code,
            "remote_gateway_lan_requires_opt_in"
        );
        config.deployment_mode = RemoteGatewayDeploymentMode::Lan;
        assert_eq!(
            config.validate().unwrap_err().code,
            "remote_gateway_lan_tls_required"
        );
        config.tls_policy = RemoteGatewayTlsPolicy::TrustedHttpsProxy;
        config.allowed_hosts = vec!["vibex.example.test".to_string()];
        config.allowed_origins = vec!["https://vibex.example.test".to_string()];
        assert!(config.validate().is_ok());

        let mut disabled = RemoteGatewayConfig::default();
        disabled.pairing_routes.direct_candidates = vec![RemotePairingCandidate {
            transport: RemotePairingTransport::Direct,
            url: "https://private-gateway.example.test".to_string(),
            relay_room_id: None,
            relay_pc_peer_id: None,
            relay_pc_public_key: None,
        }];
        assert_eq!(
            disabled.validate().unwrap_err().code,
            "remote_pairing_direct_gateway_disabled"
        );
        let debug = format!("{:?}", disabled.pairing_routes);
        assert!(debug.contains("direct_candidate_count"));
        assert!(!debug.contains("private-gateway"));
    }

    #[tokio::test]
    async fn gateway_config_replacement_is_stopped_only_and_routes_are_independent() {
        let directory = tempfile::tempdir().unwrap();
        let mut initial = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
        initial.static_dir = Some(directory.path().to_path_buf());
        let gateway = test_gateway(&directory, initial);

        let mut replacement = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
        replacement.max_connections = 7;
        replacement.static_dir = Some(directory.path().to_path_buf());
        gateway
            .apply_config_while_stopped(replacement.clone())
            .await
            .unwrap();
        assert_eq!(gateway.current_config().max_connections, 7);

        gateway.start().await.unwrap();
        let running = gateway.status();
        let relay = RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: "https://relay.example.test".to_string(),
            relay_room_id: Some(vibex_core::RelayRoomId::new()),
            relay_pc_peer_id: Some(vibex_core::RelayPeerId::new()),
            relay_pc_public_key: Some("relay-public-key".to_string()),
        };
        gateway
            .set_pairing_routes(RemoteGatewayPairingRoutes {
                direct_candidates: Vec::new(),
                relay_candidate: Some(relay.clone()),
            })
            .unwrap();
        let route_updated = gateway.status();
        assert_eq!(route_updated.bound_addr, running.bound_addr);
        assert_eq!(route_updated.session_epoch, running.session_epoch);
        gateway
            .set_pairing_routes(RemoteGatewayPairingRoutes::default())
            .unwrap();
        let error = gateway
            .apply_config_while_stopped(RemoteGatewayConfig::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, "remote_gateway_config_running");
        assert_eq!(gateway.current_config(), replacement);
        gateway.stop().await.unwrap();

        let mut disabled = RemoteGatewayConfig::default();
        disabled.pairing_routes.relay_candidate = Some(relay.clone());
        gateway.apply_config_while_stopped(disabled).await.unwrap();
        assert!(!gateway.current_config().service.enabled);
        assert!(gateway.pairing_routes_available());

        gateway
            .set_pairing_routes(RemoteGatewayPairingRoutes {
                direct_candidates: Vec::new(),
                relay_candidate: Some(relay),
            })
            .unwrap();
        assert!(!gateway.current_config().service.enabled);
    }

    #[test]
    fn relay_epoch_change_requests_reconnect_without_revoking_the_device() {
        let stale = relay_close_reason(
            "remote_relay_session_epoch_stale",
            "relay session epoch changed",
        );
        let revoked = relay_close_reason("remote_device_revoked", "device revoked");

        assert_eq!(stale.code, RemoteCloseCode::PolicyViolation);
        assert_eq!(stale.retry, RemoteRetryClass::Reconnect);
        assert_eq!(revoked.code, RemoteCloseCode::DeviceRevoked);
        assert_eq!(revoked.retry, RemoteRetryClass::Never);
    }

    #[test]
    fn rpc_concurrency_limit_is_bounded_per_connection() {
        let slots = Arc::new(Semaphore::new(1));
        let request = RemoteRpcRequestV2::new(RemoteOperationKind::AgentSession, None);

        let permit = acquire_rpc_slot(&slots, &request).unwrap();
        let error = acquire_rpc_slot(&slots, &request).unwrap_err();
        assert_eq!(
            error.error.unwrap().error.code,
            "remote_rpc_concurrency_limit"
        );

        drop(permit);
        assert!(acquire_rpc_slot(&slots, &request).is_ok());
    }

    #[test]
    fn subscriptions_are_filtered_by_the_device_grant() {
        let topics = gateway_topics()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let read_only = Arc::new(Mutex::new(HashSet::new()));
        let accepted = update_subscriptions(
            &read_only,
            RemoteSubscribeRequestV2 {
                subscription_id: "read-only".to_string(),
                topics: topics.clone(),
                cursors: Vec::new(),
            },
            RemoteDevicePermissionLevel::ReadOnly,
        );
        assert!(accepted.topics.contains(&"agent_session".to_string()));
        assert!(accepted.topics.contains(&"file".to_string()));
        assert!(accepted.topics.contains(&"provider".to_string()));
        assert!(!accepted.topics.contains(&"device".to_string()));
        assert!(
            accepted
                .resync_required
                .iter()
                .all(|item| item.domain != "device")
        );

        let full_control = Arc::new(Mutex::new(HashSet::new()));
        let accepted = update_subscriptions(
            &full_control,
            RemoteSubscribeRequestV2 {
                subscription_id: "full-control".to_string(),
                topics,
                cursors: Vec::new(),
            },
            RemoteDevicePermissionLevel::FullControl,
        );
        assert!(accepted.topics.contains(&"device".to_string()));
    }

    #[tokio::test]
    async fn full_control_device_management_is_typed_authorized_and_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let mut gateway_config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
        gateway_config.pairing_routes.direct_candidates = vec![RemotePairingCandidate {
            transport: RemotePairingTransport::Direct,
            url: "http://127.0.0.1:1428".to_string(),
            relay_room_id: None,
            relay_pc_peer_id: None,
            relay_pc_public_key: None,
        }];
        let gateway = test_gateway(&directory, gateway_config);
        let database_path = directory.path().join("gateway.db");
        let admin = pair_test_device(
            &database_path,
            RemoteDevicePermissionLevel::FullControl,
            None,
        );
        let target = pair_test_device(
            &database_path,
            RemoteDevicePermissionLevel::ReadOnly,
            Some("device-public-key-sentinel".to_string()),
        );
        let state = gateway_state_for_test(&gateway);
        let mut domain_events = state.domain_events.subscribe();

        let create_payload =
            RemoteDeviceRequest::CreatePairingOffer(RemoteDeviceCreatePairingOfferRequest {
                auth: admin.clone(),
                request: RemoteCreatePairingOfferRequest {
                    permission_level: RemoteDevicePermissionLevel::ReadOnly,
                    ttl_ms: Some(60_000),
                    direct_candidates: vec![RemotePairingCandidate {
                        transport: RemotePairingTransport::Direct,
                        url: "https://client-controlled.invalid".to_string(),
                        relay_room_id: None,
                        relay_pc_peer_id: None,
                        relay_pc_public_key: None,
                    }],
                    relay_candidate: None,
                },
            });
        let mut create = RemoteRpcRequestV2::new(
            RemoteOperationKind::DeviceManagement,
            Some(serde_json::to_value(create_payload).unwrap()),
        );
        create.mutation = Some(RemoteMutationContract {
            idempotency_key: "device-pairing-create".to_string(),
            expected_revision: None,
            expected_generation: None,
        });
        let create_retry = create.clone();
        let created = process_rpc_inner(&state, &admin, create).await.unwrap();
        let offer: vibex_core::RemoteCreatePairingOfferResponse =
            serde_json::from_value(created.payload.unwrap()).unwrap();
        assert!(!offer.launch_fragment.is_empty());
        assert_eq!(
            offer.offer.summary.direct_candidates[0].url,
            "http://127.0.0.1:1428/"
        );
        assert!(
            !offer.offer.summary.direct_candidates[0]
                .url
                .contains("client-controlled")
        );
        let event = domain_events.try_recv().unwrap();
        assert_eq!(event.channel, "device");
        assert_eq!(event.sequence, 1);
        assert!(event.payload.is_none());
        let cached = process_rpc_inner(&state, &admin, create_retry.clone())
            .await
            .unwrap();
        assert!(cached.payload.is_some());
        assert!(matches!(
            domain_events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        let cancel_payload =
            RemoteDeviceRequest::CancelPairingOffer(RemoteDeviceCancelPairingOfferRequest {
                auth: admin.clone(),
                request: vibex_core::RemoteCancelPairingOfferRequest {
                    offer_id: offer.offer.summary.offer_id,
                },
            });
        let mut cancel = RemoteRpcRequestV2::new(
            RemoteOperationKind::DeviceManagement,
            Some(serde_json::to_value(cancel_payload).unwrap()),
        );
        cancel.mutation = Some(RemoteMutationContract {
            idempotency_key: "device-pairing-cancel".to_string(),
            expected_revision: None,
            expected_generation: None,
        });
        let canceled = process_rpc_inner(&state, &admin, cancel).await.unwrap();
        let canceled: RemotePairingOfferSummary =
            serde_json::from_value(canceled.payload.unwrap()).unwrap();
        assert!(canceled.canceled);
        assert_eq!(domain_events.try_recv().unwrap().sequence, 2);

        let list = RemoteRpcRequestV2::new(
            RemoteOperationKind::DeviceManagement,
            Some(
                serde_json::to_value(RemoteDeviceRequest::ListDevices(RemoteDeviceListRequest {
                    auth: admin.clone(),
                }))
                .unwrap(),
            ),
        );
        let listed = process_rpc_inner(&state, &admin, list).await.unwrap();
        let listed: RemoteDeviceListResponse =
            serde_json::from_value(listed.payload.unwrap()).unwrap();
        assert!(listed.devices.len() >= 2);
        assert!(
            listed
                .devices
                .iter()
                .all(|device| device.public_key.is_none())
        );
        assert!(matches!(
            domain_events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        let revoke_payload = RemoteDeviceRequest::RevokeDevice(RemoteDeviceRevokeRequest {
            auth: admin.clone(),
            request: RemoteRevokeDeviceRequest {
                device_id: target.device_id.clone(),
                reason: Some("workflow regression".to_string()),
            },
        });
        let mut revoke = RemoteRpcRequestV2::new(
            RemoteOperationKind::DeviceManagement,
            Some(serde_json::to_value(revoke_payload).unwrap()),
        );
        revoke.mutation = Some(RemoteMutationContract {
            idempotency_key: "device-revoke-target".to_string(),
            expected_revision: None,
            expected_generation: None,
        });
        let revoked = process_rpc_inner(&state, &admin, revoke).await.unwrap();
        let revoked: vibex_core::RemoteDeviceDetail =
            serde_json::from_value(revoked.payload.unwrap()).unwrap();
        assert_eq!(revoked.status, vibex_core::RemoteDeviceStatus::Revoked);
        assert!(revoked.public_key.is_none());
        assert_eq!(domain_events.try_recv().unwrap().sequence, 3);

        let self_revoke_payload = RemoteDeviceRequest::RevokeDevice(RemoteDeviceRevokeRequest {
            auth: admin.clone(),
            request: RemoteRevokeDeviceRequest {
                device_id: admin.device_id.clone(),
                reason: None,
            },
        });
        let mut self_revoke = RemoteRpcRequestV2::new(
            RemoteOperationKind::DeviceManagement,
            Some(serde_json::to_value(self_revoke_payload).unwrap()),
        );
        self_revoke.mutation = Some(RemoteMutationContract {
            idempotency_key: "device-revoke-self".to_string(),
            expected_revision: None,
            expected_generation: None,
        });
        assert_eq!(
            process_rpc_inner(&state, &admin, self_revoke)
                .await
                .unwrap_err()
                .code,
            "remote_device_self_revoke_forbidden"
        );
        assert!(matches!(
            domain_events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        let reader = pair_test_device(&database_path, RemoteDevicePermissionLevel::ReadOnly, None);
        let reader_list = RemoteRpcRequestV2::new(
            RemoteOperationKind::DeviceManagement,
            Some(
                serde_json::to_value(RemoteDeviceRequest::ListDevices(RemoteDeviceListRequest {
                    auth: reader.clone(),
                }))
                .unwrap(),
            ),
        );
        assert_eq!(
            process_rpc_inner(&state, &reader, reader_list)
                .await
                .unwrap_err()
                .code,
            "remote_permission_denied"
        );

        let connection = open_migrated_database(&database_path).unwrap();
        let mut admin_record = RemoteDeviceRepository::get(&connection, &admin.device_id)
            .unwrap()
            .unwrap();
        admin_record.detail.grant_revision = admin_record.detail.grant_revision.saturating_add(1);
        admin_record.detail.permission_level = RemoteDevicePermissionLevel::ReadOnly;
        RemoteDeviceRepository::upsert(&connection, &admin_record).unwrap();
        assert!(
            cached_rpc_response(
                &state,
                &IdempotencyCacheKey {
                    device_id: admin.device_id.clone(),
                    key: "device-pairing-create".to_string(),
                },
                &admin,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            process_rpc_inner(&state, &admin, create_retry.clone())
                .await
                .unwrap_err()
                .code,
            "remote_permission_denied"
        );
        RemoteDeviceRepository::revoke(&connection, &admin.device_id, unix_timestamp_ms()).unwrap();
        assert_eq!(
            process_rpc_inner(&state, &admin, create_retry)
                .await
                .unwrap_err()
                .code,
            "remote_device_revoked"
        );
    }

    #[test]
    fn live_terminal_attach_reauthenticates_before_streaming() {
        let directory = tempfile::tempdir().unwrap();
        let gateway = test_gateway(
            &directory,
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
        );
        let auth = pair_test_device(
            &directory.path().join("gateway.db"),
            RemoteDevicePermissionLevel::ReadOnly,
            None,
        );
        let ticket = WsTicketRecord {
            proof: auth.clone(),
            auth: RemoteAuthContext {
                device_id: auth.device_id.clone(),
                display_name: "Integration Browser".to_string(),
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                authenticated_at_ms: unix_timestamp_ms(),
            },
            expires_at_ms: unix_timestamp_ms() + 30_000,
            proof_challenge: "unused-test-challenge".to_string(),
            relay_authenticated: false,
        };
        let state = gateway_state_for_test(&gateway);
        assert!(authorize_live_action(&state, &ticket, RemoteActionClass::ReadProject).is_ok());

        let connection = open_migrated_database(&directory.path().join("gateway.db")).unwrap();
        RemoteDeviceRepository::revoke(&connection, &auth.device_id, unix_timestamp_ms()).unwrap();
        assert_eq!(
            authorize_live_action(&state, &ticket, RemoteActionClass::ReadProject)
                .unwrap_err()
                .code,
            "remote_device_revoked"
        );
    }

    #[test]
    fn terminal_binary_input_rejects_stale_connection_generation() {
        let directory = tempfile::tempdir().unwrap();
        let gateway = test_gateway(
            &directory,
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
        );
        let state = gateway_state_for_test(&gateway);
        let auth = pair_test_device(
            &directory.path().join("gateway.db"),
            RemoteDevicePermissionLevel::FullControl,
            None,
        );
        let ticket = WsTicketRecord {
            proof: auth.clone(),
            auth: RemoteAuthContext {
                device_id: auth.device_id,
                display_name: "Integration Browser".to_string(),
                permission_level: RemoteDevicePermissionLevel::FullControl,
                authenticated_at_ms: unix_timestamp_ms(),
            },
            expires_at_ms: unix_timestamp_ms() + 30_000,
            proof_challenge: "unused-test-challenge".to_string(),
            relay_authenticated: false,
        };
        let frame = RemoteBinaryFrame {
            header: RemoteBinaryFrameHeader {
                protocol_version: RemoteProtocolVersion { major: 2, minor: 0 },
                kind: RemoteBinaryFrameKind::TerminalInput,
                stream_id: "terminal_stale".to_string(),
                request_id: None,
                generation: state.session_epoch.saturating_sub(1),
                sequence: 1,
                offset: 0,
                total_size: None,
                snapshot: false,
                end_of_stream: false,
                checksum_sha256: None,
                payload_length: 0,
            },
            payload: b"must-not-be-written".to_vec(),
        }
        .encode()
        .unwrap();
        let (outbound, _) = mpsc::channel(1);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(handle_active_binary(
                &state,
                &ticket,
                &frame,
                &outbound,
                &mut HashMap::new(),
            ))
            .unwrap_err();
        assert_eq!(error.code, "remote_binary_generation_stale");
    }

    #[tokio::test]
    async fn relay_attachment_tasks_abort_on_detach() {
        struct AbortFlag(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for AbortFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let aborted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_flag = aborted.clone();
        let task = tokio::spawn(async move {
            let _flag = AbortFlag(task_flag);
            futures_util::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;

        let mut tasks = RelayAttachmentTasks::default();
        tasks.insert("terminal-a".to_string(), task).unwrap();
        tasks.detach("terminal-a");
        for _ in 0..10 {
            if aborted.load(Ordering::Acquire) {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert!(aborted.load(Ordering::Acquire));
    }

    #[test]
    fn hello_rejects_non_contributory_device_identity_key() {
        let directory = tempfile::tempdir().unwrap();
        let gateway = test_gateway(
            &directory,
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
        );
        let zero_public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 32]);
        let auth = pair_test_device(
            &directory.path().join("gateway.db"),
            RemoteDevicePermissionLevel::ReadOnly,
            Some(zero_public_key.clone()),
        );
        let ticket = WsTicketRecord {
            proof: auth.clone(),
            auth: RemoteAuthContext {
                device_id: auth.device_id.clone(),
                display_name: "Integration Browser".to_string(),
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                authenticated_at_ms: unix_timestamp_ms(),
            },
            expires_at_ms: unix_timestamp_ms() + 30_000,
            proof_challenge: "proof-challenge".to_string(),
            relay_authenticated: false,
        };
        let hello = RemoteHello {
            client_id: "non-contributory-client".to_string(),
            client_type: vibex_core::RemoteClientType::Browser,
            app_version: "test".to_string(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            device_id: auth.device_id,
            device_identity_public_key: zero_public_key,
            client_ephemeral_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(PublicKey::from(&StaticSecret::random_from_rng(OsRng)).as_bytes()),
            identity_proof: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 32]),
            relay_auth: None,
            transport_endpoint: None,
            permission_context_hash: None,
            capabilities: Vec::new(),
            enabled_features: Vec::new(),
            last_session_epoch: None,
            cursors: Vec::new(),
        };

        let result =
            verify_hello_device_identity(&gateway_state_for_test(&gateway), &hello, &ticket);
        match result {
            Ok(_) => panic!("non-contributory identity key must be rejected"),
            Err(error) => assert_eq!(error.code, "remote_device_identity_key_invalid"),
        }
    }

    #[tokio::test]
    async fn security_perimeter_rejects_host_origin_and_secrets_in_url() {
        let directory = tempfile::tempdir().unwrap();
        let gateway = test_gateway(
            &directory,
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:1428"),
        );
        let router = gateway.router().unwrap();

        let bad_host = router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v2/info")
                    .header(HOST, "attacker.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_host.status(), StatusCode::MISDIRECTED_REQUEST);

        let bad_origin = router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v2/info")
                    .header(HOST, "127.0.0.1")
                    .header(ORIGIN, "https://attacker.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);

        let secret_url = router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v2/info?authToken=sensitive-value-123")
                    .header(HOST, "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(secret_url.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(secret_url.into_body(), usize::MAX).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("sensitive-value-123"));

        let allowed = router
            .oneshot(
                HttpRequest::get("/api/v2/info")
                    .header(HOST, "127.0.0.1")
                    .header(ORIGIN, "http://127.0.0.1:1428")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(
            allowed.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "http://127.0.0.1:1428"
        );
    }

    #[tokio::test]
    async fn gateway_info_exposes_the_configured_web_build_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:1428");
        config.web_build = Some(RemoteGatewayWebBuildDescriptor {
            schema_version: "vibex-web-build.v1".to_string(),
            build_id: "build-id".to_string(),
            package_version: "0.1.0-rc.1".to_string(),
            profile: "release".to_string(),
            git_commit: "revision".to_string(),
            wasm_sha256: "wasm".to_string(),
            glue_sha256: "glue".to_string(),
            static_sha256: "static".to_string(),
        });
        let gateway = test_gateway(&directory, config);

        let response = gateway
            .router()
            .unwrap()
            .oneshot(
                HttpRequest::get("/api/v2/info")
                    .header(HOST, "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["webBuild"]["buildId"], "build-id");
        assert_eq!(body["webBuild"]["gitCommit"], "revision");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn static_assets_reject_traversal_and_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let static_dir = directory.path().join("static");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "vibex-index").unwrap();
        let outside = directory.path().join("outside.txt");
        std::fs::write(&outside, "outside-secret-sentinel").unwrap();
        symlink(&outside, static_dir.join("escape.txt")).unwrap();

        let mut config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:1428");
        config.static_dir = Some(static_dir);
        let gateway = test_gateway(&directory, config);
        let router = gateway.router().unwrap();

        let symlink_response = router
            .clone()
            .oneshot(
                HttpRequest::get("/escape.txt")
                    .header(HOST, "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(symlink_response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(symlink_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("outside-secret-sentinel"));

        let encoded_traversal = router
            .oneshot(
                HttpRequest::get("/%2e%2e/outside.txt")
                    .header(HOST, "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!encoded_traversal.status().is_success());
    }

    #[tokio::test]
    async fn gateway_listener_start_stop_restart_releases_the_socket() {
        let directory = tempfile::tempdir().unwrap();
        let gateway = test_gateway(
            &directory,
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
        );
        let first = gateway.start().await.unwrap().unwrap();
        assert!(gateway.status().running);
        let stream = tokio::net::TcpStream::connect(first).await.unwrap();
        drop(stream);

        gateway.stop().await.unwrap();
        assert!(!gateway.status().running);
        assert!(tokio::net::TcpStream::connect(first).await.is_err());

        let second = gateway.restart().await.unwrap().unwrap();
        assert!(gateway.status().running);
        assert!(tokio::net::TcpStream::connect(second).await.is_ok());
        gateway.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ws_ticket_is_single_use_and_v2_hello_rpc_then_revoke_closes_connection() {
        let directory = tempfile::tempdir().unwrap();
        let gateway = test_gateway(
            &directory,
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
        );
        let database_path = directory.path().join("gateway.db");
        let device_identity = StaticSecret::random_from_rng(OsRng);
        let device_public = PublicKey::from(&device_identity);
        let auth = pair_test_device(
            &database_path,
            RemoteDevicePermissionLevel::ReadOnly,
            Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(device_public.as_bytes())),
        );
        let address = gateway.start().await.unwrap().unwrap();
        let origin = format!("http://{address}");

        let ticket = issue_test_ticket(&gateway, auth.clone()).await;
        let (mut socket, _) =
            tokio_tungstenite::connect_async(ws_request(address, &origin, &ticket.subprotocol))
                .await
                .unwrap();
        let client_ephemeral = StaticSecret::random_from_rng(OsRng);
        let client_ephemeral_public = PublicKey::from(&client_ephemeral);
        let identity = gateway.identity().unwrap();
        let mut hello = RemoteHello {
            client_id: "integration-client".to_string(),
            client_type: vibex_core::RemoteClientType::Browser,
            app_version: "test".to_string(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            device_id: auth.device_id.clone(),
            device_identity_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(device_public.as_bytes()),
            client_ephemeral_public_key: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(client_ephemeral_public.as_bytes()),
            identity_proof: String::new(),
            relay_auth: None,
            transport_endpoint: None,
            permission_context_hash: None,
            capabilities: vec!["rpc".to_string()],
            enabled_features: Vec::new(),
            last_session_epoch: None,
            cursors: Vec::new(),
        };
        let transcript = hello_transcript(
            &hello,
            &ticket.proof_challenge,
            identity.server_id(),
            gateway.status().session_epoch,
        )
        .unwrap();
        let server_public =
            decode_x25519_public_key(&identity.public_key_base64(), "test_server_key_invalid")
                .unwrap();
        let shared = device_identity.diffie_hellman(&server_public);
        let key = derive_key(
            shared.as_bytes(),
            b"vibex.remote.v2.identity-proof",
            &transcript,
        )
        .unwrap();
        hello.identity_proof = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(authentication_tag(&key, &transcript).unwrap());
        socket
            .send(ClientMessage::Text(
                serde_json::to_string(&RemoteJsonMessageV2::Control(
                    RemoteControlMessageV2::Hello(hello),
                ))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let server_info = next_json_message(&mut socket).await;
        assert!(matches!(
            server_info,
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::ServerInfo(_))
        ));

        let rpc_id = RequestId::new();
        socket
            .send(ClientMessage::Text(
                serde_json::to_string(&RemoteJsonMessageV2::RpcRequest(RemoteRpcRequestV2 {
                    request_id: rpc_id.clone(),
                    correlation_id: None,
                    operation: "unsupported_future_operation".to_string(),
                    timeout_class: RemoteTimeoutClass::Interactive,
                    mutation: None,
                    payload: Some(serde_json::json!({
                        "type": "future_operation",
                        "data": {}
                    })),
                    created_at_ms: unix_timestamp_ms(),
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let response = next_json_message(&mut socket).await;
        match response {
            RemoteJsonMessageV2::RpcResponse(response) => {
                assert_eq!(response.request_id, rpc_id);
                assert_eq!(
                    response.error.unwrap().error.code,
                    "remote_unsupported_operation"
                );
            }
            other => panic!("expected RPC response, got {other:?}"),
        }

        let reused =
            tokio_tungstenite::connect_async(ws_request(address, &origin, &ticket.subprotocol))
                .await
                .unwrap_err();
        assert!(reused.to_string().contains("401"));

        {
            let connection = open_migrated_database(&database_path).unwrap();
            let device = RemoteDeviceRepository::get(&connection, &auth.device_id)
                .unwrap()
                .unwrap();
            RemoteDeviceRepository::revoke(
                &connection,
                &device.detail.device_id,
                unix_timestamp_ms(),
            )
            .unwrap();
        }
        gateway.disconnect_device(&auth.device_id);
        let close_message = next_json_message(&mut socket).await;
        assert!(matches!(
            close_message,
            RemoteJsonMessageV2::Control(RemoteControlMessageV2::Close(RemoteCloseReason {
                code: RemoteCloseCode::DeviceRevoked,
                ..
            }))
        ));
        gateway.stop().await.unwrap();
    }

    fn pair_test_device(
        database_path: &FsPath,
        permission_level: RemoteDevicePermissionLevel,
        public_key: Option<String>,
    ) -> RemoteAuthProof {
        let connection = open_migrated_database(database_path).unwrap();
        let pairing = RemoteTrustService::create_pairing_code(
            &connection,
            RemoteCreatePairingCodeRequest {
                permission_level,
                ttl_ms: Some(60_000),
            },
        )
        .unwrap();
        let claimed = RemoteTrustService::claim_pairing_code(
            &connection,
            RemoteClaimPairingCodeRequest {
                pairing_code: pairing.pairing_code,
                display_name: "Integration Browser".to_string(),
                public_key,
            },
        )
        .unwrap();
        RemoteAuthProof {
            device_id: claimed.device.device_id,
            auth_token: claimed.auth_token,
        }
    }

    fn gateway_state_for_test(gateway: &RemoteGateway) -> GatewayState {
        let epoch = gateway.ensure_session_epoch();
        GatewayState {
            config: Arc::new(gateway.current_config()),
            dispatcher: gateway.inner.dispatcher.clone(),
            db_path: gateway.inner.db_path.clone(),
            identity: gateway.identity().unwrap(),
            tickets: gateway.inner.tickets.clone(),
            registry: gateway.inner.registry.clone(),
            idempotency: gateway.inner.idempotency.clone(),
            domain_events: gateway.inner.domain_events.clone(),
            pairing_routes: gateway.inner.pairing_routes.clone(),
            session_epoch: epoch,
        }
    }

    async fn issue_test_ticket(
        gateway: &RemoteGateway,
        auth: RemoteAuthProof,
    ) -> RemoteWsTicketResponse {
        let router = gateway.router().unwrap();
        let response = router
            .oneshot(
                HttpRequest::post("/api/v2/ws-ticket")
                    .header(HOST, "127.0.0.1")
                    .header(ORIGIN, "http://127.0.0.1")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RemoteWsTicketRequest { auth }).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn ws_request(
        address: SocketAddr,
        origin: &str,
        subprotocol: &str,
    ) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = format!("ws://{address}/ws/v2")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(WS_ORIGIN, HeaderValue::from_str(origin).unwrap());
        request
            .headers_mut()
            .insert(WS_PROTOCOL, HeaderValue::from_str(subprotocol).unwrap());
        request
    }

    async fn next_json_message<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
    ) -> RemoteJsonMessageV2
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let ClientMessage::Text(text) = message {
                return serde_json::from_str(text.as_ref()).unwrap();
            }
        }
    }
}

use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio::time::timeout;
use tracing::{debug, info, warn};
use vibex_core::{
    CorrelationId, RelayBridgeMessage, RelayConnectionId, RelayControlMessage, RelayEncryptedFrame,
    RelayError, RelayErrorCode, RelayFrameKind, RelayHandshakeHello, RelayHeartbeatAck,
    RelayNotificationProviderKind, RelayOpaqueNotification, RelayPeerId, RelayPeerMessage,
    RelayPeerRole, RelayProtocolVersion, RelayPushDispatchResult, RelayPushRegistration,
    RelayRoomId, WEB_REQUIRED_ASSETS, WEB_STATIC_IDENTITY_ASSETS, WebBuildDescriptor,
    unix_timestamp_ms,
};

pub type RelayServerRouter = Router;

#[derive(Clone)]
pub struct RelayServerConfig {
    pub bind_addr: SocketAddr,
    pub service_name: String,
    pub server_version: String,
    pub room_ttl_ms: i64,
    pub bridge_timeout_ms: i64,
    pub heartbeat_timeout_ms: i64,
    pub max_rooms: usize,
    pub max_total_connections: usize,
    pub max_pending_per_room: usize,
    pub max_connections_per_room: usize,
    pub max_devices_per_room: usize,
    pub max_body_bytes: usize,
    pub rate_limit_window_ms: i64,
    pub max_requests_per_window_per_room: usize,
    pub max_queue_bytes_per_connection: usize,
    pub max_bandwidth_bytes_per_window: usize,
    pub max_push_installations: usize,
    pub max_push_dedup_entries: usize,
    pub push_adapter_timeout_ms: i64,
    pub push_provider: Option<RelayNotificationProviderKind>,
    pub push_auth_token: Option<String>,
    pub push_adapter_url: Option<String>,
    pub push_adapter_auth_token: Option<String>,
    pub web_static_dir: Option<PathBuf>,
}

impl std::fmt::Debug for RelayServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelayServerConfig")
            .field("bind_addr", &self.bind_addr)
            .field("service_name", &self.service_name)
            .field("server_version", &self.server_version)
            .field("room_ttl_ms", &self.room_ttl_ms)
            .field("bridge_timeout_ms", &self.bridge_timeout_ms)
            .field("heartbeat_timeout_ms", &self.heartbeat_timeout_ms)
            .field("max_rooms", &self.max_rooms)
            .field("max_total_connections", &self.max_total_connections)
            .field("max_pending_per_room", &self.max_pending_per_room)
            .field("max_connections_per_room", &self.max_connections_per_room)
            .field("max_devices_per_room", &self.max_devices_per_room)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("rate_limit_window_ms", &self.rate_limit_window_ms)
            .field(
                "max_requests_per_window_per_room",
                &self.max_requests_per_window_per_room,
            )
            .field(
                "max_queue_bytes_per_connection",
                &self.max_queue_bytes_per_connection,
            )
            .field(
                "max_bandwidth_bytes_per_window",
                &self.max_bandwidth_bytes_per_window,
            )
            .field("max_push_installations", &self.max_push_installations)
            .field("max_push_dedup_entries", &self.max_push_dedup_entries)
            .field("push_adapter_timeout_ms", &self.push_adapter_timeout_ms)
            .field("push_provider", &self.push_provider)
            .field("has_push_auth_token", &self.push_auth_token.is_some())
            .field("has_push_adapter_url", &self.push_adapter_url.is_some())
            .field(
                "has_push_adapter_auth_token",
                &self.push_adapter_auth_token.is_some(),
            )
            .field("has_web_static_dir", &self.web_static_dir.is_some())
            .finish()
    }
}

impl RelayServerConfig {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = Self::default();
        if let Ok(bind_addr) = std::env::var("VIBEX_RELAY_BIND_ADDR") {
            config.bind_addr = bind_addr.parse()?;
        } else if let Ok(port) = std::env::var("RELAY_PORT") {
            config.bind_addr = format!("127.0.0.1:{port}").parse()?;
        }
        config.web_static_dir = std::env::var("VIBEX_RELAY_WEB_STATIC_DIR")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        apply_env_i64(&mut config.room_ttl_ms, "VIBEX_RELAY_ROOM_TTL_MS")?;
        apply_env_i64(
            &mut config.bridge_timeout_ms,
            "VIBEX_RELAY_BRIDGE_TIMEOUT_MS",
        )?;
        apply_env_i64(
            &mut config.heartbeat_timeout_ms,
            "VIBEX_RELAY_HEARTBEAT_TIMEOUT_MS",
        )?;
        apply_env_usize(&mut config.max_rooms, "VIBEX_RELAY_MAX_ROOMS")?;
        apply_env_usize(
            &mut config.max_total_connections,
            "VIBEX_RELAY_MAX_TOTAL_CONNECTIONS",
        )?;
        apply_env_usize(
            &mut config.max_pending_per_room,
            "VIBEX_RELAY_MAX_PENDING_PER_ROOM",
        )?;
        apply_env_usize(
            &mut config.max_connections_per_room,
            "VIBEX_RELAY_MAX_CONNECTIONS_PER_ROOM",
        )?;
        apply_env_usize(
            &mut config.max_devices_per_room,
            "VIBEX_RELAY_MAX_DEVICES_PER_ROOM",
        )?;
        apply_env_usize(&mut config.max_body_bytes, "VIBEX_RELAY_MAX_BODY_BYTES")?;
        apply_env_i64(
            &mut config.rate_limit_window_ms,
            "VIBEX_RELAY_RATE_LIMIT_WINDOW_MS",
        )?;
        apply_env_usize(
            &mut config.max_requests_per_window_per_room,
            "VIBEX_RELAY_MAX_REQUESTS_PER_WINDOW_PER_ROOM",
        )?;
        apply_env_usize(
            &mut config.max_queue_bytes_per_connection,
            "VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION",
        )?;
        apply_env_usize(
            &mut config.max_bandwidth_bytes_per_window,
            "VIBEX_RELAY_MAX_BANDWIDTH_BYTES_PER_WINDOW",
        )?;
        apply_env_usize(
            &mut config.max_push_installations,
            "VIBEX_RELAY_MAX_PUSH_INSTALLATIONS",
        )?;
        apply_env_usize(
            &mut config.max_push_dedup_entries,
            "VIBEX_RELAY_MAX_PUSH_DEDUP_ENTRIES",
        )?;
        apply_env_i64(
            &mut config.push_adapter_timeout_ms,
            "VIBEX_RELAY_PUSH_ADAPTER_TIMEOUT_MS",
        )?;
        config.push_provider = std::env::var("VIBEX_RELAY_PUSH_PROVIDER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "web_push" | "webpush" => Ok(RelayNotificationProviderKind::WebPush),
                "apns" => Ok(RelayNotificationProviderKind::Apns),
                "fcm" => Ok(RelayNotificationProviderKind::Fcm),
                _ => Err("VIBEX_RELAY_PUSH_PROVIDER must be web_push, apns, or fcm"),
            })
            .transpose()?;
        config.push_auth_token = std::env::var("VIBEX_RELAY_PUSH_AUTH_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        config.push_adapter_url = std::env::var("VIBEX_RELAY_PUSH_ADAPTER_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        config.push_adapter_auth_token = std::env::var("VIBEX_RELAY_PUSH_ADAPTER_AUTH_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.room_ttl_ms <= 0
            || self.bridge_timeout_ms <= 0
            || self.heartbeat_timeout_ms <= 0
            || self.rate_limit_window_ms <= 0
            || self.max_rooms == 0
            || self.max_total_connections == 0
            || self.max_pending_per_room == 0
            || self.max_connections_per_room == 0
            || self.max_devices_per_room == 0
            || self.max_body_bytes == 0
            || self.max_requests_per_window_per_room == 0
            || self.max_queue_bytes_per_connection == 0
            || self.max_bandwidth_bytes_per_window == 0
            || self.max_push_installations == 0
            || self.max_push_dedup_entries == 0
            || self.push_adapter_timeout_ms <= 0
            || self.push_adapter_timeout_ms > 60_000
        {
            return Err("Relay limits must be positive".into());
        }
        if self.max_connections_per_room != 1 {
            return Err("Relay supports exactly one authoritative PC per room".into());
        }
        let push_config_values = [
            self.push_provider.is_some(),
            self.push_auth_token.is_some(),
            self.push_adapter_url.is_some(),
            self.push_adapter_auth_token.is_some(),
        ];
        if push_config_values.iter().any(|configured| *configured)
            && !push_config_values.iter().all(|configured| *configured)
        {
            return Err(
                "Relay push configuration requires provider, inbound auth, adapter URL, and adapter auth"
                    .into(),
            );
        }
        if self.push_provider.is_some() {
            if self
                .push_auth_token
                .as_deref()
                .is_none_or(|token| token.len() < 24 || token.len() > 4096)
            {
                return Err(
                    "Relay push provider requires VIBEX_RELAY_PUSH_AUTH_TOKEN with 24-4096 characters"
                        .into(),
                );
            }
            let adapter_url = self
                .push_adapter_url
                .as_deref()
                .ok_or("Relay push provider requires VIBEX_RELAY_PUSH_ADAPTER_URL")?;
            validate_push_adapter_url(adapter_url)?;
            if self
                .push_adapter_auth_token
                .as_deref()
                .is_none_or(|token| token.len() < 24 || token.len() > 4096)
            {
                return Err(
                    "Relay push provider requires VIBEX_RELAY_PUSH_ADAPTER_AUTH_TOKEN with 24-4096 characters"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn push_adapter_configured(&self) -> bool {
        self.push_provider.is_some()
            && self.push_auth_token.is_some()
            && self.push_adapter_url.is_some()
            && self.push_adapter_auth_token.is_some()
    }
}

fn validate_push_adapter_url(value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = reqwest::Url::parse(value)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Relay push adapter URL must not contain credentials, query, or fragment".into(),
        );
    }
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    if !secure && !loopback_http {
        return Err("Relay push adapter URL requires HTTPS outside loopback development".into());
    }
    Ok(())
}

fn apply_env_usize(target: &mut usize, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var(name) {
        *target = value.parse()?;
    }
    Ok(())
}

fn apply_env_i64(target: &mut i64, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var(name) {
        *target = value.parse()?;
    }
    Ok(())
}

impl Default for RelayServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:9700"
                .parse()
                .expect("default relay bind address is valid"),
            service_name: "Vibex Relay Server".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            room_ttl_ms: 60 * 60 * 1000,
            bridge_timeout_ms: 30 * 1000,
            heartbeat_timeout_ms: 45 * 1000,
            max_rooms: 1024,
            max_total_connections: 4096,
            max_pending_per_room: 64,
            max_connections_per_room: 1,
            max_devices_per_room: 8,
            max_body_bytes: 1024 * 1024,
            rate_limit_window_ms: 1000,
            max_requests_per_window_per_room: 120,
            max_queue_bytes_per_connection: 4 * 1024 * 1024,
            max_bandwidth_bytes_per_window: 16 * 1024 * 1024,
            max_push_installations: 256,
            max_push_dedup_entries: 4096,
            push_adapter_timeout_ms: 10_000,
            push_provider: None,
            push_auth_token: None,
            push_adapter_url: None,
            push_adapter_auth_token: None,
            web_static_dir: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayHealthStatus {
    pub status: RelayServerStatus,
    pub protocol_version: RelayProtocolVersion,
    pub uptime_ms: i64,
    pub active_rooms: usize,
    pub active_connections: usize,
    pub pending_bridge_requests: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayServerStatus {
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayServerInfo {
    pub service_name: String,
    pub server_version: String,
    pub protocol_version: RelayProtocolVersion,
    pub features: RelayServerFeatures,
    pub limits: RelayServerLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_build: Option<WebBuildDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayServerFeatures {
    pub pc_websocket: bool,
    pub device_websocket: bool,
    pub websocket_frames: bool,
    pub http_pair_bridge: bool,
    pub http_command_bridge: bool,
    pub static_room_assets: bool,
    pub push_registration: bool,
    pub push_dispatch: bool,
    pub push_provider_configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayServerLimits {
    pub room_ttl_ms: i64,
    pub bridge_timeout_ms: i64,
    pub heartbeat_timeout_ms: i64,
    pub max_rooms: usize,
    pub max_total_connections: usize,
    pub max_pending_per_room: usize,
    pub max_connections_per_room: usize,
    pub max_devices_per_room: usize,
    pub max_body_bytes: usize,
    pub rate_limit_window_ms: i64,
    pub max_requests_per_window_per_room: usize,
    pub max_queue_bytes_per_connection: usize,
    pub max_bandwidth_bytes_per_window: usize,
    pub max_push_installations: usize,
    pub max_push_dedup_entries: usize,
    pub push_adapter_timeout_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayServerStartupError {
    pub code: &'static str,
    pub message: &'static str,
}

impl std::fmt::Display for RelayServerStartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RelayServerStartupError {}

#[derive(Clone)]
struct RelayWebAssets {
    root: PathBuf,
    descriptor: WebBuildDescriptor,
}

impl RelayWebAssets {
    fn load(root: &FsPath) -> Result<Self, RelayServerStartupError> {
        let metadata = fs::symlink_metadata(root).map_err(|_| web_assets_missing())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(web_assets_invalid());
        }
        let root = root.canonicalize().map_err(|_| web_assets_invalid())?;
        for relative in WEB_REQUIRED_ASSETS {
            let path = root.join(relative);
            let metadata = fs::symlink_metadata(&path).map_err(|_| web_assets_missing())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(web_assets_invalid());
            }
            let resolved = path.canonicalize().map_err(|_| web_assets_invalid())?;
            if !resolved.starts_with(&root) {
                return Err(web_assets_invalid());
            }
        }
        let descriptor: WebBuildDescriptor = serde_json::from_slice(
            &fs::read(root.join("build.json")).map_err(|_| web_assets_missing())?,
        )
        .map_err(|_| web_assets_invalid())?;
        if !descriptor.has_valid_identity(false)
            || descriptor.package_version != env!("CARGO_PKG_VERSION")
        {
            return Err(web_assets_incompatible());
        }
        if option_env!("VIBEX_RELAY_WEB_BUILD_ID")
            .is_some_and(|expected| expected != descriptor.build_id)
            || option_env!("VIBEX_RELAY_WEB_GIT_COMMIT")
                .is_some_and(|expected| expected != descriptor.git_commit)
        {
            return Err(web_assets_incompatible());
        }
        verify_web_asset_hashes(&root, &descriptor)?;
        Ok(Self { root, descriptor })
    }
}

fn verify_web_asset_hashes(
    root: &FsPath,
    descriptor: &WebBuildDescriptor,
) -> Result<(), RelayServerStartupError> {
    let wasm = fs::read(root.join("pkg/vibex_web_bg.wasm")).map_err(|_| web_assets_missing())?;
    let glue = fs::read(root.join("pkg/vibex_web.js")).map_err(|_| web_assets_missing())?;
    let mut static_hash = Sha256::new();
    for relative in WEB_STATIC_IDENTITY_ASSETS {
        let mut bytes = fs::read(root.join(relative)).map_err(|_| web_assets_missing())?;
        if *relative == "service-worker.js" {
            let source = String::from_utf8(bytes).map_err(|_| web_assets_incompatible())?;
            if !source.contains(&descriptor.build_id) {
                return Err(web_assets_incompatible());
            }
            bytes = source
                .replace(&descriptor.build_id, "__VIBEX_BUILD_ID__")
                .into_bytes();
        }
        static_hash.update(relative.as_bytes());
        static_hash.update(b"\0");
        static_hash.update(bytes);
        static_hash.update(b"\0");
    }
    if sha256_hex(&wasm) != descriptor.wasm_sha256
        || sha256_hex(&glue) != descriptor.glue_sha256
        || format!("{:x}", static_hash.finalize()) != descriptor.static_sha256
    {
        return Err(web_assets_incompatible());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn web_assets_missing() -> RelayServerStartupError {
    RelayServerStartupError {
        code: "relay_web_assets_missing",
        message: "the configured WebUI release is missing required assets",
    }
}

fn web_assets_invalid() -> RelayServerStartupError {
    RelayServerStartupError {
        code: "relay_web_assets_invalid",
        message: "the configured WebUI asset root is invalid",
    }
}

fn web_assets_incompatible() -> RelayServerStartupError {
    RelayServerStartupError {
        code: "relay_web_assets_incompatible",
        message: "the configured WebUI release identity is incompatible",
    }
}

#[derive(Clone)]
pub struct RelayServerState {
    inner: std::sync::Arc<RelayServerInner>,
}

struct WebSocketConnectionGuard {
    state: RelayServerState,
}

impl Drop for WebSocketConnectionGuard {
    fn drop(&mut self) {
        self.state
            .inner
            .active_connections
            .fetch_sub(1, Ordering::AcqRel);
    }
}

struct RelayServerInner {
    config: RelayServerConfig,
    web_assets: Option<RelayWebAssets>,
    started_at_ms: i64,
    active_connections: AtomicUsize,
    rooms: Mutex<HashMap<RelayRoomId, RoomState>>,
    push: Mutex<PushState>,
    push_client: reqwest::Client,
}

#[derive(Default)]
struct PushState {
    registrations: HashMap<String, RelayPushRegistration>,
    delivered: HashMap<PushDedupKey, i64>,
    pending: HashMap<PushDedupKey, i64>,
}

type PushDedupKey = (String, String);

/// Server-to-adapter contract. The operator-owned adapter translates this
/// opaque request into WebPush, APNs, or FCM provider protocol calls.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPushAdapterRequest {
    pub registration: RelayPushRegistration,
    pub notification: RelayOpaqueNotification,
}

struct RoomState {
    connection_id: RelayConnectionId,
    pc_peer_id: RelayPeerId,
    pc_sender: mpsc::Sender<String>,
    pc_queued_bytes: std::sync::Arc<AtomicUsize>,
    disconnect_tx: watch::Sender<()>,
    peers: HashMap<RelayPeerId, DeviceRoute>,
    created_at_ms: i64,
    last_seen_at_ms: i64,
    pending: HashMap<CorrelationId, PendingBridgeRequest>,
    rate_window: RateWindow,
    bandwidth_window: RateWindow,
}

struct DeviceRoute {
    sender: mpsc::Sender<String>,
    queued_bytes: std::sync::Arc<AtomicUsize>,
    bandwidth_window: RateWindow,
    connection_id: RelayConnectionId,
}

struct PendingBridgeRequest {
    created_at_ms: i64,
    responder: oneshot::Sender<RelayControlMessage>,
}

#[derive(Debug, Clone)]
struct RateWindow {
    started_at_ms: i64,
    count: usize,
}

#[derive(Debug)]
struct RelayServerError {
    status: StatusCode,
    code: RelayErrorCode,
    message: &'static str,
    correlation_id: Option<CorrelationId>,
    retryable: bool,
}

pub fn build_router(config: RelayServerConfig) -> RelayServerRouter {
    try_build_router(config).expect("Relay Web asset configuration is valid")
}

pub fn try_build_router(
    config: RelayServerConfig,
) -> Result<RelayServerRouter, RelayServerStartupError> {
    Ok(build_router_with_state(RelayServerState::try_new(config)?))
}

pub fn build_router_with_state(state: RelayServerState) -> RelayServerRouter {
    let max_body_bytes = state.config().max_body_bytes;
    Router::new()
        .route("/health", get(health))
        .route("/api/info", get(info))
        .route("/ws", get(ws))
        .route("/api/push/registrations", post(register_push))
        .route("/api/push/dispatch", post(dispatch_push))
        .route("/api/rooms/{room_id}/pair", post(pair))
        .route("/api/rooms/{room_id}/command", post(command))
        .route("/", get(static_index))
        .route("/{*path}", get(static_asset))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .layer(middleware::from_fn(cors))
        .with_state(state)
}

/// Browser peers (Web app origins and the Capacitor `https://localhost`
/// WebView) reach the zero-knowledge Relay cross-origin. The Relay carries
/// only E2EE payloads, uses no cookies or ambient credentials, and is already
/// reachable by any non-browser client, so a wildcard non-credentialed CORS
/// policy adds browser reachability without widening the trust model.
async fn cors(request: axum::extract::Request, next: Next) -> Response {
    let preflight = request.method() == Method::OPTIONS;
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    response
}

impl RelayServerState {
    pub fn new(config: RelayServerConfig) -> Self {
        Self::try_new(config).expect("Relay Web asset configuration is valid")
    }

    pub fn try_new(config: RelayServerConfig) -> Result<Self, RelayServerStartupError> {
        let web_assets = config
            .web_static_dir
            .as_deref()
            .map(RelayWebAssets::load)
            .transpose()?;
        Ok(Self {
            inner: std::sync::Arc::new(RelayServerInner {
                config,
                web_assets,
                started_at_ms: unix_timestamp_ms(),
                active_connections: AtomicUsize::new(0),
                rooms: Mutex::new(HashMap::new()),
                push: Mutex::new(PushState::default()),
                push_client: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("Relay push HTTP client configuration is valid"),
            }),
        })
    }

    fn config(&self) -> &RelayServerConfig {
        &self.inner.config
    }

    fn reserve_connection(&self) -> Result<WebSocketConnectionGuard, RelayServerError> {
        let limit = self.config().max_total_connections;
        let mut current = self.inner.active_connections.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(1) else {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::ConnectionLimit,
                    "relay websocket connection capacity is exhausted",
                ));
            };
            if next > limit {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::ConnectionLimit,
                    "relay websocket connection capacity is exhausted",
                ));
            }
            match self.inner.active_connections.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(WebSocketConnectionGuard {
                        state: self.clone(),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    async fn health(&self) -> RelayHealthStatus {
        let mut rooms = self.inner.rooms.lock().await;
        self.prune_locked(&mut rooms, unix_timestamp_ms());
        RelayHealthStatus {
            status: RelayServerStatus::Ok,
            protocol_version: RelayProtocolVersion::foundation(),
            uptime_ms: unix_timestamp_ms() - self.inner.started_at_ms,
            active_rooms: rooms.len(),
            active_connections: self.inner.active_connections.load(Ordering::Acquire),
            pending_bridge_requests: rooms.values().map(|room| room.pending.len()).sum(),
        }
    }

    fn info(&self) -> RelayServerInfo {
        let config = self.config();
        RelayServerInfo {
            service_name: config.service_name.clone(),
            server_version: config.server_version.clone(),
            protocol_version: RelayProtocolVersion::foundation(),
            features: RelayServerFeatures {
                pc_websocket: true,
                device_websocket: true,
                websocket_frames: true,
                http_pair_bridge: true,
                http_command_bridge: true,
                static_room_assets: self.inner.web_assets.is_some(),
                push_registration: config.push_adapter_configured(),
                push_dispatch: config.push_adapter_configured(),
                push_provider_configured: config.push_adapter_configured(),
            },
            limits: RelayServerLimits {
                room_ttl_ms: config.room_ttl_ms,
                bridge_timeout_ms: config.bridge_timeout_ms,
                heartbeat_timeout_ms: config.heartbeat_timeout_ms,
                max_rooms: config.max_rooms,
                max_total_connections: config.max_total_connections,
                max_pending_per_room: config.max_pending_per_room,
                max_connections_per_room: config.max_connections_per_room,
                max_devices_per_room: config.max_devices_per_room,
                max_body_bytes: config.max_body_bytes,
                rate_limit_window_ms: config.rate_limit_window_ms,
                max_requests_per_window_per_room: config.max_requests_per_window_per_room,
                max_queue_bytes_per_connection: config.max_queue_bytes_per_connection,
                max_bandwidth_bytes_per_window: config.max_bandwidth_bytes_per_window,
                max_push_installations: config.max_push_installations,
                max_push_dedup_entries: config.max_push_dedup_entries,
                push_adapter_timeout_ms: config.push_adapter_timeout_ms,
            },
            web_build: self
                .inner
                .web_assets
                .as_ref()
                .map(|assets| assets.descriptor.clone()),
        }
    }

    async fn register_room(
        &self,
        room_id: RelayRoomId,
        peer_id: RelayPeerId,
        pc_sender: mpsc::Sender<String>,
        pc_queued_bytes: std::sync::Arc<AtomicUsize>,
    ) -> Result<RelayConnectionId, RelayServerError> {
        let now = unix_timestamp_ms();
        let mut rooms = self.inner.rooms.lock().await;
        self.prune_locked(&mut rooms, now);

        if rooms.contains_key(&room_id) {
            return Err(RelayServerError::new(
                StatusCode::CONFLICT,
                RelayErrorCode::InvalidRoom,
                "relay room already has an active PC connection",
            ));
        }
        if rooms.len() >= self.config().max_rooms {
            return Err(RelayServerError::new(
                StatusCode::CONFLICT,
                RelayErrorCode::InvalidRoom,
                "relay server room capacity is exhausted",
            ));
        }

        let connection_id = RelayConnectionId::new();
        let (disconnect_tx, _) = watch::channel(());
        rooms.insert(
            room_id.clone(),
            RoomState {
                connection_id: connection_id.clone(),
                pc_peer_id: peer_id.clone(),
                pc_sender,
                pc_queued_bytes,
                disconnect_tx,
                peers: HashMap::new(),
                created_at_ms: now,
                last_seen_at_ms: now,
                pending: HashMap::new(),
                rate_window: RateWindow {
                    started_at_ms: now,
                    count: 0,
                },
                bandwidth_window: RateWindow {
                    started_at_ms: now,
                    count: 0,
                },
            },
        );

        info!(
            room_id = room_id.as_str(),
            connection_id = connection_id.as_str(),
            pc_peer_id = peer_id.as_str(),
            "relay room registered"
        );

        Ok(connection_id)
    }

    async fn register_device(
        &self,
        room_id: RelayRoomId,
        peer_id: RelayPeerId,
        sender: mpsc::Sender<String>,
        queued_bytes: std::sync::Arc<AtomicUsize>,
    ) -> Result<(RelayConnectionId, watch::Receiver<()>), RelayServerError> {
        let now = unix_timestamp_ms();
        let mut rooms = self.inner.rooms.lock().await;
        self.prune_locked(&mut rooms, now);
        let room = rooms.get_mut(&room_id).ok_or_else(|| {
            RelayServerError::new(
                StatusCode::NOT_FOUND,
                RelayErrorCode::InvalidRoom,
                "relay room has no active PC connection",
            )
        })?;
        if room.peers.len() >= self.config().max_devices_per_room {
            return Err(RelayServerError::new(
                StatusCode::CONFLICT,
                RelayErrorCode::ConnectionLimit,
                "relay room device connection capacity is exhausted",
            ));
        }
        if peer_id == room.pc_peer_id || room.peers.contains_key(&peer_id) {
            return Err(RelayServerError::new(
                StatusCode::CONFLICT,
                RelayErrorCode::InvalidRoom,
                "relay peer is already connected to this room",
            ));
        }
        let connection_id = RelayConnectionId::new();
        let disconnect_rx = room.disconnect_tx.subscribe();
        room.peers.insert(
            peer_id.clone(),
            DeviceRoute {
                sender,
                queued_bytes,
                bandwidth_window: RateWindow {
                    started_at_ms: now,
                    count: 0,
                },
                connection_id: connection_id.clone(),
            },
        );
        room.last_seen_at_ms = now;
        info!(
            room_id = room_id.as_str(),
            peer_id = peer_id.as_str(),
            "relay device registered"
        );
        Ok((connection_id, disconnect_rx))
    }

    async fn unregister_device(
        &self,
        room_id: &RelayRoomId,
        peer_id: &RelayPeerId,
        connection_id: &RelayConnectionId,
    ) {
        let mut rooms = self.inner.rooms.lock().await;
        if let Some(room) = rooms.get_mut(room_id)
            && room
                .peers
                .get(peer_id)
                .is_some_and(|route| &route.connection_id == connection_id)
        {
            room.peers.remove(peer_id);
            room.last_seen_at_ms = unix_timestamp_ms();
        }
    }

    async fn route_peer_message(&self, message: RelayPeerMessage) -> Result<(), RelayServerError> {
        let encoded = serde_json::to_string(&message).map_err(|_| {
            RelayServerError::new(
                StatusCode::BAD_REQUEST,
                RelayErrorCode::InvalidFrame,
                "relay peer message could not be serialized",
            )
        })?;
        if encoded.len() > self.config().max_body_bytes {
            return Err(RelayServerError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                RelayErrorCode::InvalidFrame,
                "relay peer message exceeds the frame limit",
            ));
        }
        let mut rooms = self.inner.rooms.lock().await;
        let room = rooms.get_mut(&message.room_id).ok_or_else(|| {
            RelayServerError::new(
                StatusCode::NOT_FOUND,
                RelayErrorCode::InvalidRoom,
                "relay room is not connected",
            )
        })?;
        if let RelayControlMessage::Encrypted(frame) = &message.message
            && (frame.room_id != message.room_id
                || frame.sender_peer_id != message.sender_peer_id
                || frame.recipient_peer_id != message.recipient_peer_id)
        {
            return Err(RelayServerError::new(
                StatusCode::BAD_REQUEST,
                RelayErrorCode::InvalidFrame,
                "relay encrypted frame routing metadata did not match its peer envelope",
            ));
        }
        let now = unix_timestamp_ms();
        if message.sender_peer_id == room.pc_peer_id {
            let route = room
                .peers
                .get_mut(&message.recipient_peer_id)
                .ok_or_else(|| {
                    RelayServerError::new(
                        StatusCode::NOT_FOUND,
                        RelayErrorCode::PeerNotFound,
                        "relay recipient device is not connected",
                    )
                })?;
            if !allow_bandwidth(
                &mut route.bandwidth_window,
                now,
                encoded.len(),
                self.config(),
            ) {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::BandwidthLimit,
                    "relay device bandwidth limit exceeded",
                ));
            }
            let size = encoded.len();
            if !reserve_queue_bytes(
                &route.queued_bytes,
                size,
                self.config().max_queue_bytes_per_connection,
            ) {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "relay device queue limit exceeded",
                ));
            }
            if route.sender.try_send(encoded).is_err() {
                release_queue_bytes(&route.queued_bytes, size);
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "relay device queue is full",
                ));
            }
        } else if message.recipient_peer_id == room.pc_peer_id {
            if !allow_bandwidth(
                &mut room.bandwidth_window,
                now,
                encoded.len(),
                self.config(),
            ) {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::BandwidthLimit,
                    "relay PC bandwidth limit exceeded",
                ));
            }
            let size = encoded.len();
            if !reserve_queue_bytes(
                &room.pc_queued_bytes,
                size,
                self.config().max_queue_bytes_per_connection,
            ) {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "relay PC queue limit exceeded",
                ));
            }
            if room.pc_sender.try_send(encoded).is_err() {
                release_queue_bytes(&room.pc_queued_bytes, size);
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "relay PC queue is full",
                ));
            }
        } else {
            return Err(RelayServerError::new(
                StatusCode::BAD_REQUEST,
                RelayErrorCode::InvalidFrame,
                "relay peer message sender or recipient is not registered",
            ));
        }
        room.last_seen_at_ms = unix_timestamp_ms();
        Ok(())
    }

    async fn unregister_room(&self, room_id: &RelayRoomId, connection_id: &RelayConnectionId) {
        let mut rooms = self.inner.rooms.lock().await;
        if rooms
            .get(room_id)
            .is_some_and(|room| &room.connection_id == connection_id)
        {
            rooms.remove(room_id);
            info!(
                room_id = room_id.as_str(),
                connection_id = connection_id.as_str(),
                "relay room unregistered"
            );
        }
    }

    async fn touch_room(&self, room_id: &RelayRoomId, connection_id: &RelayConnectionId) {
        let mut rooms = self.inner.rooms.lock().await;
        if let Some(room) = rooms.get_mut(room_id)
            && &room.connection_id == connection_id
        {
            room.last_seen_at_ms = unix_timestamp_ms();
        }
    }

    async fn is_pc_peer(&self, room_id: &RelayRoomId, peer_id: &RelayPeerId) -> bool {
        self.inner
            .rooms
            .lock()
            .await
            .get(room_id)
            .is_some_and(|room| &room.pc_peer_id == peer_id)
    }

    async fn bridge(
        &self,
        room_id: RelayRoomId,
        correlation_id: CorrelationId,
        kind: RelayFrameKind,
        message: RelayControlMessage,
    ) -> Result<RelayControlMessage, RelayServerError> {
        let (response_rx, timeout_ms) = {
            let now = unix_timestamp_ms();
            let mut rooms = self.inner.rooms.lock().await;
            self.prune_locked(&mut rooms, now);
            let config = self.config();
            let room = rooms.get_mut(&room_id).ok_or_else(|| {
                RelayServerError::new(
                    StatusCode::NOT_FOUND,
                    RelayErrorCode::InvalidRoom,
                    "relay room is not connected",
                )
            })?;

            if !room.accept_rate(now, config) {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::InvalidCorrelation,
                    "relay room request rate limit exceeded",
                )
                .with_correlation_id(correlation_id));
            }

            if room.pending.len() >= config.max_pending_per_room {
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::InvalidCorrelation,
                    "relay room pending bridge limit exceeded",
                )
                .with_correlation_id(correlation_id));
            }

            let outbound = RelayBridgeMessage {
                correlation_id: correlation_id.clone(),
                room_id: room_id.clone(),
                message,
            };
            let encoded = serde_json::to_string(&outbound).map_err(|_| {
                RelayServerError::new(
                    StatusCode::BAD_REQUEST,
                    RelayErrorCode::InvalidFrame,
                    "relay bridge message could not be serialized",
                )
            })?;
            let (response_tx, response_rx) = oneshot::channel();
            room.pending.insert(
                correlation_id.clone(),
                PendingBridgeRequest {
                    created_at_ms: now,
                    responder: response_tx,
                },
            );
            room.last_seen_at_ms = now;

            let size = encoded.len();
            if !reserve_queue_bytes(
                &room.pc_queued_bytes,
                size,
                config.max_queue_bytes_per_connection,
            ) {
                room.pending.remove(&correlation_id);
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "relay PC queue byte limit exceeded",
                )
                .with_correlation_id(correlation_id));
            }
            if room.pc_sender.try_send(encoded).is_err() {
                release_queue_bytes(&room.pc_queued_bytes, size);
                room.pending.remove(&correlation_id);
                return Err(RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "relay PC queue is full or closed",
                )
                .with_correlation_id(correlation_id));
            }

            debug!(
                room_id = room_id.as_str(),
                correlation_id = correlation_id.as_str(),
                kind = ?kind,
                "relay bridge request forwarded"
            );
            (response_rx, config.bridge_timeout_ms)
        };

        match timeout(Duration::from_millis(timeout_ms.max(1) as u64), response_rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(RelayServerError::new(
                StatusCode::GATEWAY_TIMEOUT,
                RelayErrorCode::InvalidCorrelation,
                "relay bridge response channel closed before a response arrived",
            )
            .retryable(true)
            .with_correlation_id(correlation_id)),
            Err(_) => {
                self.remove_pending(&room_id, &correlation_id).await;
                Err(RelayServerError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    RelayErrorCode::InvalidCorrelation,
                    "relay bridge timed out waiting for PC response",
                )
                .retryable(true)
                .with_correlation_id(correlation_id))
            }
        }
    }

    async fn resolve_bridge_response(&self, bridge: RelayBridgeMessage) {
        if !message_matches_correlation(&bridge.message, &bridge.correlation_id) {
            warn!(
                room_id = bridge.room_id.as_str(),
                correlation_id = bridge.correlation_id.as_str(),
                "relay bridge response ignored because inner correlation did not match"
            );
            return;
        }
        self.resolve_response(&bridge.room_id, &bridge.correlation_id, bridge.message)
            .await;
    }

    async fn resolve_response(
        &self,
        room_id: &RelayRoomId,
        correlation_id: &CorrelationId,
        message: RelayControlMessage,
    ) {
        let mut rooms = self.inner.rooms.lock().await;
        let Some(room) = rooms.get_mut(room_id) else {
            warn!(
                room_id = room_id.as_str(),
                correlation_id = correlation_id.as_str(),
                "relay bridge response ignored for unknown room"
            );
            return;
        };
        if !message_room_matches(&message, room_id) {
            warn!(
                room_id = room_id.as_str(),
                correlation_id = correlation_id.as_str(),
                "relay bridge response ignored because inner room did not match"
            );
            return;
        }
        let Some(pending) = room.pending.remove(correlation_id) else {
            warn!(
                room_id = room_id.as_str(),
                correlation_id = correlation_id.as_str(),
                "relay bridge response ignored for unknown correlation"
            );
            return;
        };
        room.last_seen_at_ms = unix_timestamp_ms();
        let _ = pending.responder.send(message);
    }

    async fn remove_pending(&self, room_id: &RelayRoomId, correlation_id: &CorrelationId) {
        let mut rooms = self.inner.rooms.lock().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.pending.remove(correlation_id);
        }
    }

    fn prune_locked(&self, rooms: &mut HashMap<RelayRoomId, RoomState>, now: i64) {
        let config = self.config();
        rooms.retain(|room_id, room| {
            room.prune_pending(now, config.bridge_timeout_ms);
            let ttl_expired =
                config.room_ttl_ms > 0 && now - room.created_at_ms > config.room_ttl_ms;
            let heartbeat_expired = config.heartbeat_timeout_ms > 0
                && now - room.last_seen_at_ms > config.heartbeat_timeout_ms;
            let keep = !ttl_expired && !heartbeat_expired;
            if !keep {
                info!(room_id = room_id.as_str(), "relay room expired");
            }
            keep
        });
    }
}

impl RoomState {
    fn accept_rate(&mut self, now: i64, config: &RelayServerConfig) -> bool {
        if config.rate_limit_window_ms <= 0 || config.max_requests_per_window_per_room == 0 {
            return true;
        }
        if now - self.rate_window.started_at_ms >= config.rate_limit_window_ms {
            self.rate_window.started_at_ms = now;
            self.rate_window.count = 0;
        }
        if self.rate_window.count >= config.max_requests_per_window_per_room {
            return false;
        }
        self.rate_window.count += 1;
        true
    }

    fn prune_pending(&mut self, now: i64, timeout_ms: i64) {
        self.pending
            .retain(|_, pending| now - pending.created_at_ms <= timeout_ms);
    }
}

fn allow_bandwidth(
    window: &mut RateWindow,
    now: i64,
    bytes: usize,
    config: &RelayServerConfig,
) -> bool {
    if config.rate_limit_window_ms <= 0 || config.max_bandwidth_bytes_per_window == 0 {
        return true;
    }
    if now - window.started_at_ms >= config.rate_limit_window_ms {
        window.started_at_ms = now;
        window.count = 0;
    }
    if window.count.saturating_add(bytes) > config.max_bandwidth_bytes_per_window {
        return false;
    }
    window.count = window.count.saturating_add(bytes);
    true
}

fn reserve_queue_bytes(counter: &AtomicUsize, size: usize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(size) else {
            return false;
        };
        if next > limit {
            return false;
        }
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn release_queue_bytes(counter: &AtomicUsize, size: usize) {
    let _ = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_sub(size))
    });
}

impl RelayServerError {
    const fn new(status: StatusCode, code: RelayErrorCode, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
            correlation_id: None,
            retryable: false,
        }
    }

    fn with_correlation_id(mut self, correlation_id: CorrelationId) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    const fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    fn relay_error(&self) -> RelayError {
        let mut error = RelayError::new(self.code, self.message).retryable(self.retryable);
        if let Some(correlation_id) = self.correlation_id.clone() {
            error = error.with_correlation_id(correlation_id);
        }
        error
    }
}

impl IntoResponse for RelayServerError {
    fn into_response(self) -> Response {
        (self.status, Json(self.relay_error())).into_response()
    }
}

async fn health(State(state): State<RelayServerState>) -> Json<RelayHealthStatus> {
    Json(state.health().await)
}

async fn info(State(state): State<RelayServerState>) -> Json<RelayServerInfo> {
    Json(state.info())
}

async fn static_index(State(state): State<RelayServerState>) -> Response {
    serve_static_path(&state, "index.html")
}

async fn static_asset(State(state): State<RelayServerState>, Path(path): Path<String>) -> Response {
    if is_reserved_static_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_static_path(&state, &path)
}

fn serve_static_path(state: &RelayServerState, request_path: &str) -> Response {
    let Some(assets) = state.inner.web_assets.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let relative = FsPath::new(request_path.trim_start_matches('/'));
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let candidate = assets.root.join(relative);
    let resolved = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return StatusCode::NOT_FOUND.into_response();
            }
            match candidate.canonicalize() {
                Ok(path) if path.starts_with(&assets.root) => path,
                _ => return StatusCode::NOT_FOUND.into_response(),
            }
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && relative.extension().is_none() =>
        {
            assets.root.join("index.html")
        }
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let bytes = match fs::read(&resolved) {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(content_type_for_path(&resolved)),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for_path(&resolved)),
    );
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cross-origin-resource-policy", "same-origin"),
    ] {
        response.headers_mut().insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

fn is_reserved_static_path(path: &str) -> bool {
    ["api", "ws", "health"].iter().any(|prefix| {
        path.eq_ignore_ascii_case(prefix)
            || path
                .get(..prefix.len() + 1)
                .is_some_and(|value| value.eq_ignore_ascii_case(&format!("{prefix}/")))
    })
}

fn content_type_for_path(path: &FsPath) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json; charset=utf-8",
        Some("webmanifest") => "application/manifest+json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn cache_control_for_path(path: &FsPath) -> &'static str {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("index.html" | "offline.html" | "build.json" | "service-worker.js") => "no-cache",
        Some("manifest.webmanifest") => "public, max-age=300, must-revalidate",
        _ => "public, max-age=3600",
    }
}

async fn register_push(
    State(state): State<RelayServerState>,
    headers: HeaderMap,
    body: Result<Json<RelayPushRegistration>, JsonRejection>,
) -> Response {
    if let Err(error) = require_push_provider_and_auth(&state, &headers) {
        return error.into_response();
    }
    let registration = match decode_json_body(body) {
        Ok(registration) => registration,
        Err(error) => return error.into_response(),
    };
    if !valid_opaque_id(&registration.installation_id, 256)
        || registration.provider_token.trim().is_empty()
        || registration.provider_token.len() > 4096
    {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidFrame,
            "push registration is invalid",
        )
        .into_response();
    }
    if state
        .config()
        .push_provider
        .is_some_and(|provider| provider != registration.provider)
    {
        return RelayServerError::new(
            StatusCode::CONFLICT,
            RelayErrorCode::UnsupportedProtocol,
            "push registration provider does not match the configured adapter",
        )
        .into_response();
    }
    let mut push = state.inner.push.lock().await;
    if !push
        .registrations
        .contains_key(&registration.installation_id)
        && push.registrations.len() >= state.config().max_push_installations
    {
        return RelayServerError::new(
            StatusCode::TOO_MANY_REQUESTS,
            RelayErrorCode::ConnectionLimit,
            "push installation limit was reached",
        )
        .into_response();
    }
    push.registrations
        .insert(registration.installation_id.clone(), registration);
    Json(serde_json::json!({
        "registered": true,
        "providerConfigured": state.config().push_adapter_configured(),
    }))
    .into_response()
}

async fn dispatch_push(
    State(state): State<RelayServerState>,
    headers: HeaderMap,
    body: Result<Json<RelayOpaqueNotification>, JsonRejection>,
) -> Response {
    if let Err(error) = require_push_provider_and_auth(&state, &headers) {
        return error.into_response();
    }
    let notification = match decode_json_body(body) {
        Ok(notification) => notification,
        Err(error) => return error.into_response(),
    };
    let now = unix_timestamp_ms();
    if !valid_opaque_id(&notification.notification_id, 256)
        || !valid_opaque_id(&notification.installation_id, 256)
        || !valid_opaque_id(&notification.opaque_locator, 512)
        || notification.expires_at_ms <= now
        || notification.expires_at_ms > now + 24 * 60 * 60 * 1000
        || notification
            .ciphertext
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
    {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidFrame,
            "opaque push notification is invalid or expired",
        )
        .into_response();
    }
    let provider_configured = state.config().push_adapter_configured();
    let notification_id = notification.notification_id.clone();
    let dedup_key = (
        notification.installation_id.clone(),
        notification_id.clone(),
    );
    let expires_at_ms = notification.expires_at_ms;
    let (registration, duplicate) = {
        let mut push = state.inner.push.lock().await;
        push.delivered
            .retain(|_, expires_at_ms| *expires_at_ms > now);
        push.pending.retain(|_, expires_at_ms| *expires_at_ms > now);
        let duplicate =
            push.delivered.contains_key(&dedup_key) || push.pending.contains_key(&dedup_key);
        let registration = push
            .registrations
            .get(&notification.installation_id)
            .cloned();
        if !duplicate && registration.is_some() {
            if push.delivered.len().saturating_add(push.pending.len())
                >= state.config().max_push_dedup_entries
            {
                return RelayServerError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    RelayErrorCode::QueueLimit,
                    "push deduplication limit was reached",
                )
                .into_response();
            }
            push.pending.insert(dedup_key.clone(), expires_at_ms);
        }
        (registration, duplicate)
    };
    if duplicate {
        return Json(RelayPushDispatchResult {
            accepted: provider_configured && registration.is_some(),
            provider_configured,
            duplicate,
            expires_at_ms,
        })
        .into_response();
    }
    let Some(registration) = registration else {
        return Json(RelayPushDispatchResult {
            accepted: false,
            provider_configured,
            duplicate: false,
            expires_at_ms,
        })
        .into_response();
    };
    let delivery = dispatch_to_push_adapter(&state, registration, notification).await;
    let mut push = state.inner.push.lock().await;
    push.pending.remove(&dedup_key);
    match delivery {
        Ok(()) => {
            push.delivered.insert(dedup_key, expires_at_ms);
            Json(RelayPushDispatchResult {
                accepted: true,
                provider_configured,
                duplicate: false,
                expires_at_ms,
            })
            .into_response()
        }
        Err(error) => error.into_response(),
    }
}

async fn dispatch_to_push_adapter(
    state: &RelayServerState,
    registration: RelayPushRegistration,
    notification: RelayOpaqueNotification,
) -> Result<(), RelayServerError> {
    let config = state.config();
    let adapter_url = config.push_adapter_url.as_deref().ok_or_else(|| {
        RelayServerError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            RelayErrorCode::PushProviderUnavailable,
            "relay push adapter is not configured",
        )
    })?;
    let adapter_auth_token = config.push_adapter_auth_token.as_deref().ok_or_else(|| {
        RelayServerError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            RelayErrorCode::PushProviderUnavailable,
            "relay push adapter authentication is not configured",
        )
    })?;
    let request = RelayPushAdapterRequest {
        registration,
        notification,
    };
    let timeout_ms = u64::try_from(config.push_adapter_timeout_ms).unwrap_or(60_000);
    let response = timeout(
        Duration::from_millis(timeout_ms),
        state
            .inner
            .push_client
            .post(adapter_url)
            .bearer_auth(adapter_auth_token)
            .json(&request)
            .send(),
    )
    .await
    .map_err(|_| {
        RelayServerError::new(
            StatusCode::BAD_GATEWAY,
            RelayErrorCode::PushProviderUnavailable,
            "relay push adapter timed out",
        )
        .retryable(true)
    })?
    .map_err(|_| {
        RelayServerError::new(
            StatusCode::BAD_GATEWAY,
            RelayErrorCode::PushProviderUnavailable,
            "relay push adapter is unreachable",
        )
        .retryable(true)
    })?;
    if !response.status().is_success() {
        return Err(RelayServerError::new(
            StatusCode::BAD_GATEWAY,
            RelayErrorCode::PushProviderUnavailable,
            "relay push adapter rejected the notification",
        )
        .retryable(response.status().is_server_error()));
    }
    Ok(())
}

fn require_push_provider_and_auth(
    state: &RelayServerState,
    headers: &HeaderMap,
) -> Result<(), RelayServerError> {
    let config = state.config();
    if !config.push_adapter_configured() {
        return Err(RelayServerError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            RelayErrorCode::PushProviderUnavailable,
            "relay push provider adapter is not configured",
        )
        .retryable(false));
    }
    let Some(expected) = config.push_auth_token.as_deref() else {
        return Err(RelayServerError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            RelayErrorCode::PushProviderUnavailable,
            "relay push authentication is not configured",
        ));
    };
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
        return Err(RelayServerError::new(
            StatusCode::UNAUTHORIZED,
            RelayErrorCode::PushAuthenticationRequired,
            "relay push endpoint requires a valid bearer token",
        ));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

fn valid_opaque_id(value: &str, max_len: usize) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed == value
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

async fn ws(State(state): State<RelayServerState>, upgrade: WebSocketUpgrade) -> Response {
    let guard = match state.reserve_connection() {
        Ok(guard) => guard,
        Err(error) => return error.into_response(),
    };
    let max_message_bytes = state.config().max_body_bytes;
    upgrade
        .max_message_size(max_message_bytes)
        .max_frame_size(max_message_bytes)
        .on_upgrade(move |socket| async move {
            let _guard = guard;
            handle_socket(socket, state).await;
        })
        .into_response()
}

async fn pair(
    State(state): State<RelayServerState>,
    Path(room_id): Path<String>,
    body: Result<Json<RelayControlMessage>, JsonRejection>,
) -> Response {
    let room_id = match parse_room_path(room_id) {
        Ok(room_id) => room_id,
        Err(err) => return err.into_response(),
    };
    let message = match decode_json_body(body) {
        Ok(message) => message,
        Err(err) => return err.into_response(),
    };
    let RelayControlMessage::Hello(hello) = message else {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidFrame,
            "relay pair endpoint requires a hello message",
        )
        .into_response();
    };
    if hello.protocol_version != RelayProtocolVersion::foundation() {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::UnsupportedProtocol,
            "unsupported relay protocol version",
        )
        .into_response();
    }
    if hello.room_id != room_id {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidRoom,
            "relay pair request room does not match the path room",
        )
        .into_response();
    }

    let bridge_correlation_id = CorrelationId::new();
    match state
        .bridge(
            room_id,
            bridge_correlation_id,
            RelayFrameKind::PairRequest,
            RelayControlMessage::Hello(hello),
        )
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn command(
    State(state): State<RelayServerState>,
    Path(room_id): Path<String>,
    body: Result<Json<RelayEncryptedFrame>, JsonRejection>,
) -> Response {
    let room_id = match parse_room_path(room_id) {
        Ok(room_id) => room_id,
        Err(err) => return err.into_response(),
    };
    let frame = match decode_json_body(body) {
        Ok(frame) => frame,
        Err(err) => return err.into_response(),
    };
    let Some(correlation_id) = frame.correlation_id.clone() else {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidCorrelation,
            "relay command frame requires a correlation id",
        )
        .into_response();
    };
    if frame.protocol_version != RelayProtocolVersion::foundation() {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::UnsupportedProtocol,
            "unsupported relay protocol version",
        )
        .with_correlation_id(correlation_id)
        .into_response();
    }
    if frame.room_id != room_id {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidRoom,
            "relay command frame room does not match the path room",
        )
        .with_correlation_id(correlation_id)
        .into_response();
    }
    if !matches!(
        frame.kind,
        RelayFrameKind::Command | RelayFrameKind::PairRequest
    ) {
        return RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidFrame,
            "relay command endpoint requires a command or pairing frame",
        )
        .with_correlation_id(correlation_id)
        .into_response();
    }

    match state
        .bridge(
            room_id,
            correlation_id,
            frame.kind,
            RelayControlMessage::Encrypted(frame),
        )
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn handle_socket(socket: WebSocket, state: RelayServerState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let first_frame = match timeout(
        Duration::from_millis(state.config().bridge_timeout_ms.max(1) as u64),
        ws_receiver.next(),
    )
    .await
    {
        Ok(Some(Ok(first_frame))) => first_frame,
        Ok(Some(Err(_))) | Ok(None) => return,
        Err(_) => {
            send_error_and_close(
                &mut ws_sender,
                RelayServerError::new(
                    StatusCode::REQUEST_TIMEOUT,
                    RelayErrorCode::InvalidFrame,
                    "relay websocket registration timed out",
                ),
            )
            .await;
            return;
        }
    };
    let first_text = match frame_text(first_frame) {
        Ok(text) => text,
        Err(err) => {
            send_error_and_close(&mut ws_sender, err).await;
            return;
        }
    };
    let registration = match parse_registration(&first_text) {
        Ok(registration) => registration,
        Err(err) => {
            send_error_and_close(&mut ws_sender, err).await;
            return;
        }
    };
    let (room_id, peer_id, role, hello) = registration;
    if role == RelayPeerRole::Device {
        handle_device_socket_parts(ws_sender, ws_receiver, state, room_id, peer_id, hello).await;
        return;
    }
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<String>(128);
    let queued_bytes = std::sync::Arc::new(AtomicUsize::new(0));
    let connection_id = match state
        .register_room(
            room_id.clone(),
            peer_id,
            outbound_tx.clone(),
            queued_bytes.clone(),
        )
        .await
    {
        Ok(connection_id) => connection_id,
        Err(err) => {
            send_error_and_close(&mut ws_sender, err).await;
            return;
        }
    };

    let writer = tokio::spawn(async move {
        while let Some(encoded) = outbound_rx.recv().await {
            let bytes = encoded.len();
            if ws_sender.send(Message::Text(encoded.into())).await.is_err() {
                break;
            }
            release_queue_bytes(&queued_bytes, bytes);
        }
    });

    while let Some(frame) = ws_receiver.next().await {
        match frame {
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Ok(frame) => {
                if let Ok(text) = frame_text(frame) {
                    handle_pc_text(
                        &state,
                        &room_id,
                        &connection_id,
                        &outbound_tx,
                        text.as_str(),
                    )
                    .await;
                }
            }
            Err(_) => break,
        }
    }

    state.unregister_room(&room_id, &connection_id).await;
    drop(outbound_tx);
    writer.abort();
}

async fn handle_device_socket_parts(
    mut ws_sender: futures_util::stream::SplitSink<WebSocket, Message>,
    mut ws_receiver: futures_util::stream::SplitStream<WebSocket>,
    state: RelayServerState,
    room_id: RelayRoomId,
    peer_id: RelayPeerId,
    hello: RelayHandshakeHello,
) {
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<String>(128);
    let queued_bytes = std::sync::Arc::new(AtomicUsize::new(0));
    let (connection_id, mut room_disconnect) = match state
        .register_device(
            room_id.clone(),
            peer_id.clone(),
            outbound_tx.clone(),
            queued_bytes.clone(),
        )
        .await
    {
        Ok(connection_id) => connection_id,
        Err(err) => {
            send_error_and_close(&mut ws_sender, err).await;
            return;
        }
    };
    let writer = tokio::spawn(async move {
        while let Some(encoded) = outbound_rx.recv().await {
            let bytes = encoded.len();
            if ws_sender.send(Message::Text(encoded.into())).await.is_err() {
                break;
            }
            release_queue_bytes(&queued_bytes, bytes);
        }
    });
    // Forward the registration as an opaque peer message; the PC owns the
    // actual pairing/session decision and returns Ready over this same route.
    let Some(pc_peer_id) = pc_peer_for_room(&state, &room_id).await else {
        writer.abort();
        state
            .unregister_device(&room_id, &peer_id, &connection_id)
            .await;
        return;
    };
    let registration = RelayPeerMessage {
        room_id: room_id.clone(),
        sender_peer_id: peer_id.clone(),
        recipient_peer_id: pc_peer_id,
        message: RelayControlMessage::Hello(hello),
    };
    if state.route_peer_message(registration).await.is_err() {
        writer.abort();
        state
            .unregister_device(&room_id, &peer_id, &connection_id)
            .await;
        return;
    }
    loop {
        tokio::select! {
            _ = room_disconnect.changed() => break,
            frame = ws_receiver.next() => {
                let Some(frame) = frame else {
                    break;
                };
                match frame {
                    Ok(Message::Text(text)) => {
                        if let Ok(message) = serde_json::from_str::<RelayPeerMessage>(text.as_str())
                            && message.room_id == room_id
                            && message.sender_peer_id == peer_id
                            && let Err(error) = state.route_peer_message(message.clone()).await
                        {
                            warn!(
                                code = error.code.as_str(),
                                message = error.message,
                                "relay device frame routing failed"
                            );
                            if let Ok(encoded) = serde_json::to_string(&RelayPeerMessage {
                                room_id: room_id.clone(),
                                sender_peer_id: pc_peer_for_room(&state, &room_id)
                                    .await
                                    .unwrap_or_default(),
                                recipient_peer_id: peer_id.clone(),
                                message: RelayControlMessage::Error(error.relay_error()),
                            }) {
                                let _ = outbound_tx.try_send(encoded);
                            }
                            break;
                        }
                    }
                    Ok(Message::Binary(bytes)) => {
                        if bytes.len() > state.config().max_body_bytes {
                            break;
                        }
                    }
                    Ok(Message::Ping(_)) => {}
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Close(_)) | Err(_) => break,
                }
            }
        }
    }
    writer.abort();
    state
        .unregister_device(&room_id, &peer_id, &connection_id)
        .await;
}

async fn pc_peer_for_room(state: &RelayServerState, room_id: &RelayRoomId) -> Option<RelayPeerId> {
    state
        .inner
        .rooms
        .lock()
        .await
        .get(room_id)
        .map(|room| room.pc_peer_id.clone())
}

async fn handle_pc_text(
    state: &RelayServerState,
    room_id: &RelayRoomId,
    connection_id: &RelayConnectionId,
    outbound_tx: &mpsc::Sender<String>,
    text: &str,
) {
    if let Ok(peer_message) = serde_json::from_str::<RelayPeerMessage>(text) {
        if peer_message.room_id == *room_id
            && state
                .is_pc_peer(room_id, &peer_message.sender_peer_id)
                .await
            && let Err(error) = state.route_peer_message(peer_message).await
        {
            warn!(code = error.code.as_str(), "relay PC frame routing failed");
        }
        return;
    }
    if let Ok(bridge) = serde_json::from_str::<RelayBridgeMessage>(text) {
        state.resolve_bridge_response(bridge).await;
        return;
    }

    let Ok(message) = serde_json::from_str::<RelayControlMessage>(text) else {
        warn!(
            room_id = room_id.as_str(),
            "relay websocket ignored invalid JSON frame"
        );
        return;
    };

    match message {
        RelayControlMessage::Heartbeat(heartbeat) if heartbeat.room_id == *room_id => {
            state.touch_room(room_id, connection_id).await;
            let ack = RelayControlMessage::HeartbeatAck(RelayHeartbeatAck {
                room_id: room_id.clone(),
                peer_id: heartbeat.peer_id,
                connection_id: heartbeat
                    .connection_id
                    .or_else(|| Some(connection_id.clone())),
                sequence: heartbeat.sequence,
                acknowledged_at_ms: unix_timestamp_ms(),
            });
            if let Ok(encoded) = serde_json::to_string(&ack) {
                let _ = outbound_tx.try_send(encoded);
            }
        }
        RelayControlMessage::Encrypted(frame) => {
            if frame.room_id == *room_id
                && let Some(correlation_id) = frame.correlation_id.clone()
            {
                state
                    .resolve_response(
                        room_id,
                        &correlation_id,
                        RelayControlMessage::Encrypted(frame),
                    )
                    .await;
            }
        }
        RelayControlMessage::Error(error) => {
            if let Some(correlation_id) = error.correlation_id.clone() {
                state
                    .resolve_response(room_id, &correlation_id, RelayControlMessage::Error(error))
                    .await;
            }
        }
        _ => {
            debug!(
                room_id = room_id.as_str(),
                "relay websocket ignored unhandled control message"
            );
        }
    }
}

async fn send_error_and_close(
    ws_sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    err: RelayServerError,
) {
    let message = RelayControlMessage::Error(err.relay_error());
    if let Ok(encoded) = serde_json::to_string(&message) {
        let _ = ws_sender.send(Message::Text(encoded.into())).await;
    }
    let _ = ws_sender.send(Message::Close(None)).await;
}

fn parse_registration(
    text: &str,
) -> Result<(RelayRoomId, RelayPeerId, RelayPeerRole, RelayHandshakeHello), RelayServerError> {
    match serde_json::from_str::<RelayControlMessage>(text) {
        Ok(RelayControlMessage::Hello(hello)) => {
            if hello.protocol_version != RelayProtocolVersion::foundation() {
                return Err(RelayServerError::new(
                    StatusCode::BAD_REQUEST,
                    RelayErrorCode::UnsupportedProtocol,
                    "unsupported relay protocol version",
                ));
            }
            if matches!(hello.role, RelayPeerRole::Unknown) {
                return Err(RelayServerError::new(
                    StatusCode::BAD_REQUEST,
                    RelayErrorCode::InvalidFrame,
                    "relay websocket registration role is unknown",
                ));
            }
            let role = hello.role;
            Ok((hello.room_id.clone(), hello.peer_id.clone(), role, hello))
        }
        _ => Err(RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidFrame,
            "relay websocket registration requires a hello message",
        )),
    }
}

fn parse_room_path(room_id: String) -> Result<RelayRoomId, RelayServerError> {
    RelayRoomId::parse(room_id).map_err(|_| {
        RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidRoom,
            "relay room id is invalid",
        )
    })
}

fn decode_json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, RelayServerError> {
    body.map(|Json(value)| value).map_err(|rejection| {
        let status = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        };
        RelayServerError::new(
            status,
            RelayErrorCode::InvalidFrame,
            "relay request body is invalid",
        )
    })
}

fn frame_text(frame: Message) -> Result<String, RelayServerError> {
    match frame {
        Message::Text(text) => Ok(text.to_string()),
        Message::Binary(bytes) => std::str::from_utf8(&bytes)
            .map(|text| text.to_string())
            .map_err(|_| {
                RelayServerError::new(
                    StatusCode::BAD_REQUEST,
                    RelayErrorCode::InvalidFrame,
                    "relay websocket binary frame was not valid UTF-8",
                )
            }),
        _ => Err(RelayServerError::new(
            StatusCode::BAD_REQUEST,
            RelayErrorCode::InvalidFrame,
            "relay websocket frame type is unsupported",
        )),
    }
}

fn message_matches_correlation(
    message: &RelayControlMessage,
    correlation_id: &CorrelationId,
) -> bool {
    match message {
        RelayControlMessage::Encrypted(frame) => {
            frame.correlation_id.as_ref() == Some(correlation_id)
        }
        RelayControlMessage::Error(error) => error
            .correlation_id
            .as_ref()
            .is_none_or(|inner| inner == correlation_id),
        RelayControlMessage::Ready(_) => true,
        _ => false,
    }
}

fn message_room_matches(message: &RelayControlMessage, room_id: &RelayRoomId) -> bool {
    match message {
        RelayControlMessage::Hello(hello) => &hello.room_id == room_id,
        RelayControlMessage::Ready(ready) => &ready.room_id == room_id,
        RelayControlMessage::Encrypted(frame) => &frame.room_id == room_id,
        RelayControlMessage::Heartbeat(heartbeat) => &heartbeat.room_id == room_id,
        RelayControlMessage::HeartbeatAck(ack) => &ack.room_id == room_id,
        RelayControlMessage::Error(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tokio::net::TcpListener;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tower::ServiceExt;
    use vibex_core::{
        RelayFrameId, RelayHandshakeHello, RelayHandshakeReady, RelayHeartbeat, RelaySessionId,
    };

    #[tokio::test]
    async fn health_and_info_report_protocol_counts_and_limits() {
        let router = build_router(test_config());

        let health_response = router
            .clone()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health_response.status(), StatusCode::OK);
        let health: RelayHealthStatus = json_body(health_response).await;
        assert_eq!(health.status, RelayServerStatus::Ok);
        assert_eq!(health.protocol_version, RelayProtocolVersion::foundation());
        assert_eq!(health.active_rooms, 0);

        let info_response = router
            .oneshot(Request::get("/api/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(info_response.status(), StatusCode::OK);
        let info: RelayServerInfo = json_body(info_response).await;
        assert!(info.features.pc_websocket);
        assert!(info.features.http_pair_bridge);
        assert!(info.features.http_command_bridge);
        assert!(!info.features.static_room_assets);
        assert!(info.web_build.is_none());
        assert_eq!(info.limits.max_connections_per_room, 1);
        assert_eq!(
            info.limits.max_total_connections,
            test_config().max_total_connections
        );
    }

    #[tokio::test]
    async fn unregistering_pc_room_notifies_connected_devices() {
        let state = RelayServerState::new(test_config());
        let room_id = RelayRoomId::new();
        let pc_peer_id = RelayPeerId::new();
        let (pc_sender, _) = mpsc::channel(1);
        let pc_connection = state
            .register_room(
                room_id.clone(),
                pc_peer_id,
                pc_sender,
                std::sync::Arc::new(AtomicUsize::new(0)),
            )
            .await
            .unwrap();
        let (device_sender, _) = mpsc::channel(1);
        let (_, mut disconnect) = state
            .register_device(
                room_id.clone(),
                RelayPeerId::new(),
                device_sender,
                std::sync::Arc::new(AtomicUsize::new(0)),
            )
            .await
            .unwrap();

        state.unregister_room(&room_id, &pc_connection).await;

        assert!(
            timeout(Duration::from_secs(1), disconnect.changed())
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn validated_web_assets_are_reported_and_served_with_safe_routing() {
        let directory = tempfile::tempdir().unwrap();
        let descriptor = write_test_web_build(directory.path(), "release");
        let mut config = test_config();
        config.web_static_dir = Some(directory.path().to_path_buf());
        let router = try_build_router(config).unwrap();

        let info_response = router
            .clone()
            .oneshot(Request::get("/api/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let info: RelayServerInfo = json_body(info_response).await;
        assert!(info.features.static_room_assets);
        assert_eq!(info.web_build, Some(descriptor.clone()));

        let root = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::OK);
        assert_eq!(
            root.headers().get(CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            root.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(root.headers().get(CACHE_CONTROL).unwrap(), "no-cache");
        assert_eq!(
            String::from_utf8_lossy(&to_bytes(root.into_body(), usize::MAX).await.unwrap()),
            "relay-web-index"
        );

        for (path, expected_content_type) in [
            ("/host.js", "text/javascript; charset=utf-8"),
            ("/pkg/vibex_web_bg.wasm", "application/wasm"),
            (
                "/manifest.webmanifest",
                "application/manifest+json; charset=utf-8",
            ),
        ] {
            let response = router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                expected_content_type,
                "{path}"
            );
        }

        let build = router
            .clone()
            .oneshot(Request::get("/build.json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(build.headers().get(CACHE_CONTROL).unwrap(), "no-cache");
        let advertised: WebBuildDescriptor =
            serde_json::from_slice(&to_bytes(build.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(advertised, descriptor);

        let head = router
            .clone()
            .oneshot(Request::head("/host.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert!(
            to_bytes(head.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );

        let navigation = router
            .clone()
            .oneshot(
                Request::get("/settings/remote")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(navigation.status(), StatusCode::OK);
        assert_eq!(
            String::from_utf8_lossy(&to_bytes(navigation.into_body(), usize::MAX).await.unwrap()),
            "relay-web-index"
        );

        for path in ["/missing.js", "/api/missing", "/ws/missing", "/pkg"] {
            let response = router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let post_navigation = router
            .clone()
            .oneshot(
                Request::post("/settings/remote")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_navigation.status(), StatusCode::METHOD_NOT_ALLOWED);
        let traversal = router
            .oneshot(
                Request::get("/%2e%2e/outside.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!traversal.status().is_success());
    }

    #[test]
    fn production_web_assets_fail_closed_when_missing_debug_or_tampered() {
        let missing = tempfile::tempdir().unwrap();
        let mut config = test_config();
        config.web_static_dir = Some(missing.path().to_path_buf());
        assert_eq!(
            try_build_router(config).unwrap_err().code,
            "relay_web_assets_missing"
        );

        let debug = tempfile::tempdir().unwrap();
        write_test_web_build(debug.path(), "debug");
        let mut config = test_config();
        config.web_static_dir = Some(debug.path().to_path_buf());
        assert_eq!(
            try_build_router(config).unwrap_err().code,
            "relay_web_assets_incompatible"
        );

        let tampered = tempfile::tempdir().unwrap();
        write_test_web_build(tampered.path(), "release");
        fs::write(tampered.path().join("host.js"), "tampered").unwrap();
        let mut config = test_config();
        config.web_static_dir = Some(tampered.path().to_path_buf());
        assert_eq!(
            try_build_router(config).unwrap_err().code,
            "relay_web_assets_incompatible"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn static_web_assets_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside_directory = tempfile::tempdir().unwrap();
        write_test_web_build(directory.path(), "release");
        let outside = outside_directory.path().join("relay-outside.txt");
        fs::write(&outside, "outside-secret-sentinel").unwrap();
        symlink(&outside, directory.path().join("escape.txt")).unwrap();
        let mut config = test_config();
        config.web_static_dir = Some(directory.path().to_path_buf());
        let response = try_build_router(config)
            .unwrap()
            .oneshot(Request::get("/escape.txt").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .contains("outside-secret-sentinel")
        );
    }

    #[tokio::test]
    async fn websocket_connections_and_frames_use_global_hard_limits() {
        let mut config = test_config();
        config.max_total_connections = 1;
        config.max_body_bytes = 1024;
        let router = build_router(config);
        let url = spawn_ws_server(router.clone()).await;
        let (mut first, _) = connect_async(url.clone()).await.unwrap();

        let second = connect_async(url.clone()).await;
        assert!(
            second.is_err(),
            "global websocket connection limit was bypassed"
        );

        let room_id = RelayRoomId::new();
        first
            .send(ClientMessage::Text(
                serde_json::to_string(&RelayControlMessage::Hello(RelayHandshakeHello::new(
                    room_id.clone(),
                    RelayPeerId::new(),
                    "pc-public",
                )))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        wait_for_active_rooms(router.clone(), 1).await;
        first
            .send(ClientMessage::Text("x".repeat(2048).into()))
            .await
            .unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(1), first.next())
            .await
            .expect("oversized websocket frame was not rejected");
        // Depending on the tungstenite version and where the frame-size
        // violation is observed, the peer may receive a protocol read error
        // instead of a decoded Close frame. Both are a bounded rejection; a
        // normal data frame would indicate that the hard limit was bypassed.
        assert!(matches!(
            closed,
            None | Some(Ok(ClientMessage::Close(_))) | Some(Err(_))
        ));
        wait_for_active_rooms(router, 0).await;
    }

    #[test]
    fn websocket_registration_rejects_unknown_peer_roles() {
        let hello = RelayHandshakeHello {
            role: RelayPeerRole::Unknown,
            ..RelayHandshakeHello::new(RelayRoomId::new(), RelayPeerId::new(), "public")
        };
        let error =
            parse_registration(&serde_json::to_string(&RelayControlMessage::Hello(hello)).unwrap())
                .unwrap_err();
        assert_eq!(error.code, RelayErrorCode::InvalidFrame);
    }

    #[tokio::test]
    async fn pair_request_bridges_http_to_pc_websocket_by_correlation() {
        let router = build_router(test_config());
        let url = spawn_ws_server(router.clone()).await;
        let room_id = RelayRoomId::new();
        let pc_peer_id = RelayPeerId::new();
        let (mut pc, _) = connect_async(url).await.unwrap();
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayControlMessage::Hello(RelayHandshakeHello::new(
                room_id.clone(),
                pc_peer_id.clone(),
                "pc-public",
            )))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        wait_for_active_rooms(router.clone(), 1).await;

        let mobile_hello = RelayControlMessage::Hello(RelayHandshakeHello::new(
            room_id.clone(),
            RelayPeerId::new(),
            "phone-public",
        ));
        let http_task = tokio::spawn(post_pair(router, room_id.clone(), mobile_hello));

        let bridge = next_bridge_message(&mut pc).await;
        assert_eq!(bridge.room_id, room_id);
        assert!(matches!(bridge.message, RelayControlMessage::Hello(_)));

        let ready = RelayControlMessage::Ready(RelayHandshakeReady::new(
            room_id.clone(),
            RelaySessionId::new(),
            pc_peer_id,
            "pc-public",
        ));
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayBridgeMessage {
                correlation_id: bridge.correlation_id,
                room_id,
                message: ready.clone(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

        let response = http_task.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let returned: RelayControlMessage = json_body(response).await;
        assert_eq!(returned, ready);
    }

    #[tokio::test]
    async fn command_request_forwards_opaque_encrypted_frame_and_response() {
        let router = build_router(test_config());
        let url = spawn_ws_server(router.clone()).await;
        let room_id = RelayRoomId::new();
        let (mut pc, _) = connect_async(url).await.unwrap();
        register_pc(&mut pc, room_id.clone()).await;
        wait_for_active_rooms(router.clone(), 1).await;

        let request_frame =
            encrypted_frame(room_id.clone(), RelayFrameKind::Command, "ciphertext-only");
        let correlation_id = request_frame.correlation_id.clone().unwrap();
        let http_task = tokio::spawn(post_command(router, room_id.clone(), request_frame.clone()));

        let bridge = next_bridge_message(&mut pc).await;
        let RelayControlMessage::Encrypted(forwarded) = bridge.message else {
            panic!("expected encrypted bridge message");
        };
        assert_eq!(forwarded, request_frame);
        assert_eq!(forwarded.correlation_id, Some(correlation_id.clone()));
        let forwarded_json = serde_json::to_string(&forwarded).unwrap();
        assert!(!forwarded_json.contains("sample prompt body"));
        assert!(!forwarded_json.contains("secret-auth-token"));

        let response_frame = encrypted_response_frame(
            room_id.clone(),
            correlation_id.clone(),
            "response-ciphertext",
        );
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayBridgeMessage {
                correlation_id,
                room_id,
                message: RelayControlMessage::Encrypted(response_frame.clone()),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

        let response = http_task.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let returned: RelayControlMessage = json_body(response).await;
        assert_eq!(returned, RelayControlMessage::Encrypted(response_frame));
    }

    #[tokio::test]
    async fn rejects_missing_room_duplicate_room_invalid_correlation_and_room_mismatch() {
        let router = build_router(test_config());
        let room_id = RelayRoomId::new();

        let missing = post_command(
            router.clone(),
            room_id.clone(),
            encrypted_frame(room_id.clone(), RelayFrameKind::Command, "ciphertext"),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let missing_error: RelayError = json_body(missing).await;
        assert_eq!(missing_error.code, RelayErrorCode::InvalidRoom);

        let url = spawn_ws_server(router.clone()).await;
        let (mut pc, _) = connect_async(url.clone()).await.unwrap();
        register_pc(&mut pc, room_id.clone()).await;
        wait_for_active_rooms(router.clone(), 1).await;
        let (mut duplicate, _) = connect_async(url).await.unwrap();
        duplicate
            .send(ClientMessage::Text(
                serde_json::to_string(&RelayControlMessage::Hello(RelayHandshakeHello::new(
                    room_id.clone(),
                    RelayPeerId::new(),
                    "duplicate-public",
                )))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let duplicate_error: RelayControlMessage = serde_json::from_str(
            duplicate
                .next()
                .await
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        assert!(matches!(duplicate_error, RelayControlMessage::Error(_)));

        let mut no_correlation =
            encrypted_frame(room_id.clone(), RelayFrameKind::Command, "ciphertext");
        no_correlation.correlation_id = None;
        let missing_correlation =
            post_command(router.clone(), room_id.clone(), no_correlation).await;
        assert_eq!(missing_correlation.status(), StatusCode::BAD_REQUEST);
        let error: RelayError = json_body(missing_correlation).await;
        assert_eq!(error.code, RelayErrorCode::InvalidCorrelation);

        let wrong_room_frame =
            encrypted_frame(RelayRoomId::new(), RelayFrameKind::Command, "ciphertext");
        let room_mismatch = post_command(router, room_id, wrong_room_frame).await;
        assert_eq!(room_mismatch.status(), StatusCode::BAD_REQUEST);
        let error: RelayError = json_body(room_mismatch).await;
        assert_eq!(error.code, RelayErrorCode::InvalidRoom);
    }

    #[tokio::test]
    async fn bridge_timeout_body_limit_pending_limit_and_rate_limit_are_structured() {
        let mut timeout_config = test_config();
        timeout_config.bridge_timeout_ms = 10;
        let timeout_router = build_router(timeout_config);
        let timeout_room = RelayRoomId::new();
        let url = spawn_ws_server(timeout_router.clone()).await;
        let (mut pc, _) = connect_async(url).await.unwrap();
        register_pc(&mut pc, timeout_room.clone()).await;
        wait_for_active_rooms(timeout_router.clone(), 1).await;
        let timeout_response = post_command(
            timeout_router,
            timeout_room.clone(),
            encrypted_frame(timeout_room, RelayFrameKind::Command, "ciphertext"),
        )
        .await;
        assert_eq!(timeout_response.status(), StatusCode::GATEWAY_TIMEOUT);

        let mut body_config = test_config();
        body_config.max_body_bytes = 8;
        let body_router = build_router(body_config);
        let body_response = body_router
            .oneshot(
                Request::post(format!(
                    "/api/rooms/{}/command",
                    RelayRoomId::new().as_str()
                ))
                .header("content-type", "application/json")
                .body(Body::from("{\"tooLarge\":true}"))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body_error: RelayError = json_body(body_response).await;
        assert_eq!(body_error.code, RelayErrorCode::InvalidFrame);

        let mut pending_config = test_config();
        pending_config.max_pending_per_room = 1;
        pending_config.bridge_timeout_ms = 50;
        let pending_router = build_router(pending_config);
        let pending_room = RelayRoomId::new();
        let url = spawn_ws_server(pending_router.clone()).await;
        let (mut pc, _) = connect_async(url).await.unwrap();
        register_pc(&mut pc, pending_room.clone()).await;
        wait_for_active_rooms(pending_router.clone(), 1).await;
        let first = tokio::spawn(post_command(
            pending_router.clone(),
            pending_room.clone(),
            encrypted_frame(pending_room.clone(), RelayFrameKind::Command, "first"),
        ));
        let _ = next_bridge_message(&mut pc).await;
        let second = post_command(
            pending_router,
            pending_room.clone(),
            encrypted_frame(pending_room, RelayFrameKind::Command, "second"),
        )
        .await;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        let second_error: RelayError = json_body(second).await;
        assert_eq!(second_error.code, RelayErrorCode::InvalidCorrelation);
        assert_eq!(first.await.unwrap().status(), StatusCode::GATEWAY_TIMEOUT);

        let mut rate_config = test_config();
        rate_config.max_requests_per_window_per_room = 1;
        rate_config.rate_limit_window_ms = 60_000;
        let rate_router = build_router(rate_config);
        let rate_room = RelayRoomId::new();
        let url = spawn_ws_server(rate_router.clone()).await;
        let (mut pc, _) = connect_async(url).await.unwrap();
        register_pc(&mut pc, rate_room.clone()).await;
        wait_for_active_rooms(rate_router.clone(), 1).await;
        let first = tokio::spawn(post_command(
            rate_router.clone(),
            rate_room.clone(),
            encrypted_frame(rate_room.clone(), RelayFrameKind::Command, "first"),
        ));
        let bridge = next_bridge_message(&mut pc).await;
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayBridgeMessage {
                correlation_id: bridge.correlation_id.clone(),
                room_id: rate_room.clone(),
                message: RelayControlMessage::Encrypted(encrypted_response_frame(
                    rate_room.clone(),
                    bridge.correlation_id,
                    "ok",
                )),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
        let rate_limited = post_command(
            rate_router,
            rate_room.clone(),
            encrypted_frame(rate_room, RelayFrameKind::Command, "second"),
        )
        .await;
        assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn heartbeat_acknowledges_and_stale_room_stops_accepting_requests() {
        let mut config = test_config();
        config.heartbeat_timeout_ms = 100;
        let router = build_router(config);
        let url = spawn_ws_server(router.clone()).await;
        let room_id = RelayRoomId::new();
        let pc_peer_id = RelayPeerId::new();
        let (mut pc, _) = connect_async(url).await.unwrap();
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayControlMessage::Hello(RelayHandshakeHello::new(
                room_id.clone(),
                pc_peer_id.clone(),
                "pc-public",
            )))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        wait_for_active_rooms(router.clone(), 1).await;
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayControlMessage::Heartbeat(RelayHeartbeat {
                room_id: room_id.clone(),
                peer_id: pc_peer_id,
                connection_id: None,
                sequence: 7,
                sent_at_ms: unix_timestamp_ms(),
            }))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        let ack: RelayControlMessage = serde_json::from_str(
            pc.next()
                .await
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        assert!(matches!(ack, RelayControlMessage::HeartbeatAck(_)));

        tokio::time::sleep(Duration::from_millis(120)).await;
        let stale = post_command(
            router,
            room_id.clone(),
            encrypted_frame(room_id, RelayFrameKind::Command, "ciphertext"),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn push_registration_and_dispatch_require_provider_and_bearer_auth() {
        let disabled = build_router(test_config());
        let disabled_response = post_push_registration(
            disabled,
            None,
            RelayPushRegistration {
                installation_id: "install-a".to_string(),
                provider: RelayNotificationProviderKind::WebPush,
                provider_token: "provider-token".to_string(),
            },
        )
        .await;
        assert_eq!(disabled_response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let disabled_error: RelayError = json_body(disabled_response).await;
        assert_eq!(disabled_error.code, RelayErrorCode::PushProviderUnavailable);

        let mut config = test_config();
        let (adapter_url, mut adapter_requests) = spawn_push_adapter(StatusCode::NO_CONTENT).await;
        config.push_provider = Some(RelayNotificationProviderKind::WebPush);
        config.push_auth_token = Some("operator-token-at-least-24-bytes".to_string());
        config.push_adapter_url = Some(adapter_url);
        config.push_adapter_auth_token = Some("adapter-token-at-least-24-bytes".to_string());
        let router = build_router(config);

        let unauthorized = post_push_registration(
            router.clone(),
            None,
            RelayPushRegistration {
                installation_id: "install-a".to_string(),
                provider: RelayNotificationProviderKind::WebPush,
                provider_token: "provider-token".to_string(),
            },
        )
        .await;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let unauthorized_error: RelayError = json_body(unauthorized).await;
        assert_eq!(
            unauthorized_error.code,
            RelayErrorCode::PushAuthenticationRequired
        );

        let registered = post_push_registration(
            router.clone(),
            Some("operator-token-at-least-24-bytes"),
            RelayPushRegistration {
                installation_id: "install-a".to_string(),
                provider: RelayNotificationProviderKind::WebPush,
                provider_token: "provider-token".to_string(),
            },
        )
        .await;
        assert_eq!(registered.status(), StatusCode::OK);
        let registered_json: serde_json::Value = json_body(registered).await;
        assert_eq!(registered_json["registered"], true);
        assert_eq!(registered_json["providerConfigured"], true);
        let config_debug = format!(
            "{:?}",
            RelayServerConfig {
                push_provider: Some(RelayNotificationProviderKind::WebPush),
                push_auth_token: Some("operator-token-at-least-24-bytes".to_string()),
                push_adapter_url: Some("http://127.0.0.1:1/push".to_string()),
                push_adapter_auth_token: Some("adapter-token-at-least-24-bytes".to_string()),
                ..test_config()
            }
        );
        assert!(!config_debug.contains("operator-token-at-least-24-bytes"));
        assert!(!config_debug.contains("adapter-token-at-least-24-bytes"));
        assert!(!config_debug.contains("127.0.0.1:1"));

        let notification = RelayOpaqueNotification {
            notification_id: "notification-a".to_string(),
            installation_id: "install-a".to_string(),
            opaque_locator: "opaque-session-ref".to_string(),
            expires_at_ms: unix_timestamp_ms() + 60_000,
            ciphertext: Some("opaque-ciphertext".to_string()),
        };
        let dispatched = post_push_dispatch(
            router.clone(),
            Some("operator-token-at-least-24-bytes"),
            notification.clone(),
        )
        .await;
        assert_eq!(dispatched.status(), StatusCode::OK);
        let result: RelayPushDispatchResult = json_body(dispatched).await;
        assert!(result.accepted);
        assert!(!result.duplicate);
        let adapter_request = tokio::time::timeout(Duration::from_secs(1), adapter_requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            adapter_request.authorization,
            "Bearer adapter-token-at-least-24-bytes"
        );
        assert_eq!(
            adapter_request.request.registration.provider_token,
            "provider-token"
        );
        assert_eq!(
            adapter_request.request.notification.opaque_locator,
            "opaque-session-ref"
        );

        let second_registration = post_push_registration(
            router.clone(),
            Some("operator-token-at-least-24-bytes"),
            RelayPushRegistration {
                installation_id: "install-b".to_string(),
                provider: RelayNotificationProviderKind::WebPush,
                provider_token: "provider-token-b".to_string(),
            },
        )
        .await;
        assert_eq!(second_registration.status(), StatusCode::OK);
        let second_installation = RelayOpaqueNotification {
            installation_id: "install-b".to_string(),
            ..notification.clone()
        };
        let second_dispatch = post_push_dispatch(
            router.clone(),
            Some("operator-token-at-least-24-bytes"),
            second_installation,
        )
        .await;
        assert_eq!(second_dispatch.status(), StatusCode::OK);
        let second_dispatch: RelayPushDispatchResult = json_body(second_dispatch).await;
        assert!(second_dispatch.accepted);
        assert!(!second_dispatch.duplicate);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), adapter_requests.recv())
                .await
                .unwrap()
                .is_some()
        );

        let duplicate = post_push_dispatch(
            router,
            Some("operator-token-at-least-24-bytes"),
            notification,
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::OK);
        let duplicate: RelayPushDispatchResult = json_body(duplicate).await;
        assert!(duplicate.duplicate);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), adapter_requests.recv())
                .await
                .is_err()
        );
    }

    #[test]
    fn push_adapter_config_requires_complete_secrets_and_secure_url() {
        let mut config = test_config();
        config.push_provider = Some(RelayNotificationProviderKind::Apns);
        config.push_auth_token = Some("operator-token-at-least-24-bytes".to_string());
        config.push_adapter_auth_token = Some("adapter-token-at-least-24-bytes".to_string());
        config.push_adapter_url = Some("http://adapter.example.test/push".to_string());
        assert!(config.validate().is_err());

        config.push_adapter_url = Some("http://127.0.0.1:8080/push".to_string());
        assert!(config.validate().is_ok());
        config.push_adapter_auth_token = None;
        assert!(config.validate().is_err());

        let mut partial_without_provider = test_config();
        partial_without_provider.push_auth_token =
            Some("operator-token-at-least-24-bytes".to_string());
        assert!(partial_without_provider.validate().is_err());
    }

    #[tokio::test]
    async fn push_adapter_failure_is_retryable_and_not_deduplicated() {
        let (adapter_url, mut adapter_requests) =
            spawn_push_adapter(StatusCode::SERVICE_UNAVAILABLE).await;
        let mut config = test_config();
        config.push_provider = Some(RelayNotificationProviderKind::Fcm);
        config.push_auth_token = Some("operator-token-at-least-24-bytes".to_string());
        config.push_adapter_url = Some(adapter_url);
        config.push_adapter_auth_token = Some("adapter-token-at-least-24-bytes".to_string());
        let router = build_router(config);
        let registration = RelayPushRegistration {
            installation_id: "install-retry".to_string(),
            provider: RelayNotificationProviderKind::Fcm,
            provider_token: "provider-token-retry".to_string(),
        };
        let registered = post_push_registration(
            router.clone(),
            Some("operator-token-at-least-24-bytes"),
            registration,
        )
        .await;
        assert_eq!(registered.status(), StatusCode::OK);
        let notification = RelayOpaqueNotification {
            notification_id: "notification-retry".to_string(),
            installation_id: "install-retry".to_string(),
            opaque_locator: "opaque-retry-ref".to_string(),
            expires_at_ms: unix_timestamp_ms() + 60_000,
            ciphertext: None,
        };

        for _ in 0..2 {
            let response = post_push_dispatch(
                router.clone(),
                Some("operator-token-at-least-24-bytes"),
                notification.clone(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let error: RelayError = json_body(response).await;
            assert_eq!(error.code, RelayErrorCode::PushProviderUnavailable);
            assert!(error.retryable);
        }
        assert!(adapter_requests.recv().await.is_some());
        assert!(adapter_requests.recv().await.is_some());
    }

    fn test_config() -> RelayServerConfig {
        RelayServerConfig {
            bridge_timeout_ms: 250,
            heartbeat_timeout_ms: 60_000,
            max_rooms: 8,
            max_pending_per_room: 4,
            max_body_bytes: 64 * 1024,
            rate_limit_window_ms: 1000,
            max_requests_per_window_per_room: 100,
            ..RelayServerConfig::default()
        }
    }

    fn write_test_web_build(root: &FsPath, profile: &str) -> WebBuildDescriptor {
        fs::create_dir_all(root.join("pkg")).unwrap();
        for relative in WEB_STATIC_IDENTITY_ASSETS {
            let contents = match *relative {
                "index.html" => "relay-web-index".to_string(),
                "service-worker.js" => "const BUILD_ID = \"__VIBEX_BUILD_ID__\";\n".to_string(),
                _ => format!("test asset: {relative}\n"),
            };
            fs::write(root.join(relative), contents).unwrap();
        }
        fs::write(root.join("pkg/vibex_web.js"), "test glue\n").unwrap();
        fs::write(root.join("pkg/vibex_web_bg.wasm"), b"\0asmtest").unwrap();

        let build_id = "a".repeat(24);
        let mut static_hash = Sha256::new();
        for relative in WEB_STATIC_IDENTITY_ASSETS {
            static_hash.update(relative.as_bytes());
            static_hash.update(b"\0");
            static_hash.update(fs::read(root.join(relative)).unwrap());
            static_hash.update(b"\0");
        }
        let descriptor = WebBuildDescriptor {
            schema_version: vibex_core::WEB_BUILD_SCHEMA_VERSION.to_string(),
            build_id: build_id.clone(),
            package_version: env!("CARGO_PKG_VERSION").to_string(),
            profile: profile.to_string(),
            git_commit: "b".repeat(40),
            wasm_sha256: sha256_hex(b"\0asmtest"),
            glue_sha256: sha256_hex(b"test glue\n"),
            static_sha256: format!("{:x}", static_hash.finalize()),
        };
        fs::write(
            root.join("build.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();
        let service_worker = fs::read_to_string(root.join("service-worker.js")).unwrap();
        fs::write(
            root.join("service-worker.js"),
            service_worker.replace("__VIBEX_BUILD_ID__", &build_id),
        )
        .unwrap();
        descriptor
    }

    struct ObservedPushAdapterRequest {
        authorization: String,
        request: RelayPushAdapterRequest,
    }

    async fn spawn_push_adapter(
        response_status: StatusCode,
    ) -> (String, mpsc::Receiver<ObservedPushAdapterRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel(4);
        let router = Router::new().route(
            "/push",
            post(
                move |headers: HeaderMap, Json(request): Json<RelayPushAdapterRequest>| {
                    let sender = sender.clone();
                    async move {
                        let authorization = headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        let _ = sender
                            .send(ObservedPushAdapterRequest {
                                authorization,
                                request,
                            })
                            .await;
                        response_status
                    }
                },
            ),
        );
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}/push"), receiver)
    }

    async fn spawn_ws_server(router: RelayServerRouter) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("ws://{addr}/ws")
    }

    async fn register_pc(
        pc: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        room_id: RelayRoomId,
    ) {
        pc.send(ClientMessage::Text(
            serde_json::to_string(&RelayControlMessage::Hello(RelayHandshakeHello::new(
                room_id,
                RelayPeerId::new(),
                "pc-public",
            )))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    }

    async fn wait_for_active_rooms(router: RelayServerRouter, expected: usize) {
        for _ in 0..50 {
            let response = router
                .clone()
                .oneshot(Request::get("/health").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let health: RelayHealthStatus = json_body(response).await;
            if health.active_rooms == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("relay room registration did not reach expected count");
    }

    async fn next_bridge_message(
        pc: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> RelayBridgeMessage {
        let message = tokio::time::timeout(Duration::from_secs(2), pc.next())
            .await
            .expect("timed out waiting for relay bridge websocket message")
            .unwrap()
            .unwrap();
        serde_json::from_str(message.into_text().unwrap().as_ref()).unwrap()
    }

    async fn post_pair(
        router: RelayServerRouter,
        room_id: RelayRoomId,
        message: RelayControlMessage,
    ) -> axum::response::Response {
        router
            .oneshot(
                Request::post(format!("/api/rooms/{}/pair", room_id.as_str()))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&message).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_command(
        router: RelayServerRouter,
        room_id: RelayRoomId,
        frame: RelayEncryptedFrame,
    ) -> axum::response::Response {
        router
            .oneshot(
                Request::post(format!("/api/rooms/{}/command", room_id.as_str()))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&frame).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_push_registration(
        router: RelayServerRouter,
        token: Option<&str>,
        registration: RelayPushRegistration,
    ) -> axum::response::Response {
        let mut request =
            Request::post("/api/push/registrations").header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        router
            .oneshot(
                request
                    .body(Body::from(serde_json::to_vec(&registration).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_push_dispatch(
        router: RelayServerRouter,
        token: Option<&str>,
        notification: RelayOpaqueNotification,
    ) -> axum::response::Response {
        let mut request =
            Request::post("/api/push/dispatch").header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        router
            .oneshot(
                request
                    .body(Body::from(serde_json::to_vec(&notification).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn encrypted_frame(
        room_id: RelayRoomId,
        kind: RelayFrameKind,
        ciphertext: &str,
    ) -> RelayEncryptedFrame {
        RelayEncryptedFrame {
            protocol_version: RelayProtocolVersion::foundation(),
            room_id,
            session_id: RelaySessionId::new(),
            frame_id: RelayFrameId::new(),
            sender_peer_id: RelayPeerId::new(),
            recipient_peer_id: RelayPeerId::new(),
            correlation_id: Some(CorrelationId::new()),
            kind,
            nonce: "nonce-redacted-in-debug".to_string(),
            ciphertext: ciphertext.to_string(),
            counter: 1,
            created_at_ms: unix_timestamp_ms(),
        }
    }

    fn encrypted_response_frame(
        room_id: RelayRoomId,
        correlation_id: CorrelationId,
        ciphertext: &str,
    ) -> RelayEncryptedFrame {
        RelayEncryptedFrame {
            correlation_id: Some(correlation_id),
            kind: RelayFrameKind::Response,
            ciphertext: ciphertext.to_string(),
            ..encrypted_frame(room_id, RelayFrameKind::Response, ciphertext)
        }
    }
}

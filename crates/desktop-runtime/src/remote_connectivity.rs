//! Durable remote publication and route lifecycle owned by `DesktopRuntime`.
//!
//! The controller deliberately keeps network publication adapters behind small
//! traits.  Startup reconciliation can therefore inspect the outside world
//! without making a mutation, while product actions remain explicit and
//! serialised.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;
use url::{Host, Url};
pub use vibex_core::WebBuildDescriptor;
use vibex_core::{
    ErrorCategory, RelayPeerId, RelayProtocolVersion, RelayRoomId, RemoteCancelPairingOfferRequest,
    RemoteCreatePairingOfferRequest, RemoteCreatePairingOfferResponse, RemoteDevicePermissionLevel,
    RemotePairingCandidate, RemotePairingOfferSummary, RemotePairingTransport,
    RemoteProtocolVersionRange, RequestId, VibexError, VibexResult, WEB_REQUIRED_ASSETS,
    WEB_STATIC_IDENTITY_ASSETS,
};
use vibex_remote::{
    RemoteGateway, RemoteGatewayConfig, RemoteGatewayDeploymentMode, RemoteGatewayPairingRoutes,
    RemoteGatewayTlsPolicy,
};

use crate::relay::{RelayClientRuntime, RelayClientSettingsUpdate};

pub const REMOTE_ACCESS_SETTINGS_FILE: &str = "remote-access.json";
pub const REMOTE_CONNECTIVITY_SCHEMA_VERSION: u16 = 1;
pub const DIRECT_LOOPBACK_BIND_ADDR: &str = "127.0.0.1:1428";
pub const DIRECT_LOOPBACK_TARGET: &str = "http://127.0.0.1:1428";
pub const MAX_DIRECT_CANDIDATES: usize = 8;
pub const TAILSCALE_DEFAULT_PORT: u16 = 443;
pub const TAILSCALE_FALLBACK_PORTS: RangeInclusive<u16> = 8443..=8450;
const STORE_TEMP_PREFIX: &str = ".remote-access-";
const MAX_PROCESS_OUTPUT_BYTES: usize = 256 * 1024;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const DIRECT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const DIRECT_PROBE_MAX_BYTES: usize = 128 * 1024;
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DESKTOP_BINARY_NAME: &str = "vibex-desktop";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteConnectivityMethod {
    TailscaleServe,
    Direct,
    SelfHostedRelay,
}

impl RemoteConnectivityMethod {
    pub const ALL: [Self; 3] = [Self::TailscaleServe, Self::Direct, Self::SelfHostedRelay];

    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::TailscaleServe => "tailscale_serve",
            Self::Direct => "direct",
            Self::SelfHostedRelay => "self_hosted_relay",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMethodState {
    #[default]
    Disabled,
    Checking,
    ConfirmationNeeded,
    Enabling,
    Online,
    Degraded,
    RepairRequired,
    Conflict,
    Stopping,
    Error,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRecoveryAction {
    #[default]
    None,
    Retry,
    ConfirmPort,
    RepairRoute,
    Configure,
    UpdateWebBuild,
    ManualCommand,
    RePair,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRouteOwnership {
    #[default]
    None,
    DesktopCreated,
    External,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMethodSnapshot {
    pub method: RemoteConnectivityMethod,
    pub desired_enabled: bool,
    pub state: RemoteMethodState,
    pub origin: Option<String>,
    pub https_port: Option<u16>,
    pub candidate_available: bool,
    pub last_validated_at_ms: Option<i64>,
    pub ownership: RemoteRouteOwnership,
    pub error_code: Option<String>,
    pub recovery_action: RemoteRecoveryAction,
}

impl fmt::Debug for RemoteMethodSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteMethodSnapshot")
            .field("method", &self.method)
            .field("desired_enabled", &self.desired_enabled)
            .field("state", &self.state)
            .field("origin", &self.origin)
            .field("https_port", &self.https_port)
            .field("candidate_available", &self.candidate_available)
            .field("last_validated_at_ms", &self.last_validated_at_ms)
            .field("ownership", &self.ownership)
            .field("error_code", &self.error_code)
            .field("recovery_action", &self.recovery_action)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnectivitySnapshot {
    pub schema_version: u16,
    pub desired_enabled: bool,
    pub running: bool,
    pub generation: u64,
    pub methods: Vec<RemoteMethodSnapshot>,
    pub active_route: Option<RemoteConnectivityMethod>,
    pub last_successful_pairing_entry: Option<RemoteConnectivityMethod>,
    pub direct_route_count: usize,
    pub relay_connected: bool,
    pub gateway_running: bool,
    pub gateway_bound_addr: Option<SocketAddr>,
}

impl fmt::Debug for RemoteConnectivitySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteConnectivitySnapshot")
            .field("schema_version", &self.schema_version)
            .field("desired_enabled", &self.desired_enabled)
            .field("running", &self.running)
            .field("generation", &self.generation)
            .field("methods", &self.methods)
            .field("active_route", &self.active_route)
            .field(
                "last_successful_pairing_entry",
                &self.last_successful_pairing_entry,
            )
            .field("direct_route_count", &self.direct_route_count)
            .field("relay_connected", &self.relay_connected)
            .field("gateway_running", &self.gateway_running)
            .field("gateway_bound_addr", &self.gateway_bound_addr)
            .finish()
    }
}

impl RemoteConnectivitySnapshot {
    pub fn method(&self, method: RemoteConnectivityMethod) -> Option<&RemoteMethodSnapshot> {
        self.methods.iter().find(|item| item.method == method)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteTransitionKind {
    Enabling,
    Disabling,
    Repairing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTransitionRecord {
    pub kind: RemoteTransitionKind,
    pub generation: u64,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleSettings {
    pub desired_enabled: bool,
    pub origin: Option<String>,
    pub https_port: Option<u16>,
    pub loopback_target: String,
    pub ownership: RemoteRouteOwnership,
    pub transition: Option<RemoteTransitionRecord>,
}

impl Default for TailscaleSettings {
    fn default() -> Self {
        Self {
            desired_enabled: false,
            origin: None,
            https_port: None,
            loopback_target: DIRECT_LOOPBACK_TARGET.to_string(),
            ownership: RemoteRouteOwnership::None,
            transition: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DirectSettings {
    pub desired_enabled: bool,
    pub origin: Option<String>,
    pub transition: Option<RemoteTransitionRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaySettings {
    pub desired_enabled: bool,
    pub origin: Option<String>,
    pub room_id: RelayRoomId,
    pub pc_peer_id: RelayPeerId,
    pub transition: Option<RemoteTransitionRecord>,
}

impl Default for RelaySettings {
    fn default() -> Self {
        Self {
            desired_enabled: false,
            origin: None,
            room_id: RelayRoomId::new(),
            pc_peer_id: RelayPeerId::new(),
            transition: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnectivitySettingsV1 {
    pub schema_version: u16,
    pub desired_enabled: bool,
    pub generation: u64,
    pub tailscale: TailscaleSettings,
    pub direct: DirectSettings,
    pub relay: RelaySettings,
    pub last_successful_pairing_entry: Option<RemoteConnectivityMethod>,
    pub updated_at_ms: i64,
}

impl Default for RemoteConnectivitySettingsV1 {
    fn default() -> Self {
        Self {
            schema_version: REMOTE_CONNECTIVITY_SCHEMA_VERSION,
            desired_enabled: false,
            generation: 0,
            tailscale: TailscaleSettings::default(),
            direct: DirectSettings::default(),
            relay: RelaySettings::default(),
            last_successful_pairing_entry: None,
            updated_at_ms: 0,
        }
    }
}

impl RemoteConnectivitySettingsV1 {
    pub fn validate(&self) -> VibexResult<()> {
        if self.schema_version != REMOTE_CONNECTIVITY_SCHEMA_VERSION {
            return Err(VibexError::storage(
                "remote_connectivity_settings_version_unsupported",
                "remote connectivity settings version is unsupported",
            ));
        }
        if self.tailscale.loopback_target != DIRECT_LOOPBACK_TARGET {
            return Err(VibexError::validation(
                "remote_connectivity_loopback_target_invalid",
                "remote connectivity routes must target the fixed loopback Gateway",
            ));
        }
        let any_desired = self.tailscale.desired_enabled
            || self.direct.desired_enabled
            || self.relay.desired_enabled;
        if self.desired_enabled != any_desired {
            return Err(VibexError::validation(
                "remote_connectivity_desired_state_invalid",
                "remote connectivity global and method intent is inconsistent",
            ));
        }
        if self.tailscale.ownership == RemoteRouteOwnership::DesktopCreated
            && (self.tailscale.origin.is_none() || self.tailscale.https_port.is_none())
        {
            return Err(VibexError::validation(
                "remote_connectivity_route_ownership_invalid",
                "an owned Tailscale route requires exact persisted metadata",
            ));
        }
        for transition in [
            self.tailscale.transition.as_ref(),
            self.direct.transition.as_ref(),
            self.relay.transition.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if transition.generation == 0 || transition.generation > self.generation {
                return Err(VibexError::validation(
                    "remote_connectivity_transition_invalid",
                    "remote connectivity transition generation is invalid",
                ));
            }
        }
        if let Some(origin) = &self.tailscale.origin {
            normalize_https_origin(origin)?;
        }
        if let Some(origin) = &self.direct.origin {
            normalize_https_origin(origin)?;
        }
        if let Some(origin) = &self.relay.origin {
            normalize_https_origin(origin)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConnectivityLoad {
    pub settings: RemoteConnectivitySettingsV1,
    pub recovered_corrupt_state: bool,
    pub corrupt_backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct RemoteConnectivityStore {
    path: PathBuf,
}

impl RemoteConnectivityStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn for_home(home: impl AsRef<Path>) -> Self {
        Self::new(home.as_ref().join(REMOTE_ACCESS_SETTINGS_FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> VibexResult<Option<RemoteConnectivitySettingsV1>> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(store_io_error(
                    "remote_connectivity_settings_stat_failed",
                    error,
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(VibexError::storage(
                "remote_connectivity_settings_invalid",
                "remote connectivity settings must be a regular file",
            ));
        }
        enforce_private_permissions(&self.path)?;
        let mut encoded = String::new();
        File::open(&self.path)
            .and_then(|mut file| file.read_to_string(&mut encoded))
            .map_err(|error| store_io_error("remote_connectivity_settings_read_failed", error))?;
        let settings: RemoteConnectivitySettingsV1 =
            serde_json::from_str(&encoded).map_err(|_| {
                VibexError::storage(
                    "remote_connectivity_settings_invalid",
                    "remote connectivity settings file is invalid",
                )
            })?;
        settings.validate().map_err(|error| {
            if error.code == "remote_connectivity_settings_version_unsupported" {
                error
            } else {
                VibexError::storage(
                    "remote_connectivity_settings_invalid",
                    "remote connectivity settings file is invalid",
                )
            }
        })?;
        Ok(Some(settings))
    }

    pub fn load_or_default(&self, now_ms: i64) -> VibexResult<RemoteConnectivityLoad> {
        match self.load() {
            Ok(Some(settings)) => Ok(RemoteConnectivityLoad {
                settings,
                recovered_corrupt_state: false,
                corrupt_backup_path: None,
            }),
            Ok(None) => Ok(RemoteConnectivityLoad {
                settings: RemoteConnectivitySettingsV1 {
                    updated_at_ms: now_ms,
                    ..RemoteConnectivitySettingsV1::default()
                },
                recovered_corrupt_state: false,
                corrupt_backup_path: None,
            }),
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "remote_connectivity_settings_invalid"
                        | "remote_connectivity_settings_version_unsupported"
                ) =>
            {
                let backup = self.quarantine_invalid(now_ms)?;
                Ok(RemoteConnectivityLoad {
                    settings: RemoteConnectivitySettingsV1 {
                        updated_at_ms: now_ms,
                        ..RemoteConnectivitySettingsV1::default()
                    },
                    recovered_corrupt_state: true,
                    corrupt_backup_path: backup,
                })
            }
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, settings: &RemoteConnectivitySettingsV1) -> VibexResult<()> {
        settings.validate()?;
        let bytes = serde_json::to_vec_pretty(settings).map_err(|_| {
            VibexError::storage(
                "remote_connectivity_settings_encode_failed",
                "remote connectivity settings could not be encoded",
            )
        })?;
        let parent = self.path.parent().ok_or_else(|| {
            VibexError::storage(
                "remote_connectivity_settings_parent_missing",
                "remote connectivity settings path has no parent",
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            store_io_error("remote_connectivity_settings_directory_failed", error)
        })?;
        enforce_private_directory_permissions(parent)?;
        let temp = parent.join(format!(
            "{STORE_TEMP_PREFIX}{}.tmp",
            RequestId::new().into_string()
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp).map_err(|error| {
                store_io_error("remote_connectivity_settings_create_failed", error)
            })?;
            file.write_all(&bytes)
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|error| {
                    store_io_error("remote_connectivity_settings_write_failed", error)
                })?;
            replace_file(&temp, &self.path).map_err(|error| {
                store_io_error("remote_connectivity_settings_commit_failed", error)
            })?;
            sync_parent(&self.path).map_err(|error| {
                store_io_error("remote_connectivity_settings_sync_failed", error)
            })?;
            enforce_private_permissions(&self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn quarantine_invalid(&self, now_ms: i64) -> VibexResult<Option<PathBuf>> {
        match fs::symlink_metadata(&self.path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(store_io_error(
                    "remote_connectivity_settings_quarantine_stat_failed",
                    error,
                ));
            }
        }
        let backup = self
            .path
            .with_file_name(format!("remote-access.invalid-{now_ms}.json"));
        fs::rename(&self.path, &backup).map_err(|error| {
            store_io_error("remote_connectivity_settings_quarantine_failed", error)
        })?;
        let metadata = fs::symlink_metadata(&backup).map_err(|error| {
            store_io_error("remote_connectivity_settings_quarantine_stat_failed", error)
        })?;
        if metadata.is_file() {
            enforce_private_permissions(&backup)?;
        } else if metadata.is_dir() {
            enforce_private_directory_permissions(&backup)?;
        }
        Ok(Some(backup))
    }
}

fn load_web_build_descriptor(
    root: impl AsRef<Path>,
    allow_debug: bool,
) -> VibexResult<WebBuildDescriptor> {
    let root = canonical_contained_root(root.as_ref())?;
    for relative in WEB_REQUIRED_ASSETS {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|_| {
            VibexError::capability(
                "web_assets_missing",
                "the packaged WebUI is missing a required asset",
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(VibexError::capability(
                "web_assets_invalid",
                "the packaged WebUI contains an invalid asset",
            ));
        }
        let canonical = fs::canonicalize(&path).map_err(|_| {
            VibexError::capability(
                "web_assets_invalid",
                "the packaged WebUI asset could not be resolved",
            )
        })?;
        if !canonical.starts_with(&root) {
            return Err(VibexError::capability(
                "web_assets_containment_failed",
                "the packaged WebUI asset escapes its source root",
            ));
        }
    }
    let bytes = fs::read(root.join("build.json")).map_err(|_| {
        VibexError::capability(
            "web_assets_missing",
            "the packaged WebUI build identity is missing",
        )
    })?;
    let descriptor: WebBuildDescriptor = serde_json::from_slice(&bytes).map_err(|_| {
        VibexError::capability(
            "web_assets_invalid",
            "the packaged WebUI build identity is invalid",
        )
    })?;
    if !descriptor.has_valid_identity(allow_debug) {
        return Err(VibexError::capability(
            "web_assets_incompatible",
            "the packaged WebUI build identity is incomplete",
        ));
    }
    if descriptor.package_version != env!("CARGO_PKG_VERSION") {
        return Err(VibexError::capability(
            "web_assets_incompatible",
            "the WebUI build is not compatible with this Desktop",
        ));
    }
    verify_web_asset_hashes(&root, &descriptor)?;
    Ok(descriptor)
}

#[derive(Debug, Clone)]
pub struct WebAssetResolver {
    packaged_roots: Vec<PathBuf>,
    debug_root: Option<PathBuf>,
    allow_debug: bool,
    require_source_identity: bool,
    expected_build_id: Option<String>,
    expected_git_commit: Option<String>,
}

impl WebAssetResolver {
    pub fn debug(root: impl Into<PathBuf>) -> Self {
        Self {
            packaged_roots: Vec::new(),
            debug_root: Some(root.into()),
            allow_debug: true,
            require_source_identity: false,
            expected_build_id: None,
            expected_git_commit: None,
        }
    }

    pub fn packaged(root: impl Into<PathBuf>) -> Self {
        Self {
            packaged_roots: vec![root.into()],
            debug_root: None,
            allow_debug: false,
            require_source_identity: true,
            expected_build_id: option_env!("VIBEX_WEB_BUILD_ID").map(str::to_string),
            expected_git_commit: option_env!("VIBEX_WEB_GIT_COMMIT").map(str::to_string),
        }
    }

    pub fn packaged_for_current_exe() -> Self {
        let executable = std::env::current_exe().unwrap_or_default();
        let binary_dir = executable.parent().unwrap_or_else(|| Path::new(""));
        let install_root = binary_dir.parent().unwrap_or(binary_dir);
        let roots = vec![
            install_root
                .join("lib")
                .join(DESKTOP_BINARY_NAME)
                .join("web"),
            install_root.join("Resources").join("web"),
            binary_dir.join("web"),
        ];
        let mut resolver = Self::packaged(roots[0].clone());
        resolver.packaged_roots = roots;
        resolver
    }

    pub fn with_debug_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.debug_root = Some(root.into());
        self
    }

    pub fn allow_debug(mut self, allow: bool) -> Self {
        self.allow_debug = allow;
        self
    }

    pub fn resolve(&self) -> VibexResult<(PathBuf, WebBuildDescriptor)> {
        if !self.packaged_roots.is_empty() {
            let Some(root) = self.packaged_roots.iter().find(|root| root.exists()) else {
                return Err(VibexError::capability(
                    "web_assets_missing",
                    "the packaged WebUI resource is unavailable",
                ));
            };
            let canonical = canonical_contained_root(root)?;
            let descriptor = load_web_build_descriptor(&canonical, self.allow_debug)?;
            if self.require_source_identity
                && (self.expected_build_id.as_deref() != Some(descriptor.build_id.as_str())
                    || self.expected_git_commit.as_deref() != Some(descriptor.git_commit.as_str()))
            {
                return Err(VibexError::capability(
                    "web_assets_incompatible",
                    "the packaged WebUI was not built from this Desktop source",
                ));
            }
            return Ok((canonical, descriptor));
        }
        if self.allow_debug
            && let Some(root) = &self.debug_root
        {
            let canonical = canonical_contained_root(root)?;
            let descriptor = load_web_build_descriptor(&canonical, true)?;
            return Ok((canonical, descriptor));
        }
        Err(VibexError::capability(
            "web_assets_missing",
            "no source-bound WebUI build is configured",
        ))
    }
}

fn verify_web_asset_hashes(root: &Path, descriptor: &WebBuildDescriptor) -> VibexResult<()> {
    let wasm =
        fs::read(root.join("pkg/vibex_web_bg.wasm")).map_err(|_| web_asset_integrity_error())?;
    let glue = fs::read(root.join("pkg/vibex_web.js")).map_err(|_| web_asset_integrity_error())?;
    let mut static_hash = Sha256::new();
    for relative in WEB_STATIC_IDENTITY_ASSETS {
        let mut bytes = fs::read(root.join(relative)).map_err(|_| web_asset_integrity_error())?;
        if *relative == "service-worker.js" {
            let source = String::from_utf8(bytes).map_err(|_| web_asset_integrity_error())?;
            if descriptor.profile == "release" && !source.contains(&descriptor.build_id) {
                return Err(web_asset_integrity_error());
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
        return Err(web_asset_integrity_error());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn web_asset_integrity_error() -> VibexError {
    VibexError::capability(
        "web_assets_incompatible",
        "the packaged WebUI asset identity does not match its build descriptor",
    )
}

pub fn normalize_https_origin(value: &str) -> VibexResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 2_048 {
        return Err(VibexError::validation(
            "remote_origin_invalid",
            "remote publication origin is invalid",
        ));
    }
    let parsed = Url::parse(trimmed).map_err(|_| {
        VibexError::validation(
            "remote_origin_invalid",
            "remote publication origin is invalid",
        )
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(VibexError::validation(
            "remote_origin_invalid",
            "remote publication origin must be an exact HTTPS origin without credentials or paths",
        ));
    }
    let host = match parsed.host() {
        Some(Host::Domain(host)) => host.to_ascii_lowercase(),
        Some(Host::Ipv4(host)) => host.to_string(),
        Some(Host::Ipv6(host)) => format!("[{host}]"),
        None => String::new(),
    };
    let mut output = format!("https://{host}");
    if let Some(port) = parsed.port()
        && port != 443
    {
        output.push(':');
        output.push_str(&port.to_string());
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectProbeInfo {
    pub server_id: String,
    pub server_identity_public_key: String,
    pub protocol_range: RemoteProtocolVersionRange,
    pub ws_path: String,
    pub pairing_claim_path: String,
    pub ws_ticket_path: String,
    #[serde(default)]
    pub deployment_mode: String,
    #[serde(default)]
    pub tls_policy: String,
    #[serde(default)]
    pub web_build: Option<WebBuildDescriptor>,
}

#[async_trait]
pub trait DirectPublicationProbe: Send + Sync {
    async fn probe(&self, origin: &str) -> VibexResult<DirectProbeInfo>;
}

#[derive(Debug, Clone)]
pub struct HttpDirectPublicationProbe {
    client: reqwest::Client,
}

impl Default for HttpDirectPublicationProbe {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .timeout(DIRECT_PROBE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

#[async_trait]
impl DirectPublicationProbe for HttpDirectPublicationProbe {
    async fn probe(&self, origin: &str) -> VibexResult<DirectProbeInfo> {
        let origin = normalize_https_origin(origin)?;
        let url = format!("{origin}/api/v2/info");
        let response = self.client.get(url).send().await.map_err(|_| {
            VibexError::process(
                "remote_direct_probe_failed",
                "the user-managed Direct origin could not be reached",
            )
        })?;
        if response.status() != StatusCode::OK {
            return Err(VibexError::process(
                "remote_direct_probe_failed",
                "the user-managed Direct origin returned an unexpected status",
            )
            .with_diagnostic("status", response.status().as_u16().to_string()));
        }
        let bytes = read_bounded_http_body(
            response,
            DIRECT_PROBE_MAX_BYTES,
            "remote_direct_probe_failed",
            "remote_direct_probe_response_too_large",
            "the user-managed Direct origin returned an unreadable response",
            "the user-managed Direct probe response is too large",
        )
        .await?;
        serde_json::from_slice(&bytes).map_err(|_| {
            VibexError::validation(
                "remote_direct_probe_invalid",
                "the user-managed Direct origin returned an invalid info response",
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPublicationInfo {
    pub protocol_version: RelayProtocolVersion,
    pub features: RelayPublicationFeatures,
    #[serde(default)]
    pub web_build: Option<WebBuildDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPublicationFeatures {
    pub pc_websocket: bool,
    pub device_websocket: bool,
    pub websocket_frames: bool,
    pub http_pair_bridge: bool,
    pub static_room_assets: bool,
}

impl RelayPublicationInfo {
    fn validate_browser_bootstrap(&self, expected: &WebBuildDescriptor) -> VibexResult<()> {
        if self.protocol_version != RelayProtocolVersion::foundation() {
            return Err(VibexError::capability(
                "relay_protocol_incompatible",
                "the self-hosted Relay protocol is incompatible",
            ));
        }
        if !self.features.pc_websocket
            || !self.features.device_websocket
            || !self.features.websocket_frames
            || !self.features.http_pair_bridge
            || !self.features.static_room_assets
        {
            return Err(VibexError::capability(
                "relay_browser_bootstrap_unavailable",
                "the self-hosted Relay does not expose the required browser pairing surface",
            ));
        }
        let Some(actual) = self.web_build.as_ref() else {
            return Err(VibexError::capability(
                "relay_web_build_missing",
                "the self-hosted Relay does not advertise a WebUI build",
            ));
        };
        if actual != expected || actual.profile != "release" {
            return Err(VibexError::capability(
                "relay_web_build_incompatible",
                "the self-hosted Relay WebUI does not match this Desktop",
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait RelayPublicationProbe: Send + Sync {
    async fn probe(&self, origin: &str) -> VibexResult<RelayPublicationInfo>;
}

#[derive(Debug, Clone)]
pub struct HttpRelayPublicationProbe {
    client: reqwest::Client,
}

impl Default for HttpRelayPublicationProbe {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .timeout(DIRECT_PROBE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

#[async_trait]
impl RelayPublicationProbe for HttpRelayPublicationProbe {
    async fn probe(&self, origin: &str) -> VibexResult<RelayPublicationInfo> {
        let origin = normalize_https_origin(origin)?;
        let response = self
            .client
            .get(format!("{origin}/api/info"))
            .send()
            .await
            .map_err(|_| {
                VibexError::process(
                    "relay_publication_probe_failed",
                    "the self-hosted Relay could not be reached",
                )
            })?;
        if response.status() != StatusCode::OK {
            return Err(VibexError::process(
                "relay_publication_probe_failed",
                "the self-hosted Relay returned an unexpected status",
            ));
        }
        let bytes = read_bounded_http_body(
            response,
            DIRECT_PROBE_MAX_BYTES,
            "relay_publication_probe_failed",
            "relay_publication_probe_response_too_large",
            "the self-hosted Relay returned an unreadable response",
            "the self-hosted Relay info response is too large",
        )
        .await?;
        serde_json::from_slice(&bytes).map_err(|_| {
            VibexError::validation(
                "relay_publication_probe_invalid",
                "the self-hosted Relay returned invalid endpoint information",
            )
        })
    }
}

async fn read_bounded_http_body(
    response: reqwest::Response,
    max_bytes: usize,
    read_error_code: &'static str,
    size_error_code: &'static str,
    read_error_message: &'static str,
    size_error_message: &'static str,
) -> VibexResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(VibexError::validation(size_error_code, size_error_message));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| VibexError::process(read_error_code, read_error_message))?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(VibexError::validation(size_error_code, size_error_message));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl fmt::Display for ProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "status={:?} stdout={}B stderr={}B",
            self.status,
            self.stdout.len(),
            self.stderr.len()
        )
    }
}

impl fmt::Debug for ProcessOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOutput")
            .field("status", &self.status)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

fn tailscale_command_error(
    output: &ProcessOutput,
    default_code: &'static str,
    default_message: &'static str,
) -> VibexError {
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("permission denied")
        || stderr.contains("access denied")
        || stderr.contains("not permitted")
    {
        VibexError::new(
            ErrorCategory::Permission,
            "tailscale_permission_denied",
            "Tailscale denied the requested operation",
        )
    } else if stderr.contains("tailscaled")
        && (stderr.contains("not running") || stderr.contains("connect"))
    {
        VibexError::process(
            "tailscale_daemon_offline",
            "the Tailscale daemon is unavailable",
        )
    } else if stderr.contains("unknown flag") || stderr.contains("unknown command") {
        VibexError::capability(
            "tailscale_cli_unsupported",
            "the installed Tailscale CLI does not support this operation",
        )
    } else {
        VibexError::process(default_code, default_message)
    }
}

#[async_trait]
pub trait ProcessRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[String]) -> VibexResult<ProcessOutput>;
}

#[derive(Debug, Clone, Default)]
pub struct TokioProcessRunner;

#[async_trait]
impl ProcessRunner for TokioProcessRunner {
    async fn run(&self, program: &str, args: &[String]) -> VibexResult<ProcessOutput> {
        if program != "tailscale" || args.iter().any(|arg| arg.contains('\0')) {
            return Err(VibexError::validation(
                "remote_process_command_invalid",
                "remote publication commands must use the fixed Tailscale executable",
            ));
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => VibexError::capability(
                "tailscale_binary_missing",
                "the Tailscale CLI is not installed",
            ),
            std::io::ErrorKind::PermissionDenied => VibexError::new(
                ErrorCategory::Permission,
                "tailscale_permission_denied",
                "the Tailscale CLI could not be executed",
            ),
            _ => VibexError::process("remote_process_failed", "Tailscale command failed to start"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            VibexError::process(
                "remote_process_failed",
                "Tailscale stdout pipe is unavailable",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            VibexError::process(
                "remote_process_failed",
                "Tailscale stderr pipe is unavailable",
            )
        })?;
        let completed = timeout(PROCESS_TIMEOUT, async {
            let (stdout, stderr, status) = tokio::join!(
                read_bounded_process_pipe(stdout),
                read_bounded_process_pipe(stderr),
                child.wait()
            );
            (stdout, stderr, status)
        })
        .await;
        let (stdout, stderr, status) = match completed {
            Ok(completed) => completed,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(VibexError::process(
                    "remote_process_timeout",
                    "Tailscale command timed out",
                ));
            }
        };
        let (stdout, stdout_truncated) = stdout.map_err(|_| {
            VibexError::process(
                "remote_process_failed",
                "Tailscale stdout could not be read",
            )
        })?;
        let (stderr, stderr_truncated) = stderr.map_err(|_| {
            VibexError::process(
                "remote_process_failed",
                "Tailscale stderr could not be read",
            )
        })?;
        if stdout_truncated || stderr_truncated {
            return Err(VibexError::process(
                "remote_process_output_too_large",
                "Tailscale command output exceeded the safety limit",
            ));
        }
        let status = status
            .map_err(|_| {
                VibexError::process("remote_process_failed", "Tailscale command did not finish")
            })?
            .code();
        Ok(ProcessOutput {
            status,
            stdout,
            stderr,
        })
    }
}

async fn read_bounded_process_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok((output, truncated))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleRoute {
    pub origin: String,
    pub https_port: u16,
    pub path: String,
    pub target: String,
    pub ownership: RemoteRouteOwnership,
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct TailscaleInspection {
    pub dns_name: Option<String>,
    pub routes: Vec<TailscaleRoute>,
}

impl fmt::Debug for TailscaleInspection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TailscaleInspection")
            .field("has_dns_name", &self.dns_name.is_some())
            .field("route_count", &self.routes.len())
            .finish()
    }
}

#[async_trait]
pub trait TailscalePublication: Send + Sync {
    async fn inspect(&self) -> VibexResult<TailscaleInspection>;
    async fn create(&self, port: u16, target: &str) -> VibexResult<TailscaleRoute>;
    async fn remove_owned(&self, route: &TailscaleRoute) -> VibexResult<()>;
}

#[derive(Debug, Clone)]
pub struct TailscaleCli<R = TokioProcessRunner> {
    runner: Arc<R>,
    fallback_ports: RangeInclusive<u16>,
}

impl<R> TailscaleCli<R> {
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner: Arc::new(runner),
            fallback_ports: TAILSCALE_FALLBACK_PORTS,
        }
    }

    pub fn with_fallback_ports(mut self, ports: RangeInclusive<u16>) -> Self {
        self.fallback_ports = ports;
        self
    }
}

impl Default for TailscaleCli<TokioProcessRunner> {
    fn default() -> Self {
        Self::with_runner(TokioProcessRunner)
    }
}

#[async_trait]
impl<R: ProcessRunner + 'static> TailscalePublication for TailscaleCli<R> {
    async fn inspect(&self) -> VibexResult<TailscaleInspection> {
        let status = self
            .runner
            .run("tailscale", &["status".to_string(), "--json".to_string()])
            .await?;
        let serve = self
            .runner
            .run(
                "tailscale",
                &[
                    "serve".to_string(),
                    "status".to_string(),
                    "--json".to_string(),
                ],
            )
            .await?;
        if status.status != Some(0) || serve.status != Some(0) {
            let failed = if status.status != Some(0) {
                &status
            } else {
                &serve
            };
            return Err(tailscale_command_error(
                failed,
                "tailscale_status_failed",
                "Tailscale status inspection failed",
            ));
        }
        parse_tailscale_inspection(&status.stdout, &serve.stdout)
    }

    async fn create(&self, port: u16, target: &str) -> VibexResult<TailscaleRoute> {
        if (port != TAILSCALE_DEFAULT_PORT && !self.fallback_ports.contains(&port))
            || target != DIRECT_LOOPBACK_TARGET
        {
            return Err(VibexError::validation(
                "tailscale_route_invalid",
                "Tailscale route target or port is invalid",
            ));
        }
        if self
            .inspect()
            .await?
            .routes
            .iter()
            .any(|route| route.https_port == port)
        {
            return Err(VibexError::conflict(
                "tailscale_route_conflict",
                "the requested Tailscale HTTPS port is already configured",
            ));
        }
        let args = vec![
            "serve".to_string(),
            "--bg".to_string(),
            format!("--https={port}"),
            target.to_string(),
        ];
        let output = self.runner.run("tailscale", &args).await?;
        if output.status != Some(0) {
            return Err(tailscale_command_error(
                &output,
                "tailscale_route_create_failed",
                "Tailscale Serve route could not be created",
            ));
        }
        let inspection = self.inspect().await?;
        let route = inspection
            .routes
            .into_iter()
            .find(|route| route.https_port == port && route.path == "/" && route.target == target);
        route
            .map(|mut route| {
                route.ownership = RemoteRouteOwnership::DesktopCreated;
                route
            })
            .ok_or_else(|| {
                VibexError::conflict(
                    "tailscale_route_verification_failed",
                    "Tailscale Serve route did not match the requested target after creation",
                )
            })
    }

    async fn remove_owned(&self, route: &TailscaleRoute) -> VibexResult<()> {
        if route.ownership != RemoteRouteOwnership::DesktopCreated
            || route.path != "/"
            || route.target != DIRECT_LOOPBACK_TARGET
        {
            return Err(VibexError::conflict(
                "tailscale_route_not_owned",
                "the Tailscale Serve route is not owned by Vibex",
            ));
        }
        let before = self.inspect().await?;
        let port_routes = before
            .routes
            .iter()
            .filter(|candidate| candidate.https_port == route.https_port)
            .collect::<Vec<_>>();
        if port_routes.is_empty() {
            return Ok(());
        }
        let exact_count = port_routes
            .iter()
            .filter(|candidate| {
                candidate.path == route.path
                    && candidate.target == route.target
                    && candidate.origin == route.origin
            })
            .count();
        if exact_count != 1 || port_routes.len() != 1 {
            return Err(VibexError::conflict(
                "tailscale_route_ownership_mismatch",
                "the owned Tailscale Serve route no longer matches exactly",
            ));
        }
        let args = vec![
            "serve".to_string(),
            format!("--https={}", route.https_port),
            "off".to_string(),
        ];
        let output = self.runner.run("tailscale", &args).await?;
        if output.status != Some(0) {
            return Err(tailscale_command_error(
                &output,
                "tailscale_route_remove_failed",
                "the owned Tailscale Serve route could not be removed",
            ));
        }
        let inspection = self.inspect().await?;
        if inspection
            .routes
            .iter()
            .any(|candidate| candidate.https_port == route.https_port)
        {
            return Err(VibexError::conflict(
                "tailscale_route_verification_failed",
                "the Tailscale Serve route remained after removal",
            ));
        }
        Ok(())
    }
}

pub fn parse_tailscale_inspection(status: &[u8], serve: &[u8]) -> VibexResult<TailscaleInspection> {
    let status: serde_json::Value = serde_json::from_slice(status).map_err(|_| {
        VibexError::process(
            "tailscale_status_invalid",
            "Tailscale status JSON is invalid",
        )
    })?;
    let serve: serde_json::Value = serde_json::from_slice(serve).map_err(|_| {
        VibexError::process(
            "tailscale_serve_status_invalid",
            "Tailscale Serve status JSON is invalid",
        )
    })?;
    let status_object = status.as_object().ok_or_else(|| {
        VibexError::process(
            "tailscale_status_invalid",
            "Tailscale status JSON has an invalid shape",
        )
    })?;
    let backend_state = object_string(
        status_object,
        &["BackendState", "backendState", "backend_state"],
    );
    if backend_state.is_some_and(|state| !state.eq_ignore_ascii_case("running")) {
        return Err(VibexError::process(
            "tailscale_daemon_offline",
            "the Tailscale daemon is not running",
        ));
    }
    let self_status =
        object_value(status_object, &["Self", "self"]).and_then(serde_json::Value::as_object);
    let dns_name = self_status
        .and_then(|self_status| object_string(self_status, &["DNSName", "dnsName", "dns_name"]))
        .map(|value| value.trim_end_matches('.').to_string());
    let dns_name = dns_name.filter(|value| !value.is_empty());
    let mut occupied = BTreeSet::new();
    let mut handlers = Vec::new();
    collect_tailscale_serve(&serve, &mut occupied, &mut handlers);
    handlers.sort();
    handlers.dedup();
    let mut routes = handlers
        .into_iter()
        .map(|(port, path, target)| TailscaleRoute {
            origin: tailscale_origin(dns_name.as_deref(), port),
            https_port: port,
            path,
            target,
            ownership: RemoteRouteOwnership::External,
        })
        .collect::<Vec<_>>();
    for port in occupied {
        if !routes.iter().any(|route| route.https_port == port) {
            routes.push(TailscaleRoute {
                origin: tailscale_origin(dns_name.as_deref(), port),
                https_port: port,
                path: String::new(),
                target: String::new(),
                ownership: RemoteRouteOwnership::External,
            });
        }
    }
    routes.sort_by(|left, right| {
        (left.https_port, &left.path, &left.target).cmp(&(
            right.https_port,
            &right.path,
            &right.target,
        ))
    });
    Ok(TailscaleInspection { dns_name, routes })
}

fn collect_tailscale_serve(
    value: &serde_json::Value,
    occupied: &mut BTreeSet<u16>,
    handlers: &mut Vec<(u16, String, String)>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                if key.eq_ignore_ascii_case("tcp") {
                    collect_port_keys(child, occupied);
                }
                if key.eq_ignore_ascii_case("web") {
                    collect_web_handlers(child, occupied, handlers);
                }
            }
            for child in object.values() {
                collect_tailscale_serve(child, occupied, handlers);
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                collect_tailscale_serve(child, occupied, handlers);
            }
        }
        _ => {}
    }
}

fn collect_port_keys(value: &serde_json::Value, occupied: &mut BTreeSet<u16>) {
    if let serde_json::Value::Object(object) = value {
        for (key, child) in object {
            if let Some(port) = port_from_authority(key) {
                occupied.insert(port);
            }
            if let Some(port) = numeric_port_field(child) {
                occupied.insert(port);
            }
        }
    }
}

fn collect_web_handlers(
    value: &serde_json::Value,
    occupied: &mut BTreeSet<u16>,
    handlers: &mut Vec<(u16, String, String)>,
) {
    let serde_json::Value::Object(object) = value else {
        return;
    };
    if let (Some(port), Some(target)) = (numeric_port_field(value), proxy_target(value)) {
        occupied.insert(port);
        handlers.push((port, "/".to_string(), target));
    }
    for (authority, site) in object {
        let Some(port) = port_from_authority(authority) else {
            continue;
        };
        occupied.insert(port);
        if let Some(handler_map) = site
            .as_object()
            .and_then(|site| site.get("Handlers").or_else(|| site.get("handlers")))
            .and_then(serde_json::Value::as_object)
        {
            for (path, handler) in handler_map {
                if let Some(target) = proxy_target(handler) {
                    handlers.push((port, normalize_handler_path(path), target));
                }
            }
        } else if let Some(target) = proxy_target(site) {
            handlers.push((port, "/".to_string(), target));
        }
    }
}

fn numeric_port_field(value: &serde_json::Value) -> Option<u16> {
    let object = value.as_object()?;
    object.iter().find_map(|(key, value)| {
        if matches!(
            key.as_str(),
            "HTTPS" | "https" | "httpsPort" | "https_port" | "Port" | "port"
        ) {
            value.as_u64().and_then(|port| u16::try_from(port).ok())
        } else {
            None
        }
    })
}

fn proxy_target(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "Proxy" | "proxy" | "Target" | "target" | "backend" | "Backend"
        ) && let Some(value) = value.as_str()
        {
            return Some(normalize_tailscale_target(value));
        }
    }
    object.values().find_map(proxy_target)
}

fn port_from_authority(value: &str) -> Option<u16> {
    let value = value.trim().trim_start_matches("https://");
    value.parse::<u16>().ok().or_else(|| {
        value
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
    })
}

fn normalize_handler_path(value: &str) -> String {
    if value.is_empty() {
        "/".to_string()
    } else if value.starts_with('/') {
        value.to_string()
    } else {
        format!("/{value}")
    }
}

fn tailscale_origin(dns_name: Option<&str>, port: u16) -> String {
    let Some(dns_name) = dns_name else {
        return String::new();
    };
    if port == TAILSCALE_DEFAULT_PORT {
        format!("https://{dns_name}")
    } else {
        format!("https://{dns_name}:{port}")
    }
}

fn normalize_tailscale_target(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed == DIRECT_LOOPBACK_BIND_ADDR || trimmed == DIRECT_LOOPBACK_TARGET {
        DIRECT_LOOPBACK_TARGET.to_string()
    } else {
        trimmed.to_string()
    }
}

fn object_value<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a serde_json::Value> {
    keys.iter().find_map(|key| object.get(*key))
}

fn object_string(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    object_value(object, keys)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[derive(Debug, Clone)]
struct MethodRuntime {
    state: RemoteMethodState,
    origin: Option<String>,
    https_port: Option<u16>,
    candidate: Option<RemotePairingCandidate>,
    ownership: RemoteRouteOwnership,
    last_validated_at_ms: Option<i64>,
    error_code: Option<String>,
    recovery_action: RemoteRecoveryAction,
}

impl MethodRuntime {
    fn disabled() -> Self {
        Self {
            state: RemoteMethodState::Disabled,
            origin: None,
            https_port: None,
            candidate: None,
            ownership: RemoteRouteOwnership::None,
            last_validated_at_ms: None,
            error_code: None,
            recovery_action: RemoteRecoveryAction::None,
        }
    }
}

#[derive(Clone)]
struct ControllerState {
    settings: RemoteConnectivitySettingsV1,
    methods: BTreeMap<RemoteConnectivityMethod, MethodRuntime>,
    initial_error: Option<String>,
}

struct ControllerInner {
    store: RemoteConnectivityStore,
    gateway: RemoteGateway,
    relay: RelayClientRuntime,
    tailscale: Arc<dyn TailscalePublication>,
    direct_probe: Arc<dyn DirectPublicationProbe>,
    relay_probe: Arc<dyn RelayPublicationProbe>,
    web_assets: Mutex<Option<WebAssetResolver>>,
    operation: Mutex<()>,
    state: Mutex<ControllerState>,
}

#[derive(Clone)]
pub struct RemoteConnectivityController {
    inner: Arc<ControllerInner>,
}

impl fmt::Debug for RemoteConnectivityController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteConnectivityController")
            .field("store", &self.inner.store.path())
            .field("gateway", &self.inner.gateway.status())
            .finish()
    }
}

impl RemoteConnectivityController {
    pub fn new(
        home: impl AsRef<Path>,
        gateway: RemoteGateway,
        relay: RelayClientRuntime,
    ) -> VibexResult<Self> {
        Self::with_adapters(
            home,
            gateway,
            relay,
            Arc::new(TailscaleCli::default()),
            Arc::new(HttpDirectPublicationProbe::default()),
        )
    }

    pub fn with_adapters(
        home: impl AsRef<Path>,
        gateway: RemoteGateway,
        relay: RelayClientRuntime,
        tailscale: Arc<dyn TailscalePublication>,
        direct_probe: Arc<dyn DirectPublicationProbe>,
    ) -> VibexResult<Self> {
        Self::with_publication_adapters(
            home,
            gateway,
            relay,
            tailscale,
            direct_probe,
            Arc::new(HttpRelayPublicationProbe::default()),
        )
    }

    pub fn with_publication_adapters(
        home: impl AsRef<Path>,
        gateway: RemoteGateway,
        relay: RelayClientRuntime,
        tailscale: Arc<dyn TailscalePublication>,
        direct_probe: Arc<dyn DirectPublicationProbe>,
        relay_probe: Arc<dyn RelayPublicationProbe>,
    ) -> VibexResult<Self> {
        let store = RemoteConnectivityStore::for_home(home);
        let loaded = store.load_or_default(vibex_core::unix_timestamp_ms())?;
        let mut methods = BTreeMap::new();
        for method in RemoteConnectivityMethod::ALL {
            methods.insert(method, MethodRuntime::disabled());
        }
        let initial_error = loaded
            .recovered_corrupt_state
            .then_some("remote_connectivity_settings_invalid".to_string());
        Ok(Self {
            inner: Arc::new(ControllerInner {
                store,
                gateway,
                relay,
                tailscale,
                direct_probe,
                relay_probe,
                web_assets: Mutex::new(None),
                operation: Mutex::new(()),
                state: Mutex::new(ControllerState {
                    settings: loaded.settings,
                    methods,
                    initial_error,
                }),
            }),
        })
    }

    pub fn store(&self) -> RemoteConnectivityStore {
        self.inner.store.clone()
    }

    pub fn gateway(&self) -> RemoteGateway {
        self.inner.gateway.clone()
    }

    pub fn relay(&self) -> RelayClientRuntime {
        self.inner.relay.clone()
    }

    pub async fn set_web_asset_resolver(&self, resolver: Option<WebAssetResolver>) {
        let _operation = self.inner.operation.lock().await;
        *self.inner.web_assets.lock().await = resolver;
    }

    pub async fn snapshot(&self) -> RemoteConnectivitySnapshot {
        let state = self.inner.state.lock().await.clone();
        let gateway_status = self.inner.gateway.status();
        let relay_status = self.inner.relay.get_status().await;
        let methods = RemoteConnectivityMethod::ALL
            .into_iter()
            .map(|method| {
                let runtime = state.methods.get(&method).expect("all methods initialized");
                let desired_enabled = desired_for(&state.settings, method);
                RemoteMethodSnapshot {
                    method,
                    desired_enabled,
                    state: if state.initial_error.is_some() && desired_enabled {
                        RemoteMethodState::RepairRequired
                    } else {
                        runtime.state
                    },
                    origin: runtime.origin.clone(),
                    https_port: runtime.https_port,
                    candidate_available: runtime.candidate.is_some(),
                    last_validated_at_ms: runtime.last_validated_at_ms,
                    ownership: runtime.ownership,
                    error_code: runtime
                        .error_code
                        .clone()
                        .or_else(|| state.initial_error.clone()),
                    recovery_action: runtime.recovery_action,
                }
            })
            .collect::<Vec<_>>();
        let direct_route_count = methods
            .iter()
            .filter(|item| {
                item.candidate_available
                    && matches!(
                        item.method,
                        RemoteConnectivityMethod::Direct | RemoteConnectivityMethod::TailscaleServe
                    )
            })
            .count();
        let active_route = methods
            .iter()
            .find(|item| item.candidate_available)
            .map(|item| item.method);
        RemoteConnectivitySnapshot {
            schema_version: REMOTE_CONNECTIVITY_SCHEMA_VERSION,
            desired_enabled: state.settings.desired_enabled,
            running: gateway_status.running
                || relay_status.state == crate::RelayClientConnectionState::Connected,
            generation: state.settings.generation,
            methods,
            active_route,
            last_successful_pairing_entry: state.settings.last_successful_pairing_entry,
            direct_route_count,
            relay_connected: relay_status.state == crate::RelayClientConnectionState::Connected,
            gateway_running: gateway_status.running,
            gateway_bound_addr: gateway_status.bound_addr,
        }
    }

    pub fn create_pairing_offer(
        &self,
        permission_level: RemoteDevicePermissionLevel,
        ttl_ms: u32,
    ) -> VibexResult<RemoteCreatePairingOfferResponse> {
        self.inner
            .gateway
            .create_pairing_offer(RemoteCreatePairingOfferRequest {
                permission_level,
                ttl_ms: Some(ttl_ms),
                direct_candidates: Vec::new(),
                relay_candidate: None,
            })
    }

    pub fn pairing_offer_status(
        &self,
        offer_id: &RequestId,
    ) -> VibexResult<RemotePairingOfferSummary> {
        self.inner.gateway.pairing_offer_status(offer_id)
    }

    pub fn cancel_pairing_offer(
        &self,
        offer_id: RequestId,
    ) -> VibexResult<RemotePairingOfferSummary> {
        self.inner
            .gateway
            .cancel_pairing_offer(RemoteCancelPairingOfferRequest { offer_id })
    }

    pub async fn record_claimed_pairing_entry(
        &self,
        offer_id: &RequestId,
        method: RemoteConnectivityMethod,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let summary = self.inner.gateway.pairing_offer_status(offer_id)?;
        if summary.claimed_device_id.is_none() {
            return Err(VibexError::conflict(
                "remote_pairing_offer_not_claimed",
                "pairing entry preference requires a claimed offer",
            ));
        }
        let was_offered = match method {
            RemoteConnectivityMethod::TailscaleServe => summary
                .direct_candidates
                .iter()
                .any(|candidate| candidate.transport == RemotePairingTransport::Tailnet),
            RemoteConnectivityMethod::Direct => summary
                .direct_candidates
                .iter()
                .any(|candidate| candidate.transport == RemotePairingTransport::Direct),
            RemoteConnectivityMethod::SelfHostedRelay => summary.relay_candidate.is_some(),
        };
        if !was_offered {
            return Err(VibexError::validation(
                "remote_pairing_entry_not_offered",
                "selected pairing entry was not part of the claimed offer",
            ));
        }

        let _operation = self.inner.operation.lock().await;
        let mut state = self.inner.state.lock().await;
        if state.settings.last_successful_pairing_entry != Some(method) {
            state.settings.last_successful_pairing_entry = Some(method);
            state.settings.generation = state.settings.generation.saturating_add(1);
            state.settings.updated_at_ms = vibex_core::unix_timestamp_ms();
            persist_state(&self.inner.store, &state.settings)?;
        }
        drop(state);
        Ok(self.snapshot().await)
    }

    pub async fn reconcile_on_startup(&self) -> VibexResult<RemoteConnectivitySnapshot> {
        let _operation = self.inner.operation.lock().await;
        let mut state = self.inner.state.lock().await;
        if !state.settings.desired_enabled {
            for runtime in state.methods.values_mut() {
                *runtime = MethodRuntime::disabled();
            }
            self.inner
                .gateway
                .set_pairing_routes(RemoteGatewayPairingRoutes::default())?;
            drop(state);
            return Ok(self.snapshot().await);
        }

        for method in RemoteConnectivityMethod::ALL {
            if !desired_for(&state.settings, method) {
                continue;
            }
            let runtime = state
                .methods
                .get_mut(&method)
                .expect("all methods initialized");
            runtime.state = RemoteMethodState::Checking;
            runtime.error_code = None;
            runtime.recovery_action = RemoteRecoveryAction::Retry;
        }

        let tailscale_settings = state.settings.tailscale.clone();
        let direct_settings = state.settings.direct.clone();
        let relay_settings = state.settings.relay.clone();
        drop(state);

        if tailscale_settings.desired_enabled {
            self.reconcile_tailscale(tailscale_settings).await;
        }
        if direct_settings.desired_enabled {
            self.reconcile_direct(direct_settings).await;
        }
        if relay_settings.desired_enabled {
            self.reconcile_relay(relay_settings).await;
        }
        self.apply_routes().await?;
        Ok(self.snapshot().await)
    }

    pub async fn enable_direct(
        &self,
        origin: impl AsRef<str>,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let origin = origin.as_ref().to_string();
        let _operation = self.inner.operation.lock().await;
        self.enable_direct_inner(&origin).await
    }

    async fn enable_direct_inner(&self, origin: &str) -> VibexResult<RemoteConnectivitySnapshot> {
        let origin = normalize_https_origin(origin)?;
        {
            let mut state = self.inner.state.lock().await;
            state.settings.desired_enabled = true;
            state.settings.direct.desired_enabled = true;
            state.settings.direct.origin = Some(origin.clone());
            bump_transition(
                &mut state.settings,
                RemoteConnectivityMethod::Direct,
                RemoteTransitionKind::Enabling,
            );
            persist_state(&self.inner.store, &state.settings)?;
            state
                .methods
                .get_mut(&RemoteConnectivityMethod::Direct)
                .unwrap()
                .state = RemoteMethodState::Enabling;
        }
        let expected_build = match self.prepare_gateway_for_origin(&origin).await {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::Direct,
                    &error,
                    RemoteRecoveryAction::UpdateWebBuild,
                )
                .await;
                return Err(error);
            }
        };
        let result = self
            .inner
            .direct_probe
            .probe(&origin)
            .await
            .and_then(|info| self.validate_direct_info(&info, &expected_build));
        match result {
            Ok(()) => {
                let persisted = {
                    let mut state = self.inner.state.lock().await;
                    let runtime = state
                        .methods
                        .get_mut(&RemoteConnectivityMethod::Direct)
                        .unwrap();
                    runtime.state = RemoteMethodState::Online;
                    runtime.origin = Some(origin.clone());
                    runtime.candidate = Some(RemotePairingCandidate {
                        transport: RemotePairingTransport::Direct,
                        url: origin,
                        relay_room_id: None,
                        relay_pc_peer_id: None,
                        relay_pc_public_key: None,
                    });
                    runtime.last_validated_at_ms = Some(vibex_core::unix_timestamp_ms());
                    runtime.error_code = None;
                    runtime.recovery_action = RemoteRecoveryAction::None;
                    state.settings.direct.transition = None;
                    state.initial_error = None;
                    persist_reconciled_state(
                        &self.inner.store,
                        &mut state,
                        RemoteConnectivityMethod::Direct,
                    )
                };
                if let Err(error) = persisted {
                    self.apply_routes().await?;
                    return Err(error);
                }
            }
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::Direct,
                    &error,
                    RemoteRecoveryAction::RepairRoute,
                )
                .await;
                self.apply_routes().await?;
                return Err(error);
            }
        }
        self.apply_routes().await?;
        Ok(self.snapshot().await)
    }

    pub async fn enable_relay(
        &self,
        origin: impl AsRef<str>,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let origin = origin.as_ref().to_string();
        let _operation = self.inner.operation.lock().await;
        self.enable_relay_inner(&origin).await
    }

    async fn enable_relay_inner(&self, origin: &str) -> VibexResult<RemoteConnectivitySnapshot> {
        let origin = normalize_https_origin(origin)?;
        {
            let mut state = self.inner.state.lock().await;
            state.settings.desired_enabled = true;
            state.settings.relay.desired_enabled = true;
            state.settings.relay.origin = Some(origin.clone());
            bump_transition(
                &mut state.settings,
                RemoteConnectivityMethod::SelfHostedRelay,
                RemoteTransitionKind::Enabling,
            );
            persist_state(&self.inner.store, &state.settings)?;
            state
                .methods
                .get_mut(&RemoteConnectivityMethod::SelfHostedRelay)
                .unwrap()
                .state = RemoteMethodState::Enabling;
        }
        let expected_build = match self.resolve_web_assets().await {
            Ok((_, descriptor)) => descriptor,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::SelfHostedRelay,
                    &error,
                    RemoteRecoveryAction::UpdateWebBuild,
                )
                .await;
                return Err(error);
            }
        };
        let publication = self.inner.relay_probe.probe(&origin).await;
        if let Err(error) =
            publication.and_then(|info| info.validate_browser_bootstrap(&expected_build))
        {
            self.record_method_error(
                RemoteConnectivityMethod::SelfHostedRelay,
                &error,
                RemoteRecoveryAction::RepairRoute,
            )
            .await;
            return Err(error);
        }
        let relay_settings = self.inner.state.lock().await.settings.relay.clone();
        if let Err(error) = self
            .inner
            .relay
            .update_settings(RelayClientSettingsUpdate {
                enabled: Some(true),
                relay_url: Some(Some(origin.clone())),
                room_id: Some(relay_settings.room_id.clone()),
                pc_peer_id: Some(relay_settings.pc_peer_id.clone()),
                ..RelayClientSettingsUpdate::default()
            })
            .await
        {
            self.record_method_error(
                RemoteConnectivityMethod::SelfHostedRelay,
                &error,
                RemoteRecoveryAction::RepairRoute,
            )
            .await;
            return Err(error);
        }
        if let Err(error) = self.inner.relay.start().await {
            self.record_method_error(
                RemoteConnectivityMethod::SelfHostedRelay,
                &error,
                RemoteRecoveryAction::Retry,
            )
            .await;
            return Err(error);
        }
        let status = match self.wait_for_relay_connected().await {
            Ok(status) => status,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::SelfHostedRelay,
                    &error,
                    RemoteRecoveryAction::Retry,
                )
                .await;
                return Err(error);
            }
        };
        let persisted = {
            let mut state = self.inner.state.lock().await;
            let settings = state.settings.relay.clone();
            let runtime = state
                .methods
                .get_mut(&RemoteConnectivityMethod::SelfHostedRelay)
                .unwrap();
            runtime.state = RemoteMethodState::Online;
            runtime.origin = Some(origin.clone());
            runtime.candidate = Some(RemotePairingCandidate {
                transport: RemotePairingTransport::SelfHostedRelay,
                url: origin,
                relay_room_id: Some(settings.room_id),
                relay_pc_peer_id: Some(settings.pc_peer_id),
                relay_pc_public_key: Some(status.pc_public_key),
            });
            runtime.error_code = None;
            runtime.recovery_action = RemoteRecoveryAction::None;
            runtime.last_validated_at_ms = Some(vibex_core::unix_timestamp_ms());
            state.settings.relay.transition = None;
            state.initial_error = None;
            persist_reconciled_state(
                &self.inner.store,
                &mut state,
                RemoteConnectivityMethod::SelfHostedRelay,
            )
        };
        if let Err(error) = persisted {
            self.apply_routes().await?;
            return Err(error);
        }
        self.apply_routes().await?;
        Ok(self.snapshot().await)
    }

    pub async fn enable_tailscale(
        &self,
        confirmed_port: Option<u16>,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let _operation = self.inner.operation.lock().await;
        self.enable_tailscale_inner(confirmed_port).await
    }

    async fn enable_tailscale_inner(
        &self,
        confirmed_port: Option<u16>,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        {
            let mut state = self.inner.state.lock().await;
            state.settings.desired_enabled = true;
            state.settings.tailscale.desired_enabled = true;
            bump_transition(
                &mut state.settings,
                RemoteConnectivityMethod::TailscaleServe,
                RemoteTransitionKind::Enabling,
            );
            persist_state(&self.inner.store, &state.settings)?;
            state
                .methods
                .get_mut(&RemoteConnectivityMethod::TailscaleServe)
                .unwrap()
                .state = RemoteMethodState::Checking;
        }
        let inspection = match self.inner.tailscale.inspect().await {
            Ok(inspection) => inspection,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::TailscaleServe,
                    &error,
                    RemoteRecoveryAction::Retry,
                )
                .await;
                return Err(error);
            }
        };
        let (recorded_origin, recorded_port, recorded_ownership) = {
            let state = self.inner.state.lock().await;
            (
                state.settings.tailscale.origin.clone(),
                state.settings.tailscale.https_port,
                state.settings.tailscale.ownership,
            )
        };
        let exact = recorded_port
            .and_then(|port| {
                inspection
                    .routes
                    .iter()
                    .find(|route| {
                        route.https_port == port
                            && route.path == "/"
                            && route.target == DIRECT_LOOPBACK_TARGET
                    })
                    .cloned()
            })
            .or_else(|| {
                inspection
                    .routes
                    .iter()
                    .find(|route| route.path == "/" && route.target == DIRECT_LOOPBACK_TARGET)
                    .cloned()
            });
        let (origin, port, existing_route) = if let Some(route) = exact {
            let origin = match normalize_https_origin(&route.origin) {
                Ok(origin) => origin,
                Err(error) => {
                    self.record_method_error(
                        RemoteConnectivityMethod::TailscaleServe,
                        &error,
                        RemoteRecoveryAction::RepairRoute,
                    )
                    .await;
                    return Err(error);
                }
            };
            (origin, route.https_port, Some(route))
        } else {
            let port = if !inspection
                .routes
                .iter()
                .any(|route| route.https_port == TAILSCALE_DEFAULT_PORT)
            {
                TAILSCALE_DEFAULT_PORT
            } else {
                let Some(proposal) = first_free_port(&inspection, &TAILSCALE_FALLBACK_PORTS) else {
                    let error = VibexError::conflict(
                        "tailscale_https_port_exhausted",
                        "no supported Tailscale HTTPS port is available",
                    );
                    self.record_method_error(
                        RemoteConnectivityMethod::TailscaleServe,
                        &error,
                        RemoteRecoveryAction::ManualCommand,
                    )
                    .await;
                    return Err(error);
                };
                if confirmed_port != Some(proposal)
                    || recorded_port != Some(proposal)
                    || !TAILSCALE_FALLBACK_PORTS.contains(&proposal)
                {
                    let proposed_origin = normalize_https_origin(&tailscale_origin(
                        inspection.dns_name.as_deref(),
                        proposal,
                    ))
                    .ok();
                    return self.confirmation_needed(proposal, proposed_origin).await;
                }
                proposal
            };
            let origin = match normalize_https_origin(&tailscale_origin(
                inspection.dns_name.as_deref(),
                port,
            )) {
                Ok(origin) => origin,
                Err(_) => {
                    let error = VibexError::process(
                        "tailscale_dns_unavailable",
                        "Tailscale did not report a usable MagicDNS name",
                    );
                    self.record_method_error(
                        RemoteConnectivityMethod::TailscaleServe,
                        &error,
                        RemoteRecoveryAction::Retry,
                    )
                    .await;
                    return Err(error);
                }
            };
            (origin, port, None)
        };
        let expected_build = match self.prepare_gateway_for_origin(&origin).await {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::TailscaleServe,
                    &error,
                    RemoteRecoveryAction::UpdateWebBuild,
                )
                .await;
                self.apply_routes().await?;
                return Err(error);
            }
        };
        let (route, ownership, created) = if let Some(route) = existing_route {
            let ownership = if recorded_ownership == RemoteRouteOwnership::DesktopCreated
                && recorded_port == Some(route.https_port)
                && recorded_origin.as_deref() == Some(route.origin.as_str())
            {
                RemoteRouteOwnership::DesktopCreated
            } else {
                RemoteRouteOwnership::External
            };
            (route, ownership, false)
        } else {
            let route = match self
                .inner
                .tailscale
                .create(port, DIRECT_LOOPBACK_TARGET)
                .await
            {
                Ok(route) => route,
                Err(error) => {
                    self.record_method_error(
                        RemoteConnectivityMethod::TailscaleServe,
                        &error,
                        RemoteRecoveryAction::Retry,
                    )
                    .await;
                    self.apply_routes().await?;
                    return Err(error);
                }
            };
            (route, RemoteRouteOwnership::DesktopCreated, true)
        };
        let publication = self
            .inner
            .direct_probe
            .probe(&origin)
            .await
            .and_then(|info| self.validate_direct_info(&info, &expected_build));
        if let Err(error) = publication {
            if created {
                let mut owned = route.clone();
                owned.ownership = RemoteRouteOwnership::DesktopCreated;
                let _ = self.inner.tailscale.remove_owned(&owned).await;
            }
            self.record_method_error(
                RemoteConnectivityMethod::TailscaleServe,
                &error,
                RemoteRecoveryAction::RepairRoute,
            )
            .await;
            self.apply_routes().await?;
            return Err(error);
        }
        let persisted = {
            let mut state = self.inner.state.lock().await;
            state.settings.tailscale.origin = Some(origin.clone());
            state.settings.tailscale.https_port = Some(route.https_port);
            state.settings.tailscale.ownership = ownership;
            state.settings.tailscale.transition = None;
            let ownership = state.settings.tailscale.ownership;
            let runtime = state
                .methods
                .get_mut(&RemoteConnectivityMethod::TailscaleServe)
                .unwrap();
            runtime.state = RemoteMethodState::Online;
            runtime.origin = Some(origin.clone());
            runtime.https_port = Some(route.https_port);
            runtime.ownership = ownership;
            runtime.candidate = Some(RemotePairingCandidate {
                transport: RemotePairingTransport::Tailnet,
                url: origin,
                relay_room_id: None,
                relay_pc_peer_id: None,
                relay_pc_public_key: None,
            });
            runtime.last_validated_at_ms = Some(vibex_core::unix_timestamp_ms());
            runtime.error_code = None;
            runtime.recovery_action = RemoteRecoveryAction::None;
            state.initial_error = None;
            persist_reconciled_state(
                &self.inner.store,
                &mut state,
                RemoteConnectivityMethod::TailscaleServe,
            )
        };
        if let Err(error) = persisted {
            self.apply_routes().await?;
            return Err(error);
        }
        self.apply_routes().await?;
        Ok(self.snapshot().await)
    }

    pub async fn disable_method(
        &self,
        method: RemoteConnectivityMethod,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let _operation = self.inner.operation.lock().await;
        self.disable_method_inner(method).await
    }

    pub async fn repair_method(
        &self,
        method: RemoteConnectivityMethod,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let _operation = self.inner.operation.lock().await;
        let settings = self.inner.state.lock().await.settings.clone();
        if !desired_for(&settings, method) {
            return self.disable_method_inner(method).await;
        }
        match method {
            RemoteConnectivityMethod::TailscaleServe => self.enable_tailscale_inner(None).await,
            RemoteConnectivityMethod::Direct => {
                let origin = settings.direct.origin.ok_or_else(|| {
                    VibexError::validation(
                        "remote_direct_origin_missing",
                        "Direct origin is missing",
                    )
                })?;
                self.enable_direct_inner(&origin).await
            }
            RemoteConnectivityMethod::SelfHostedRelay => {
                let origin = settings.relay.origin.ok_or_else(|| {
                    VibexError::validation("relay_origin_missing", "Relay origin is missing")
                })?;
                self.enable_relay_inner(&origin).await
            }
        }
    }

    async fn disable_method_inner(
        &self,
        method: RemoteConnectivityMethod,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let (owned_route, settings) = {
            let mut state = self.inner.state.lock().await;
            let owned_route = if method == RemoteConnectivityMethod::TailscaleServe
                && state.settings.tailscale.ownership == RemoteRouteOwnership::DesktopCreated
            {
                state
                    .settings
                    .tailscale
                    .origin
                    .clone()
                    .zip(state.settings.tailscale.https_port)
            } else {
                None
            };
            set_desired(&mut state.settings, method, false);
            bump_transition(&mut state.settings, method, RemoteTransitionKind::Disabling);
            if let Some(runtime) = state.methods.get_mut(&method) {
                runtime.state = RemoteMethodState::Stopping;
                runtime.candidate = None;
            }
            persist_state(&self.inner.store, &state.settings)?;
            (owned_route, state.settings.clone())
        };
        self.apply_routes().await?;
        if method == RemoteConnectivityMethod::SelfHostedRelay {
            if let Err(error) = self
                .inner
                .relay
                .update_settings(RelayClientSettingsUpdate {
                    enabled: Some(false),
                    ..RelayClientSettingsUpdate::default()
                })
                .await
            {
                self.record_method_error(method, &error, RemoteRecoveryAction::Retry)
                    .await;
                return Err(error);
            }
            if let Err(error) = self.inner.relay.stop().await {
                self.record_method_error(method, &error, RemoteRecoveryAction::Retry)
                    .await;
                return Err(error);
            }
        }
        if method == RemoteConnectivityMethod::TailscaleServe
            && settings.tailscale.ownership == RemoteRouteOwnership::DesktopCreated
            && let Some((origin, port)) = owned_route.clone()
        {
            let route = TailscaleRoute {
                origin,
                https_port: port,
                path: "/".to_string(),
                target: DIRECT_LOOPBACK_TARGET.to_string(),
                ownership: RemoteRouteOwnership::DesktopCreated,
            };
            if let Err(error) = self.inner.tailscale.remove_owned(&route).await {
                self.record_method_error(method, &error, RemoteRecoveryAction::RepairRoute)
                    .await;
                return Err(error);
            }
        }
        {
            let mut state = self.inner.state.lock().await;
            let runtime = state.methods.get_mut(&method).unwrap();
            *runtime = MethodRuntime::disabled();
            match method {
                RemoteConnectivityMethod::TailscaleServe => {
                    state.settings.tailscale.transition = None;
                    if owned_route.is_some() {
                        state.settings.tailscale.origin = None;
                        state.settings.tailscale.https_port = None;
                        state.settings.tailscale.ownership = RemoteRouteOwnership::None;
                    }
                }
                RemoteConnectivityMethod::Direct => state.settings.direct.transition = None,
                RemoteConnectivityMethod::SelfHostedRelay => state.settings.relay.transition = None,
            }
            persist_state(&self.inner.store, &state.settings)?;
        }
        self.apply_routes().await?;
        Ok(self.snapshot().await)
    }

    pub async fn disable_all(&self) -> VibexResult<RemoteConnectivitySnapshot> {
        let _operation = self.inner.operation.lock().await;
        for method in RemoteConnectivityMethod::ALL {
            if desired_for(&self.inner.state.lock().await.settings, method) {
                self.disable_method_inner(method).await?;
            }
        }
        let mut state = self.inner.state.lock().await;
        state.settings.desired_enabled = false;
        persist_state(&self.inner.store, &state.settings)?;
        drop(state);
        Ok(self.snapshot().await)
    }

    async fn resolve_web_assets(&self) -> VibexResult<(PathBuf, WebBuildDescriptor)> {
        let resolver = self.inner.web_assets.lock().await.clone().ok_or_else(|| {
            VibexError::capability(
                "web_assets_missing",
                "no source-bound WebUI build is configured",
            )
        })?;
        resolver.resolve()
    }

    async fn prepare_gateway_for_origin(&self, origin: &str) -> VibexResult<WebBuildDescriptor> {
        let origin = normalize_https_origin(origin)?;
        let (root, descriptor) = self.resolve_web_assets().await?;
        let routes = self.aggregate_routes().await;
        let mut origins = routes
            .direct_candidates
            .iter()
            .map(|candidate| candidate.url.clone())
            .collect::<BTreeSet<_>>();
        origins.insert(origin);
        self.configure_gateway(
            routes,
            origins.into_iter().collect(),
            Some((root, descriptor.clone())),
            true,
        )
        .await?;
        Ok(descriptor)
    }

    async fn aggregate_routes(&self) -> RemoteGatewayPairingRoutes {
        let state = self.inner.state.lock().await;
        let mut direct_candidates = Vec::new();
        let mut relay_candidate = None;
        for method in RemoteConnectivityMethod::ALL {
            let Some(candidate) = state
                .methods
                .get(&method)
                .and_then(|runtime| runtime.candidate.clone())
            else {
                continue;
            };
            match candidate.transport {
                RemotePairingTransport::Direct | RemotePairingTransport::Tailnet => {
                    direct_candidates.push(candidate)
                }
                RemotePairingTransport::SelfHostedRelay => relay_candidate = Some(candidate),
                RemotePairingTransport::Unknown => {}
            }
        }
        direct_candidates.truncate(MAX_DIRECT_CANDIDATES);
        RemoteGatewayPairingRoutes {
            direct_candidates,
            relay_candidate,
        }
    }

    async fn configure_gateway(
        &self,
        routes: RemoteGatewayPairingRoutes,
        allowed_origins: Vec<String>,
        web_assets: Option<(PathBuf, WebBuildDescriptor)>,
        listener_enabled: bool,
    ) -> VibexResult<()> {
        let previous = self.inner.gateway.current_config();
        let mut config = previous.clone();
        config.pairing_routes = routes;
        config.service.bind_addr = DIRECT_LOOPBACK_BIND_ADDR.to_string();
        config.service.enabled = listener_enabled;
        config.deployment_mode = if listener_enabled {
            RemoteGatewayDeploymentMode::Lan
        } else {
            RemoteGatewayDeploymentMode::Loopback
        };
        config.tls_policy = if listener_enabled {
            RemoteGatewayTlsPolicy::TrustedHttpsProxy
        } else {
            RemoteGatewayTlsPolicy::LoopbackHttp
        };
        config.allowed_origins = if listener_enabled {
            allowed_origins
        } else {
            RemoteGatewayConfig::default().allowed_origins
        };
        config.allowed_hosts = if listener_enabled {
            config
                .allowed_origins
                .iter()
                .filter_map(|origin| {
                    Url::parse(origin)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_string))
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            RemoteGatewayConfig::default().allowed_hosts
        };
        if listener_enabled && config.allowed_hosts.is_empty() {
            return Err(VibexError::validation(
                "remote_gateway_origin_allowlist_invalid",
                "the remote Gateway requires at least one validated origin",
            ));
        }
        config.static_dir = web_assets.as_ref().map(|(root, _)| root.clone());
        config.web_build = web_assets
            .as_ref()
            .map(|(_, descriptor)| descriptor.clone());
        let was_running = self.inner.gateway.status().running;
        if was_running {
            let mut route_only_update = previous.clone();
            route_only_update.pairing_routes = config.pairing_routes.clone();
            if route_only_update == config {
                return self.inner.gateway.set_pairing_routes(config.pairing_routes);
            }
        }
        if was_running {
            self.inner.gateway.stop().await?;
        }
        if let Err(error) = self.inner.gateway.apply_config_while_stopped(config).await {
            if was_running {
                let _ = self.inner.gateway.start().await;
            }
            return Err(error);
        }
        if listener_enabled && let Err(error) = self.inner.gateway.start().await {
            let rollback = async {
                self.inner.gateway.stop().await?;
                self.inner
                    .gateway
                    .apply_config_while_stopped(previous)
                    .await?;
                if was_running {
                    self.inner.gateway.start().await?;
                }
                Ok::<(), VibexError>(())
            }
            .await;
            if let Err(rollback_error) = rollback {
                tracing::warn!(
                    target: "vibex_desktop",
                    error_code = %rollback_error.code,
                    "remote Gateway configuration rollback failed"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    async fn confirmation_needed(
        &self,
        port: u16,
        proposed_origin: Option<String>,
    ) -> VibexResult<RemoteConnectivitySnapshot> {
        let mut state = self.inner.state.lock().await;
        let runtime = state
            .methods
            .get_mut(&RemoteConnectivityMethod::TailscaleServe)
            .unwrap();
        runtime.state = RemoteMethodState::ConfirmationNeeded;
        runtime.origin = proposed_origin;
        runtime.https_port = Some(port);
        runtime.error_code = Some("tailscale_https_port_confirmation_required".to_string());
        runtime.recovery_action = RemoteRecoveryAction::ConfirmPort;
        state.settings.tailscale.https_port = Some(port);
        state.settings.updated_at_ms = vibex_core::unix_timestamp_ms();
        persist_state(&self.inner.store, &state.settings)?;
        drop(state);
        Ok(self.snapshot().await)
    }

    fn validate_direct_info(
        &self,
        info: &DirectProbeInfo,
        expected_build: &WebBuildDescriptor,
    ) -> VibexResult<()> {
        let identity = self.inner.gateway.identity()?;
        if info.server_id != identity.server_id()
            || info.server_identity_public_key != identity.public_key_base64()
        {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                "remote_direct_identity_mismatch",
                "the Direct origin belongs to a different Desktop identity",
            ));
        }
        if info
            .protocol_range
            .negotiate(RemoteProtocolVersionRange::v2())
            .is_none()
        {
            return Err(VibexError::new(
                ErrorCategory::Capability,
                "remote_direct_protocol_incompatible",
                "the Direct origin does not support Remote Protocol v2",
            ));
        }
        if info.ws_path != "/ws/v2"
            || info.pairing_claim_path != "/api/v2/pairing/claim"
            || info.ws_ticket_path != "/api/v2/ws-ticket"
        {
            return Err(VibexError::validation(
                "remote_direct_paths_invalid",
                "the Direct origin does not expose the expected Remote Protocol paths",
            ));
        }
        if info.deployment_mode != "lan" || info.tls_policy != "trusted_https_proxy" {
            return Err(VibexError::validation(
                "remote_direct_security_policy_invalid",
                "the Direct origin does not expose the trusted HTTPS proxy policy",
            ));
        }
        match info.web_build.as_ref() {
            Some(actual) if actual == expected_build => {}
            Some(_) => {
                return Err(VibexError::capability(
                    "remote_direct_web_build_incompatible",
                    "the Direct origin WebUI does not match this Desktop",
                ));
            }
            None => {
                return Err(VibexError::capability(
                    "remote_direct_web_build_missing",
                    "the Direct origin does not advertise a WebUI build",
                ));
            }
        }
        Ok(())
    }

    async fn reconcile_tailscale(&self, settings: TailscaleSettings) {
        let inspection = match self.inner.tailscale.inspect().await {
            Ok(inspection) => inspection,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::TailscaleServe,
                    &error,
                    RemoteRecoveryAction::Retry,
                )
                .await;
                return;
            }
        };
        let exact = settings.https_port.and_then(|port| {
            inspection.routes.into_iter().find(|route| {
                route.https_port == port
                    && route.path == "/"
                    && route.target == DIRECT_LOOPBACK_TARGET
                    && settings.origin.as_deref() == Some(route.origin.as_str())
            })
        });
        let Some(route) = exact else {
            self.record_method_error(
                RemoteConnectivityMethod::TailscaleServe,
                &VibexError::conflict(
                    "tailscale_route_missing",
                    "the configured Tailscale Serve route is missing or mismatched",
                ),
                RemoteRecoveryAction::RepairRoute,
            )
            .await;
            return;
        };
        let origin = match normalize_https_origin(&route.origin) {
            Ok(origin) => origin,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::TailscaleServe,
                    &error,
                    RemoteRecoveryAction::RepairRoute,
                )
                .await;
                return;
            }
        };
        let expected_build = match self.prepare_gateway_for_origin(&origin).await {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::TailscaleServe,
                    &error,
                    RemoteRecoveryAction::UpdateWebBuild,
                )
                .await;
                return;
            }
        };
        let validation = self
            .inner
            .direct_probe
            .probe(&origin)
            .await
            .and_then(|info| self.validate_direct_info(&info, &expected_build));
        if let Err(error) = validation {
            self.record_method_error(
                RemoteConnectivityMethod::TailscaleServe,
                &error,
                RemoteRecoveryAction::Retry,
            )
            .await;
            return;
        }
        let mut state = self.inner.state.lock().await;
        let runtime = state
            .methods
            .get_mut(&RemoteConnectivityMethod::TailscaleServe)
            .unwrap();
        runtime.state = RemoteMethodState::Online;
        runtime.origin = Some(origin.clone());
        runtime.https_port = Some(route.https_port);
        runtime.ownership = settings.ownership;
        runtime.candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::Tailnet,
            url: origin,
            relay_room_id: None,
            relay_pc_peer_id: None,
            relay_pc_public_key: None,
        });
        runtime.last_validated_at_ms = Some(vibex_core::unix_timestamp_ms());
        runtime.error_code = None;
        runtime.recovery_action = RemoteRecoveryAction::None;
        state.settings.tailscale.transition = None;
        let _ = persist_reconciled_state(
            &self.inner.store,
            &mut state,
            RemoteConnectivityMethod::TailscaleServe,
        );
    }

    async fn reconcile_direct(&self, settings: DirectSettings) {
        let Some(origin) = settings.origin else {
            self.record_method_error(
                RemoteConnectivityMethod::Direct,
                &VibexError::validation("remote_direct_origin_missing", "Direct origin is missing"),
                RemoteRecoveryAction::Configure,
            )
            .await;
            return;
        };
        let expected_build = match self.prepare_gateway_for_origin(&origin).await {
            Ok(descriptor) => descriptor,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::Direct,
                    &error,
                    RemoteRecoveryAction::UpdateWebBuild,
                )
                .await;
                return;
            }
        };
        let result = self
            .inner
            .direct_probe
            .probe(&origin)
            .await
            .and_then(|info| self.validate_direct_info(&info, &expected_build));
        if let Err(error) = result {
            self.record_method_error(
                RemoteConnectivityMethod::Direct,
                &error,
                RemoteRecoveryAction::Retry,
            )
            .await;
            return;
        }
        let mut state = self.inner.state.lock().await;
        let runtime = state
            .methods
            .get_mut(&RemoteConnectivityMethod::Direct)
            .unwrap();
        runtime.state = RemoteMethodState::Online;
        runtime.origin = Some(origin.clone());
        runtime.candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::Direct,
            url: origin,
            relay_room_id: None,
            relay_pc_peer_id: None,
            relay_pc_public_key: None,
        });
        runtime.last_validated_at_ms = Some(vibex_core::unix_timestamp_ms());
        runtime.error_code = None;
        runtime.recovery_action = RemoteRecoveryAction::None;
        state.settings.direct.transition = None;
        let _ = persist_reconciled_state(
            &self.inner.store,
            &mut state,
            RemoteConnectivityMethod::Direct,
        );
    }

    async fn reconcile_relay(&self, settings: RelaySettings) {
        let Some(origin) = settings.origin.clone() else {
            self.record_method_error(
                RemoteConnectivityMethod::SelfHostedRelay,
                &VibexError::validation("relay_origin_missing", "Relay origin is missing"),
                RemoteRecoveryAction::Configure,
            )
            .await;
            return;
        };
        let expected_build = match self.resolve_web_assets().await {
            Ok((_, descriptor)) => descriptor,
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::SelfHostedRelay,
                    &error,
                    RemoteRecoveryAction::UpdateWebBuild,
                )
                .await;
                return;
            }
        };
        let publication = self.inner.relay_probe.probe(&origin).await;
        if let Err(error) =
            publication.and_then(|info| info.validate_browser_bootstrap(&expected_build))
        {
            self.record_method_error(
                RemoteConnectivityMethod::SelfHostedRelay,
                &error,
                RemoteRecoveryAction::RepairRoute,
            )
            .await;
            return;
        }
        let update = self
            .inner
            .relay
            .update_settings(RelayClientSettingsUpdate {
                enabled: Some(true),
                relay_url: Some(Some(origin.clone())),
                room_id: Some(settings.room_id.clone()),
                pc_peer_id: Some(settings.pc_peer_id.clone()),
                ..RelayClientSettingsUpdate::default()
            })
            .await;
        if let Err(error) = update {
            self.record_method_error(
                RemoteConnectivityMethod::SelfHostedRelay,
                &error,
                RemoteRecoveryAction::RepairRoute,
            )
            .await;
            return;
        }
        match self.inner.relay.start().await {
            Ok(_) => match self.wait_for_relay_connected().await {
                Ok(status) => {
                    let mut state = self.inner.state.lock().await;
                    let runtime = state
                        .methods
                        .get_mut(&RemoteConnectivityMethod::SelfHostedRelay)
                        .unwrap();
                    runtime.state = RemoteMethodState::Online;
                    runtime.origin = Some(origin.clone());
                    runtime.candidate = Some(RemotePairingCandidate {
                        transport: RemotePairingTransport::SelfHostedRelay,
                        url: origin,
                        relay_room_id: Some(settings.room_id),
                        relay_pc_peer_id: Some(settings.pc_peer_id),
                        relay_pc_public_key: Some(status.pc_public_key),
                    });
                    runtime.last_validated_at_ms = Some(vibex_core::unix_timestamp_ms());
                    runtime.error_code = None;
                    runtime.recovery_action = RemoteRecoveryAction::None;
                    state.settings.relay.transition = None;
                    let _ = persist_reconciled_state(
                        &self.inner.store,
                        &mut state,
                        RemoteConnectivityMethod::SelfHostedRelay,
                    );
                }
                Err(error) => {
                    self.record_method_error(
                        RemoteConnectivityMethod::SelfHostedRelay,
                        &error,
                        RemoteRecoveryAction::Retry,
                    )
                    .await
                }
            },
            Err(error) => {
                self.record_method_error(
                    RemoteConnectivityMethod::SelfHostedRelay,
                    &error,
                    RemoteRecoveryAction::Retry,
                )
                .await
            }
        }
    }

    async fn wait_for_relay_connected(&self) -> VibexResult<crate::RelayClientStatus> {
        timeout(RELAY_CONNECT_TIMEOUT, async {
            loop {
                let status = self.inner.relay.get_status().await;
                match status.state {
                    crate::RelayClientConnectionState::Connected => return Ok(status),
                    crate::RelayClientConnectionState::Error => {
                        return Err(VibexError::process(
                            "relay_connection_failed",
                            "the self-hosted Relay connection failed",
                        ));
                    }
                    _ => tokio::time::sleep(Duration::from_millis(25)).await,
                }
            }
        })
        .await
        .map_err(|_| {
            VibexError::process(
                "relay_connection_timeout",
                "the self-hosted Relay connection timed out",
            )
        })?
    }

    async fn record_method_error(
        &self,
        method: RemoteConnectivityMethod,
        error: &VibexError,
        recovery: RemoteRecoveryAction,
    ) {
        let mut state = self.inner.state.lock().await;
        if let Some(runtime) = state.methods.get_mut(&method) {
            runtime.state = if error.category == vibex_core::ErrorCategory::Conflict {
                RemoteMethodState::Conflict
            } else if matches!(
                recovery,
                RemoteRecoveryAction::RepairRoute | RemoteRecoveryAction::UpdateWebBuild
            ) {
                RemoteMethodState::RepairRequired
            } else {
                RemoteMethodState::Error
            };
            runtime.error_code = Some(error.code.clone());
            runtime.recovery_action = recovery;
            runtime.candidate = None;
        }
        match method {
            RemoteConnectivityMethod::TailscaleServe => state.settings.tailscale.transition = None,
            RemoteConnectivityMethod::Direct => state.settings.direct.transition = None,
            RemoteConnectivityMethod::SelfHostedRelay => state.settings.relay.transition = None,
        }
        if let Err(persist_error) = persist_state(&self.inner.store, &state.settings) {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %persist_error.code,
                "remote connectivity error state could not be persisted"
            );
        }
    }

    async fn apply_routes(&self) -> VibexResult<()> {
        let mut routes = self.aggregate_routes().await;
        if routes.direct_candidates.is_empty() {
            return self
                .configure_gateway(routes, Vec::new(), None, false)
                .await;
        }
        let web_assets = match self.resolve_web_assets().await {
            Ok(web_assets) => web_assets,
            Err(error) => {
                for method in [
                    RemoteConnectivityMethod::Direct,
                    RemoteConnectivityMethod::TailscaleServe,
                ] {
                    if self
                        .inner
                        .state
                        .lock()
                        .await
                        .methods
                        .get(&method)
                        .is_some_and(|runtime| runtime.candidate.is_some())
                    {
                        self.record_method_error(
                            method,
                            &error,
                            RemoteRecoveryAction::UpdateWebBuild,
                        )
                        .await;
                    }
                }
                routes = self.aggregate_routes().await;
                return self
                    .configure_gateway(routes, Vec::new(), None, false)
                    .await;
            }
        };
        let origins = routes
            .direct_candidates
            .iter()
            .map(|candidate| candidate.url.clone())
            .collect();
        self.configure_gateway(routes, origins, Some(web_assets), true)
            .await
    }
}

fn desired_for(settings: &RemoteConnectivitySettingsV1, method: RemoteConnectivityMethod) -> bool {
    match method {
        RemoteConnectivityMethod::TailscaleServe => settings.tailscale.desired_enabled,
        RemoteConnectivityMethod::Direct => settings.direct.desired_enabled,
        RemoteConnectivityMethod::SelfHostedRelay => settings.relay.desired_enabled,
    }
}

fn set_desired(
    settings: &mut RemoteConnectivitySettingsV1,
    method: RemoteConnectivityMethod,
    desired: bool,
) {
    match method {
        RemoteConnectivityMethod::TailscaleServe => settings.tailscale.desired_enabled = desired,
        RemoteConnectivityMethod::Direct => settings.direct.desired_enabled = desired,
        RemoteConnectivityMethod::SelfHostedRelay => settings.relay.desired_enabled = desired,
    }
    settings.desired_enabled = settings.tailscale.desired_enabled
        || settings.direct.desired_enabled
        || settings.relay.desired_enabled;
}

fn bump_transition(
    settings: &mut RemoteConnectivitySettingsV1,
    method: RemoteConnectivityMethod,
    kind: RemoteTransitionKind,
) {
    settings.generation = settings.generation.saturating_add(1);
    settings.updated_at_ms = vibex_core::unix_timestamp_ms();
    let transition = Some(RemoteTransitionRecord {
        kind,
        generation: settings.generation,
        started_at_ms: settings.updated_at_ms,
    });
    match method {
        RemoteConnectivityMethod::TailscaleServe => settings.tailscale.transition = transition,
        RemoteConnectivityMethod::Direct => settings.direct.transition = transition,
        RemoteConnectivityMethod::SelfHostedRelay => settings.relay.transition = transition,
    }
}

fn persist_state(
    store: &RemoteConnectivityStore,
    settings: &RemoteConnectivitySettingsV1,
) -> VibexResult<()> {
    store.save(settings)
}

fn persist_reconciled_state(
    store: &RemoteConnectivityStore,
    state: &mut ControllerState,
    method: RemoteConnectivityMethod,
) -> VibexResult<()> {
    if let Err(error) = persist_state(store, &state.settings) {
        if let Some(runtime) = state.methods.get_mut(&method) {
            runtime.state = RemoteMethodState::Error;
            runtime.candidate = None;
            runtime.error_code = Some(error.code.clone());
            runtime.recovery_action = RemoteRecoveryAction::Retry;
        }
        tracing::warn!(
            target: "vibex_desktop",
            method = method.wire_name(),
            error_code = %error.code,
            "reconciled remote connectivity state could not be persisted"
        );
        return Err(error);
    }
    Ok(())
}

fn first_free_port(inspection: &TailscaleInspection, ports: &RangeInclusive<u16>) -> Option<u16> {
    ports.clone().find(|port| {
        !inspection
            .routes
            .iter()
            .any(|route| route.https_port == *port)
    })
}

fn canonical_contained_root(root: &Path) -> VibexResult<PathBuf> {
    let canonical = fs::canonicalize(root).map_err(|_| {
        VibexError::capability("web_assets_missing", "the WebUI asset root is unavailable")
    })?;
    if !canonical.is_dir() {
        return Err(VibexError::capability(
            "web_assets_invalid",
            "the WebUI asset root is not a directory",
        ));
    }
    Ok(canonical)
}

fn store_io_error(code: &'static str, error: std::io::Error) -> VibexError {
    VibexError::storage(code, "remote connectivity settings storage failed")
        .with_diagnostic("errorKind", format!("{:?}", error.kind()))
}

#[cfg(unix)]
fn enforce_private_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::metadata(path)
        .map_err(|error| store_io_error("remote_connectivity_settings_stat_failed", error))?;
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            store_io_error("remote_connectivity_settings_permissions_failed", error)
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_private_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

#[cfg(unix)]
fn enforce_private_directory_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        store_io_error(
            "remote_connectivity_settings_directory_permissions_failed",
            error,
        )
    })
}

#[cfg(not(unix))]
fn enforce_private_directory_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    fs::rename(source, destination)
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;
    use vibex_core::RemoteClaimPairingOfferRequest;

    static CONTROLLER_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    #[derive(Default)]
    struct FakeRunner {
        calls: StdMutex<Vec<(String, Vec<String>)>>,
        responses: StdMutex<Vec<ProcessOutput>>,
    }

    #[async_trait]
    impl ProcessRunner for FakeRunner {
        async fn run(&self, program: &str, args: &[String]) -> VibexResult<ProcessOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_string(), args.to_vec()));
            self.responses.lock().unwrap().pop().ok_or_else(|| {
                VibexError::process("fake_runner_empty", "fake runner has no response")
            })
        }
    }

    fn successful_process(stdout: &[u8]) -> ProcessOutput {
        ProcessOutput {
            status: Some(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[derive(Default)]
    struct FakeTailscale {
        inspection: Mutex<TailscaleInspection>,
        inspect_calls: AtomicUsize,
        create_calls: StdMutex<Vec<u16>>,
        remove_calls: StdMutex<Vec<u16>>,
    }

    #[async_trait]
    impl TailscalePublication for FakeTailscale {
        async fn inspect(&self) -> VibexResult<TailscaleInspection> {
            self.inspect_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.inspection.lock().await.clone())
        }

        async fn create(&self, port: u16, target: &str) -> VibexResult<TailscaleRoute> {
            self.create_calls.lock().unwrap().push(port);
            let mut inspection = self.inspection.lock().await;
            if inspection
                .routes
                .iter()
                .any(|route| route.https_port == port)
            {
                return Err(VibexError::conflict(
                    "fake_tailscale_conflict",
                    "fake Tailscale port is occupied",
                ));
            }
            let route = TailscaleRoute {
                origin: tailscale_origin(inspection.dns_name.as_deref(), port),
                https_port: port,
                path: "/".to_string(),
                target: target.to_string(),
                ownership: RemoteRouteOwnership::DesktopCreated,
            };
            inspection.routes.push(route.clone());
            Ok(route)
        }

        async fn remove_owned(&self, route: &TailscaleRoute) -> VibexResult<()> {
            self.remove_calls.lock().unwrap().push(route.https_port);
            let mut inspection = self.inspection.lock().await;
            let Some(index) = inspection.routes.iter().position(|candidate| {
                candidate.https_port == route.https_port
                    && candidate.path == route.path
                    && candidate.target == route.target
            }) else {
                return Err(VibexError::conflict(
                    "fake_tailscale_route_missing",
                    "fake Tailscale route is missing",
                ));
            };
            inspection.routes.remove(index);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDirectProbe {
        info: StdMutex<Option<DirectProbeInfo>>,
        gateway: StdMutex<Option<RemoteGateway>>,
        on_probe: StdMutex<Option<Box<dyn FnOnce() + Send>>>,
        calls: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    #[async_trait]
    impl DirectPublicationProbe for FakeDirectProbe {
        async fn probe(&self, _origin: &str) -> VibexResult<DirectProbeInfo> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self
                .gateway
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|gateway| !gateway.status().running)
            {
                return Err(VibexError::process(
                    "fake_gateway_not_running",
                    "Gateway was not running before publication validation",
                ));
            }
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            if let Some(action) = self.on_probe.lock().unwrap().take() {
                action();
            }
            self.info.lock().unwrap().clone().ok_or_else(|| {
                VibexError::process(
                    "fake_direct_probe_unconfigured",
                    "fake probe is unconfigured",
                )
            })
        }
    }

    #[derive(Default)]
    struct FakeRelayProbe {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl RelayPublicationProbe for FakeRelayProbe {
        async fn probe(&self, _origin: &str) -> VibexResult<RelayPublicationInfo> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(VibexError::process(
                "fake_relay_probe_unconfigured",
                "fake Relay probe is unconfigured",
            ))
        }
    }

    async fn test_controller(
        directory: &Path,
        tailscale: Arc<FakeTailscale>,
        direct_probe: Arc<FakeDirectProbe>,
        relay_probe: Arc<FakeRelayProbe>,
    ) -> (
        RemoteConnectivityController,
        RemoteGateway,
        WebBuildDescriptor,
    ) {
        let dispatcher = vibex_remote::RemoteDispatcher::new(
            vibex_remote::RemoteServiceConfig::loopback_disabled(),
        );
        let gateway = RemoteGateway::new(
            RemoteGatewayConfig::default(),
            dispatcher.clone(),
            directory.join("vibex.db"),
            directory.join("relay/desktop-identity.json"),
        );
        let relay = RelayClientRuntime::with_remote_gateway(dispatcher, gateway.clone()).unwrap();
        let controller = RemoteConnectivityController::with_publication_adapters(
            directory,
            gateway.clone(),
            relay,
            tailscale,
            direct_probe.clone(),
            relay_probe,
        )
        .unwrap();
        let web_root = directory.join("web");
        let descriptor = write_test_web_build(&web_root);
        controller
            .set_web_asset_resolver(Some(WebAssetResolver::debug(web_root)))
            .await;
        *direct_probe.gateway.lock().unwrap() = Some(gateway.clone());
        (controller, gateway, descriptor)
    }

    fn write_test_web_build(root: &Path) -> WebBuildDescriptor {
        write_test_web_build_with_profile(root, "debug")
    }

    fn write_test_web_build_with_profile(root: &Path, profile: &str) -> WebBuildDescriptor {
        let build_id = "bbbbbbbbbbbbbbbbbbbbbbbb";
        fs::create_dir_all(root.join("pkg")).unwrap();
        for relative in WEB_STATIC_IDENTITY_ASSETS {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let contents = if *relative == "service-worker.js" && profile == "release" {
                format!("const BUILD_ID = \"{build_id}\";\n")
            } else {
                format!("test asset {relative}\n")
            };
            fs::write(path, contents).unwrap();
        }
        fs::write(root.join("pkg/vibex_web.js"), "test glue\n").unwrap();
        fs::write(root.join("pkg/vibex_web_bg.wasm"), b"\0asmtest").unwrap();
        let mut static_hash = Sha256::new();
        for relative in WEB_STATIC_IDENTITY_ASSETS {
            let mut bytes = fs::read(root.join(relative)).unwrap();
            if *relative == "service-worker.js" && profile == "release" {
                bytes = String::from_utf8(bytes)
                    .unwrap()
                    .replace(build_id, "__VIBEX_BUILD_ID__")
                    .into_bytes();
            }
            static_hash.update(relative.as_bytes());
            static_hash.update(b"\0");
            static_hash.update(bytes);
            static_hash.update(b"\0");
        }
        let descriptor = WebBuildDescriptor {
            schema_version: "vibex-web-build.v1".to_string(),
            build_id: build_id.to_string(),
            package_version: env!("CARGO_PKG_VERSION").to_string(),
            profile: profile.to_string(),
            git_commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            wasm_sha256: sha256_hex(b"\0asmtest"),
            glue_sha256: sha256_hex(b"test glue\n"),
            static_sha256: format!("{:x}", static_hash.finalize()),
        };
        fs::write(
            root.join("build.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();
        load_web_build_descriptor(root, profile == "debug").unwrap()
    }

    fn direct_info(gateway: &RemoteGateway, web_build: &WebBuildDescriptor) -> DirectProbeInfo {
        let identity = gateway.identity().unwrap();
        DirectProbeInfo {
            server_id: identity.server_id().to_string(),
            server_identity_public_key: identity.public_key_base64(),
            protocol_range: RemoteProtocolVersionRange::v2(),
            ws_path: "/ws/v2".to_string(),
            pairing_claim_path: "/api/v2/pairing/claim".to_string(),
            ws_ticket_path: "/api/v2/ws-ticket".to_string(),
            deployment_mode: "lan".to_string(),
            tls_policy: "trusted_https_proxy".to_string(),
            web_build: Some(web_build.clone()),
        }
    }

    #[test]
    fn settings_store_is_atomic_private_and_round_trips_without_secrets() {
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        let settings = RemoteConnectivitySettingsV1 {
            desired_enabled: true,
            direct: DirectSettings {
                desired_enabled: true,
                origin: Some("https://desktop.example.test:8443".to_string()),
                ..DirectSettings::default()
            },
            updated_at_ms: 42,
            ..RemoteConnectivitySettingsV1::default()
        };
        store.save(&settings).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded, settings);
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("private_key"));
        assert!(!text.contains("grant"));
        assert!(!text.contains("challenge"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(store.path()).unwrap().mode() & 0o077, 0);
        }
    }

    #[test]
    fn corrupt_settings_are_quarantined_and_never_enabled() {
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        fs::create_dir_all(directory.path()).unwrap();
        fs::write(
            store.path(),
            b"{\"desiredEnabled\":true,\"grant\":\"secret\"}",
        )
        .unwrap();
        let loaded = store.load_or_default(10).unwrap();
        assert!(loaded.recovered_corrupt_state);
        assert!(!loaded.settings.desired_enabled);
        assert!(loaded.corrupt_backup_path.unwrap().exists());
    }

    #[test]
    fn unknown_settings_version_is_quarantined_and_fails_closed() {
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        let settings = RemoteConnectivitySettingsV1 {
            schema_version: REMOTE_CONNECTIVITY_SCHEMA_VERSION + 1,
            ..RemoteConnectivitySettingsV1::default()
        };
        fs::write(store.path(), serde_json::to_vec(&settings).unwrap()).unwrap();

        let loaded = store.load_or_default(11).unwrap();

        assert!(loaded.recovered_corrupt_state);
        assert!(!loaded.settings.desired_enabled);
        assert!(loaded.corrupt_backup_path.unwrap().exists());
    }

    #[test]
    fn origin_normalization_rejects_secret_bearing_or_non_exact_values() {
        assert_eq!(
            normalize_https_origin("HTTPS://Desktop.Example:443/").unwrap(),
            "https://desktop.example"
        );
        assert_eq!(
            normalize_https_origin("https://[2001:db8::1]:8443").unwrap(),
            "https://[2001:db8::1]:8443"
        );
        for value in [
            "http://desktop.example",
            "https://user:pass@desktop.example",
            "https://desktop.example/path",
            "https://desktop.example/?token=secret",
            "https://desktop.example/#fragment",
        ] {
            assert!(normalize_https_origin(value).is_err(), "{value}");
        }
    }

    #[test]
    fn tailscale_parser_finds_dns_and_routes_without_exposing_json() {
        let status = br#"{
          "BackendState":"Running",
          "Peer":{"peer":{"DNSName":"peer.tailnet.ts.net."}},
          "Self":{"DNSName":"desktop.tailnet.ts.net."}
        }"#;
        let serve = br#"{
          "TCP":{"443":{"HTTPS":true},"8443":{"HTTPS":true}},
          "Web":{"desktop.tailnet.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:1428"}}}}
        }"#;
        let inspection = parse_tailscale_inspection(status, serve).unwrap();
        assert_eq!(
            inspection.dns_name.as_deref(),
            Some("desktop.tailnet.ts.net")
        );
        assert_eq!(inspection.routes.len(), 2);
        assert_eq!(inspection.routes[0].https_port, 443);
        assert_eq!(inspection.routes[0].path, "/");
        assert_eq!(inspection.routes[0].target, DIRECT_LOOPBACK_TARGET);
        assert_eq!(inspection.routes[1].https_port, 8443);
        assert!(inspection.routes[1].target.is_empty());
        assert!(!format!("{inspection:?}").contains("127.0.0.1:1428"));
    }

    #[test]
    fn fallback_port_selection_is_bounded_and_deterministic() {
        let inspection = TailscaleInspection {
            dns_name: None,
            routes: vec![TailscaleRoute {
                origin: "https://desktop.invalid:8443".to_string(),
                https_port: 8443,
                path: "/".to_string(),
                target: DIRECT_LOOPBACK_TARGET.to_string(),
                ownership: RemoteRouteOwnership::External,
            }],
        };
        assert_eq!(first_free_port(&inspection, &(8443..=8445)), Some(8444));
    }

    #[test]
    fn debug_output_does_not_include_process_bytes() {
        let output = ProcessOutput {
            status: Some(1),
            stdout: b"private-cli-output".to_vec(),
            stderr: b"private-error".to_vec(),
        };
        assert!(!format!("{output:?}").contains("private-cli-output"));
        assert!(!format!("{output:?}").contains("private-error"));
        let denied = ProcessOutput {
            status: Some(1),
            stdout: Vec::new(),
            stderr: b"permission denied: sensitive detail".to_vec(),
        };
        let error = tailscale_command_error(&denied, "fallback", "fallback");
        assert_eq!(error.code, "tailscale_permission_denied");
        assert!(!format!("{error:?}").contains("sensitive detail"));
    }

    #[tokio::test]
    async fn default_reconcile_has_no_network_or_process_side_effects() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, _) = test_controller(
            directory.path(),
            tailscale.clone(),
            direct.clone(),
            relay.clone(),
        )
        .await;

        let snapshot = controller.reconcile_on_startup().await.unwrap();

        assert!(!snapshot.desired_enabled);
        assert!(!snapshot.running);
        assert!(!gateway.status().running);
        assert_eq!(tailscale.inspect_calls.load(Ordering::SeqCst), 0);
        assert_eq!(direct.calls.load(Ordering::SeqCst), 0);
        assert_eq!(relay.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn process_pipe_reader_drains_but_bounds_retained_output() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(16 * 1024);
        let write = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; MAX_PROCESS_OUTPUT_BYTES + 17])
                .await
                .unwrap();
            writer.shutdown().await.unwrap();
        });
        let (output, truncated) = read_bounded_process_pipe(reader).await.unwrap();
        write.await.unwrap();

        assert_eq!(output.len(), MAX_PROCESS_OUTPUT_BYTES);
        assert!(truncated);
    }

    #[tokio::test]
    async fn duplicate_direct_actions_are_serialized_and_publish_one_candidate() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale, direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));

        let first = controller.clone();
        let second = controller.clone();
        let (first, second) = tokio::join!(
            first.enable_direct("https://desktop.example.test"),
            second.enable_direct("https://desktop.example.test")
        );
        first.unwrap();
        second.unwrap();

        let snapshot = controller.snapshot().await;
        assert_eq!(snapshot.direct_route_count, 1);
        assert!(snapshot.gateway_running);
        assert_eq!(direct.calls.load(Ordering::SeqCst), 2);
        assert_eq!(direct.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(
            controller
                .gateway()
                .current_config()
                .pairing_routes
                .direct_candidates
                .len(),
            1
        );
        controller
            .disable_method(RemoteConnectivityMethod::Direct)
            .await
            .unwrap();
        assert!(!gateway.status().running);
    }

    #[tokio::test]
    async fn claimed_offer_records_only_the_entry_that_completed_pairing() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale, direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));
        controller
            .enable_direct("https://direct.example.test")
            .await
            .unwrap();
        assert_eq!(
            controller.snapshot().await.last_successful_pairing_entry,
            None,
            "enabling a route must not change the pairing preference"
        );

        let response = controller
            .create_pairing_offer(RemoteDevicePermissionLevel::ReadOnly, 90_000)
            .unwrap();
        let offer_id = response.offer.summary.offer_id.clone();
        let debug = format!("{response:?}");
        assert!(!debug.contains(&response.offer.one_time_challenge));
        assert!(!debug.contains(&response.launch_fragment));
        assert!(
            controller
                .pairing_offer_status(&offer_id)
                .unwrap()
                .claimed_device_id
                .is_none()
        );
        assert_eq!(
            controller
                .record_claimed_pairing_entry(&offer_id, RemoteConnectivityMethod::Direct)
                .await
                .unwrap_err()
                .code,
            "remote_pairing_offer_not_claimed"
        );
        assert_eq!(
            controller.snapshot().await.last_successful_pairing_entry,
            None
        );

        let claim = RemoteClaimPairingOfferRequest {
            offer_id: offer_id.clone(),
            one_time_challenge: response.offer.one_time_challenge,
            expected_server_id: response.offer.summary.server_id,
            expected_server_identity_public_key: response.offer.summary.server_identity_public_key,
            display_name: "Pairing integration phone".to_string(),
            device_identity_public_key: URL_SAFE_NO_PAD.encode([7_u8; 32]),
            claim_nonce: "pairing-integration-claim-nonce".to_string(),
        };
        let challenge = claim.one_time_challenge.clone();
        assert!(!format!("{claim:?}").contains(&challenge));
        let claimed = gateway.relay_claim_pairing_offer(claim).unwrap();
        assert_eq!(
            controller
                .pairing_offer_status(&offer_id)
                .unwrap()
                .claimed_device_id,
            Some(claimed.device.device_id)
        );

        let snapshot = controller
            .record_claimed_pairing_entry(&offer_id, RemoteConnectivityMethod::Direct)
            .await
            .unwrap();
        assert_eq!(
            snapshot.last_successful_pairing_entry,
            Some(RemoteConnectivityMethod::Direct)
        );
        assert_eq!(
            RemoteConnectivityStore::for_home(directory.path())
                .load()
                .unwrap()
                .unwrap()
                .last_successful_pairing_entry,
            Some(RemoteConnectivityMethod::Direct)
        );
    }

    #[tokio::test]
    async fn relay_route_only_update_does_not_restart_the_direct_gateway() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale, direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));
        controller
            .enable_direct("https://direct.example.test")
            .await
            .unwrap();
        let before = gateway.status();
        let mut routes = controller.aggregate_routes().await;
        routes.relay_candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: "https://relay.example.test".to_string(),
            relay_room_id: Some(RelayRoomId::new()),
            relay_pc_peer_id: Some(RelayPeerId::new()),
            relay_pc_public_key: Some("relay-public-key".to_string()),
        });

        controller
            .configure_gateway(
                routes,
                vec!["https://direct.example.test".to_string()],
                Some(controller.resolve_web_assets().await.unwrap()),
                true,
            )
            .await
            .unwrap();

        let after = gateway.status();
        assert_eq!(after.bound_addr, before.bound_addr);
        assert_eq!(after.session_epoch, before.session_epoch);
        assert!(
            gateway
                .current_config()
                .pairing_routes
                .relay_candidate
                .is_some()
        );
        controller
            .disable_method(RemoteConnectivityMethod::Direct)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn tailscale_conflict_requires_confirmation_and_preserves_direct_route() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let tailscale = Arc::new(FakeTailscale {
            inspection: Mutex::new(TailscaleInspection {
                dns_name: Some("desktop.tailnet.ts.net".to_string()),
                routes: vec![TailscaleRoute {
                    origin: "https://desktop.tailnet.ts.net".to_string(),
                    https_port: 443,
                    path: "/".to_string(),
                    target: "http://127.0.0.1:9000".to_string(),
                    ownership: RemoteRouteOwnership::External,
                }],
            }),
            ..FakeTailscale::default()
        });
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale.clone(), direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));
        controller
            .enable_direct("https://direct.example.test")
            .await
            .unwrap();

        let confirmation = controller.enable_tailscale(None).await.unwrap();
        let method = confirmation
            .method(RemoteConnectivityMethod::TailscaleServe)
            .unwrap();
        assert_eq!(method.state, RemoteMethodState::ConfirmationNeeded);
        assert_eq!(method.https_port, Some(8443));
        assert!(
            confirmation
                .method(RemoteConnectivityMethod::Direct)
                .unwrap()
                .candidate_available
        );
        assert!(tailscale.create_calls.lock().unwrap().is_empty());

        let online = controller.enable_tailscale(Some(8443)).await.unwrap();
        assert_eq!(online.direct_route_count, 2);
        assert_eq!(
            online
                .method(RemoteConnectivityMethod::TailscaleServe)
                .unwrap()
                .ownership,
            RemoteRouteOwnership::DesktopCreated
        );
        assert_eq!(*tailscale.create_calls.lock().unwrap(), vec![8443]);

        let direct_only = controller
            .disable_method(RemoteConnectivityMethod::TailscaleServe)
            .await
            .unwrap();
        assert_eq!(direct_only.direct_route_count, 1);
        assert!(direct_only.gateway_running);
        assert_eq!(*tailscale.remove_calls.lock().unwrap(), vec![8443]);
        let inspection = tailscale.inspection.lock().await.clone();
        assert_eq!(inspection.routes.len(), 1);
        assert_eq!(inspection.routes[0].target, "http://127.0.0.1:9000");

        controller
            .disable_method(RemoteConnectivityMethod::Direct)
            .await
            .unwrap();
        assert!(!gateway.status().running);
    }

    #[tokio::test]
    async fn existing_exact_tailscale_handler_is_external_and_never_removed() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let tailscale = Arc::new(FakeTailscale {
            inspection: Mutex::new(TailscaleInspection {
                dns_name: Some("desktop.tailnet.ts.net".to_string()),
                routes: vec![TailscaleRoute {
                    origin: "https://desktop.tailnet.ts.net".to_string(),
                    https_port: 443,
                    path: "/".to_string(),
                    target: DIRECT_LOOPBACK_TARGET.to_string(),
                    ownership: RemoteRouteOwnership::External,
                }],
            }),
            ..FakeTailscale::default()
        });
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale.clone(), direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));

        let snapshot = controller.enable_tailscale(None).await.unwrap();
        assert_eq!(
            snapshot
                .method(RemoteConnectivityMethod::TailscaleServe)
                .unwrap()
                .ownership,
            RemoteRouteOwnership::External
        );
        controller
            .disable_method(RemoteConnectivityMethod::TailscaleServe)
            .await
            .unwrap();
        assert!(tailscale.remove_calls.lock().unwrap().is_empty());
        assert_eq!(tailscale.inspection.lock().await.routes.len(), 1);
    }

    #[tokio::test]
    async fn repair_of_interrupted_tailscale_disable_finishes_cleanup_without_reenabling() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        let origin = "https://desktop.tailnet.ts.net:8443".to_string();
        let settings = RemoteConnectivitySettingsV1 {
            tailscale: TailscaleSettings {
                origin: Some(origin.clone()),
                https_port: Some(8443),
                ownership: RemoteRouteOwnership::DesktopCreated,
                ..TailscaleSettings::default()
            },
            ..RemoteConnectivitySettingsV1::default()
        };
        store.save(&settings).unwrap();
        let tailscale = Arc::new(FakeTailscale {
            inspection: Mutex::new(TailscaleInspection {
                dns_name: Some("desktop.tailnet.ts.net".to_string()),
                routes: vec![TailscaleRoute {
                    origin,
                    https_port: 8443,
                    path: "/".to_string(),
                    target: DIRECT_LOOPBACK_TARGET.to_string(),
                    ownership: RemoteRouteOwnership::External,
                }],
            }),
            ..FakeTailscale::default()
        });
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, _, _) =
            test_controller(directory.path(), tailscale.clone(), direct, relay).await;

        let snapshot = controller
            .repair_method(RemoteConnectivityMethod::TailscaleServe)
            .await
            .unwrap();

        assert!(!snapshot.desired_enabled);
        assert_eq!(*tailscale.remove_calls.lock().unwrap(), vec![8443]);
        assert!(tailscale.create_calls.lock().unwrap().is_empty());
        assert!(tailscale.inspection.lock().await.routes.is_empty());
        let persisted = store.load().unwrap().unwrap();
        assert_eq!(persisted.tailscale.ownership, RemoteRouteOwnership::None);
        assert!(persisted.tailscale.origin.is_none());
        assert!(persisted.tailscale.https_port.is_none());
    }

    #[tokio::test]
    async fn startup_reconcile_completes_interrupted_direct_transition() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        let settings = RemoteConnectivitySettingsV1 {
            desired_enabled: true,
            generation: 7,
            direct: DirectSettings {
                desired_enabled: true,
                origin: Some("https://direct.example.test".to_string()),
                transition: Some(RemoteTransitionRecord {
                    kind: RemoteTransitionKind::Enabling,
                    generation: 7,
                    started_at_ms: 10,
                }),
            },
            ..RemoteConnectivitySettingsV1::default()
        };
        store.save(&settings).unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale, direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));

        let snapshot = controller.reconcile_on_startup().await.unwrap();
        assert_eq!(
            snapshot
                .method(RemoteConnectivityMethod::Direct)
                .unwrap()
                .state,
            RemoteMethodState::Online
        );
        assert!(store.load().unwrap().unwrap().direct.transition.is_none());
        controller
            .disable_method(RemoteConnectivityMethod::Direct)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn startup_reconcile_withholds_candidate_when_reconciled_state_cannot_persist() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        let settings = RemoteConnectivitySettingsV1 {
            desired_enabled: true,
            generation: 1,
            direct: DirectSettings {
                desired_enabled: true,
                origin: Some("https://direct.example.test".to_string()),
                transition: Some(RemoteTransitionRecord {
                    kind: RemoteTransitionKind::Enabling,
                    generation: 1,
                    started_at_ms: 10,
                }),
            },
            ..RemoteConnectivitySettingsV1::default()
        };
        store.save(&settings).unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale, direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));
        fs::remove_file(store.path()).unwrap();
        fs::create_dir(store.path()).unwrap();

        let snapshot = controller.reconcile_on_startup().await.unwrap();
        let method = snapshot.method(RemoteConnectivityMethod::Direct).unwrap();

        assert_eq!(method.state, RemoteMethodState::Error);
        assert_eq!(
            method.error_code.as_deref(),
            Some("remote_connectivity_settings_commit_failed")
        );
        assert_eq!(method.recovery_action, RemoteRecoveryAction::Retry);
        assert!(!method.candidate_available);
        assert!(!snapshot.gateway_running);
    }

    #[tokio::test]
    async fn explicit_enable_withholds_candidate_when_validated_state_cannot_persist() {
        let _guard = CONTROLLER_TEST_LOCK.lock().await;
        let directory = tempdir().unwrap();
        let store = RemoteConnectivityStore::for_home(directory.path());
        let tailscale = Arc::new(FakeTailscale::default());
        let direct = Arc::new(FakeDirectProbe::default());
        let relay = Arc::new(FakeRelayProbe::default());
        let (controller, gateway, descriptor) =
            test_controller(directory.path(), tailscale, direct.clone(), relay).await;
        *direct.info.lock().unwrap() = Some(direct_info(&gateway, &descriptor));

        let store_path = store.path().to_path_buf();
        *direct.on_probe.lock().unwrap() = Some(Box::new(move || {
            fs::remove_file(&store_path).unwrap();
            fs::create_dir(&store_path).unwrap();
        }));
        let error = controller
            .enable_direct("https://direct.example.test")
            .await
            .unwrap_err();

        assert_eq!(error.code, "remote_connectivity_settings_commit_failed");
        let snapshot = controller.snapshot().await;
        let method = snapshot.method(RemoteConnectivityMethod::Direct).unwrap();
        assert_eq!(method.state, RemoteMethodState::Error);
        assert!(!method.candidate_available);
        assert!(!snapshot.gateway_running);
    }

    #[test]
    fn relay_publication_requires_transport_pairing_assets_and_exact_release_build() {
        let expected = WebBuildDescriptor {
            schema_version: "vibex-web-build.v1".to_string(),
            build_id: "bbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            package_version: env!("CARGO_PKG_VERSION").to_string(),
            profile: "release".to_string(),
            git_commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            wasm_sha256: "1".repeat(64),
            glue_sha256: "2".repeat(64),
            static_sha256: "3".repeat(64),
        };
        let mut info = RelayPublicationInfo {
            protocol_version: RelayProtocolVersion::foundation(),
            features: RelayPublicationFeatures {
                pc_websocket: true,
                device_websocket: true,
                websocket_frames: true,
                http_pair_bridge: true,
                static_room_assets: true,
            },
            web_build: Some(expected.clone()),
        };
        info.validate_browser_bootstrap(&expected).unwrap();
        info.features.static_room_assets = false;
        assert_eq!(
            info.validate_browser_bootstrap(&expected).unwrap_err().code,
            "relay_browser_bootstrap_unavailable"
        );
        info.features.static_room_assets = true;
        info.web_build.as_mut().unwrap().build_id = "other".to_string();
        assert_eq!(
            info.validate_browser_bootstrap(&expected).unwrap_err().code,
            "relay_web_build_incompatible"
        );
    }

    #[test]
    fn web_asset_resolver_rejects_tampered_build_contents() {
        let directory = tempdir().unwrap();
        write_test_web_build(directory.path());
        fs::write(directory.path().join("pkg/vibex_web.js"), "tampered").unwrap();
        assert_eq!(
            load_web_build_descriptor(directory.path(), true)
                .unwrap_err()
                .code,
            "web_assets_incompatible"
        );
    }

    #[test]
    fn web_asset_resolver_rejects_tampered_service_worker() {
        let directory = tempdir().unwrap();
        write_test_web_build(directory.path());
        fs::write(directory.path().join("service-worker.js"), "tampered").unwrap();
        assert_eq!(
            load_web_build_descriptor(directory.path(), true)
                .unwrap_err()
                .code,
            "web_assets_incompatible"
        );
    }

    #[test]
    fn web_asset_resolver_accepts_a_source_bound_release_service_worker() {
        let directory = tempdir().unwrap();
        let descriptor = write_test_web_build_with_profile(directory.path(), "release");

        assert_eq!(descriptor.profile, "release");
        assert_eq!(
            load_web_build_descriptor(directory.path(), false).unwrap(),
            descriptor
        );
    }

    #[tokio::test]
    async fn tailscale_cli_uses_read_only_inspection_and_fixed_arguments() {
        let runner = FakeRunner {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![
                ProcessOutput {
                    status: Some(0),
                    stdout: br#"{"Web":{"HTTPS":443,"Proxy":"http://127.0.0.1:1428"}}"#.to_vec(),
                    stderr: Vec::new(),
                },
                ProcessOutput {
                    status: Some(0),
                    stdout: br#"{"DNSName":"desktop.tailnet.ts.net"}"#.to_vec(),
                    stderr: Vec::new(),
                },
            ]),
        };
        let cli = TailscaleCli::with_runner(runner);
        let inspection = cli.inspect().await.unwrap();
        assert_eq!(inspection.routes.len(), 1);
        let calls = cli.runner.calls.lock().unwrap().clone();
        assert_eq!(
            calls[0],
            (
                "tailscale".to_string(),
                vec!["status".to_string(), "--json".to_string()]
            )
        );
        assert_eq!(
            calls[1],
            (
                "tailscale".to_string(),
                vec![
                    "serve".to_string(),
                    "status".to_string(),
                    "--json".to_string()
                ]
            )
        );
        assert!(
            calls
                .iter()
                .all(|(_, args)| !args.iter().any(|arg| arg == "reset" || arg == "clear"))
        );
    }

    #[tokio::test]
    async fn tailscale_cli_create_and_remove_use_exact_mutation_arguments() {
        let status = br#"{"BackendState":"Running","Self":{"DNSName":"desktop.tailnet.ts.net."}}"#;
        let empty_serve = br#"{"TCP":{},"Web":{}}"#;
        let configured_serve = br#"{
          "TCP":{"8443":{"HTTPS":true}},
          "Web":{"desktop.tailnet.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:1428"}}}}
        }"#;
        let create_runner = FakeRunner {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![
                successful_process(configured_serve),
                successful_process(status),
                successful_process(b""),
                successful_process(empty_serve),
                successful_process(status),
            ]),
        };
        let create_cli = TailscaleCli::with_runner(create_runner);

        let route = create_cli
            .create(8443, DIRECT_LOOPBACK_TARGET)
            .await
            .unwrap();

        assert_eq!(route.ownership, RemoteRouteOwnership::DesktopCreated);
        assert_eq!(route.https_port, 8443);
        let create_calls = create_cli.runner.calls.lock().unwrap().clone();
        assert_eq!(
            create_calls[2],
            (
                "tailscale".to_string(),
                vec![
                    "serve".to_string(),
                    "--bg".to_string(),
                    "--https=8443".to_string(),
                    DIRECT_LOOPBACK_TARGET.to_string()
                ]
            )
        );
        assert_eq!(create_calls.len(), 5);

        let remove_runner = FakeRunner {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![
                successful_process(empty_serve),
                successful_process(status),
                successful_process(b""),
                successful_process(configured_serve),
                successful_process(status),
            ]),
        };
        let remove_cli = TailscaleCli::with_runner(remove_runner);

        remove_cli.remove_owned(&route).await.unwrap();

        let remove_calls = remove_cli.runner.calls.lock().unwrap().clone();
        assert_eq!(
            remove_calls[2],
            (
                "tailscale".to_string(),
                vec![
                    "serve".to_string(),
                    "--https=8443".to_string(),
                    "off".to_string()
                ]
            )
        );
        assert_eq!(remove_calls.len(), 5);
    }

    #[tokio::test]
    async fn tailscale_cli_refuses_to_remove_a_port_with_sibling_handlers() {
        let status = br#"{"BackendState":"Running","Self":{"DNSName":"desktop.tailnet.ts.net."}}"#;
        let sibling_serve = br#"{
          "TCP":{"8443":{"HTTPS":true}},
          "Web":{"desktop.tailnet.ts.net:8443":{"Handlers":{
            "/":{"Proxy":"http://127.0.0.1:1428"},
            "/admin":{"Proxy":"http://127.0.0.1:9000"}
          }}}
        }"#;
        let runner = FakeRunner {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![
                successful_process(sibling_serve),
                successful_process(status),
            ]),
        };
        let cli = TailscaleCli::with_runner(runner);
        let route = TailscaleRoute {
            origin: "https://desktop.tailnet.ts.net:8443".to_string(),
            https_port: 8443,
            path: "/".to_string(),
            target: DIRECT_LOOPBACK_TARGET.to_string(),
            ownership: RemoteRouteOwnership::DesktopCreated,
        };

        let error = cli.remove_owned(&route).await.unwrap_err();

        assert_eq!(error.code, "tailscale_route_ownership_mismatch");
        let calls = cli.runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|(_, args)| !args.iter().any(|arg| arg == "off"))
        );
    }

    #[tokio::test]
    async fn tailscale_cli_treats_an_already_absent_owned_route_as_removed() {
        let status = br#"{"BackendState":"Running","Self":{"DNSName":"desktop.tailnet.ts.net."}}"#;
        let runner = FakeRunner {
            calls: StdMutex::new(Vec::new()),
            responses: StdMutex::new(vec![
                successful_process(br#"{"TCP":{},"Web":{}}"#),
                successful_process(status),
            ]),
        };
        let cli = TailscaleCli::with_runner(runner);
        let route = TailscaleRoute {
            origin: "https://desktop.tailnet.ts.net:8443".to_string(),
            https_port: 8443,
            path: "/".to_string(),
            target: DIRECT_LOOPBACK_TARGET.to_string(),
            ownership: RemoteRouteOwnership::DesktopCreated,
        };

        cli.remove_owned(&route).await.unwrap();

        let calls = cli.runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|(_, args)| !args.iter().any(|arg| arg == "off"))
        );
    }
}

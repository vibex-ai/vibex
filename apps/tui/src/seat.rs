//! Seat resolution: deciding which `DesktopRuntime` this client talks to, and
//! how.
//!
//! Three outcomes are possible and the choice is made once, here, before the
//! interface starts:
//!
//! * **Authority seat** — this process acquires the home lock and starts the
//!   runtime itself. Only possible when nothing else owns the home.
//! * **Remote seat, explicit** — the user passed a `vibex://` link, a pairing
//!   code, or a saved credential.
//! * **Remote seat, local bootstrap** — another runtime owns the home, so the
//!   client mints a device credential that the *same machine* can use. The
//!   trust anchor is the runtime's identity public key, read from the protected
//!   identity file on this machine's filesystem, not from the network.
//!
//! Everything here is deliberately outside `crates/vibex-tui`: the library is a
//! client and must not know how to start or reach a runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vibex_backend::{BackendFacade, NativeBackend};
use vibex_core::{
    RemoteClientType, RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel,
    RemotePairingCodeLink,
};
use vibex_desktop_runtime::{
    DesktopHomeLock, DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade,
};
use vibex_remote::{RemoteIdentityStore, RemoteTrustService};
use vibex_remote_client::{
    AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity, DirectCandidate,
    RemoteClientConfig, WebRemoteBackend, claim_pairing_code_link_with_identity,
    claim_pairing_code_with_identity,
};
use vibex_tui::SeatKind;

/// Errors that describe an actionable situation rather than a stack trace.
#[derive(Debug)]
pub enum SeatError {
    /// Another runtime owns this home and it is not accepting local clients.
    HomeLocked { home: PathBuf },
    /// The requested remote runtime could not be reached.
    Unreachable { url: String, detail: String },
    /// The provided link or code was rejected.
    Rejected { detail: String },
    /// Something environmental went wrong (I/O, database, identity file).
    Environment { detail: String },
}

impl std::fmt::Display for SeatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeatError::HomeLocked { home } => write!(
                formatter,
                "Another Vibex runtime already owns {}.\n\n\
                 Vibex Desktop is running and is not accepting local clients. Either:\n\
                 \x20 1. enable Settings → Remote Access → Direct in the desktop app, then retry\n\
                 \x20 2. quit the desktop app and retry\n\
                 \x20 3. connect to another runtime with `vibex connect <vibex://…>`",
                home.display()
            ),
            SeatError::Unreachable { url, detail } => {
                write!(formatter, "could not reach the runtime at {url}: {detail}")
            }
            SeatError::Rejected { detail } => {
                write!(formatter, "the runtime rejected this client: {detail}")
            }
            SeatError::Environment { detail } => write!(formatter, "{detail}"),
        }
    }
}

impl std::error::Error for SeatError {}

/// A resolved, ready-to-run client.
pub struct Seat {
    pub kind: SeatKind,
    pub facade: BackendFacade,
    /// The home this client resolved. The authority seat keeps a handle to the
    /// runtime so the home lock lives as long as the process.
    pub home: PathBuf,
    _runtime: Option<Arc<DesktopRuntime>>,
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub struct SeatRequest {
    /// Connect to an explicit link or pairing code instead of looking locally.
    pub connect: Option<String>,
    /// Use this home directory instead of the environment default.
    pub home: Option<PathBuf>,
    /// Force the authority seat even when a remote credential exists.
    pub prefer_authority: bool,
}

/// The loopback address a desktop runtime listens on when Remote Access →
/// Direct is enabled.
pub const DESKTOP_DIRECT_LOOPBACK: &str = "http://127.0.0.1:1428";

/// The loopback address a headless server listens on by default.
pub const SERVER_LOOPBACK: &str = "http://127.0.0.1:8765";

impl Seat {
    /// Resolve the seat and construct the facade.
    pub async fn resolve(request: SeatRequest) -> Result<Self, SeatError> {
        // An explicit target always wins: the user asked for a specific
        // runtime, so do not second-guess them with local discovery.
        if let Some(target) = request.connect.as_deref() {
            return Self::connect(target, &request).await;
        }

        let home = resolve_home(request.home.clone())?;
        let config = home_config(&home)?;

        if request.prefer_authority || !credential_path(&home).exists() {
            // Try to become the authority. The lock is the only honest way to
            // find out whether this machine already has one.
            match DesktopHomeLock::acquire(&home, &config.application_id) {
                Ok(lock) => {
                    // Release immediately: `DesktopRuntime::start` takes the
                    // lock itself and would deadlock against our own handle.
                    drop(lock);
                    return Self::authority(config, home).await;
                }
                Err(error) if error.code == "desktop_runtime_home_locked" => {
                    if request.prefer_authority {
                        return Err(SeatError::HomeLocked { home });
                    }
                }
                Err(error) => {
                    return Err(SeatError::Environment {
                        detail: format!("could not probe the runtime home lock: {error}"),
                    });
                }
            }
        }

        // A runtime owns the home. Reuse a saved credential when there is one.
        if let Some(seat) = Self::saved_credential(&home).await? {
            return Ok(seat);
        }

        // Otherwise bootstrap a device credential for this machine.
        match Self::bootstrap_local(home.clone(), config).await {
            Ok(seat) => Ok(seat),
            Err(error) => {
                if matches!(error, SeatError::Unreachable { .. }) {
                    // The runtime is running but not listening: the lock is the
                    // signal, so report the actionable message rather than a
                    // connection error the user cannot act on.
                    Err(SeatError::HomeLocked { home })
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Start the runtime in this process and wrap it as the authority seat.
    async fn authority(mut config: DesktopRuntimeConfig, home: PathBuf) -> Result<Self, SeatError> {
        config.acquire_home_lock = true;
        // A locally started runtime must accept its own clients, otherwise the
        // next `vibex tui` on this machine hits the same wall.
        config.remote_gateway.service.enabled = true;
        let runtime =
            DesktopRuntime::start(config)
                .await
                .map_err(|error| SeatError::Environment {
                    detail: format!("could not start the runtime: {error}"),
                })?;
        let facade = Arc::new(NativeBackend::new(runtime.clone())).facade();
        Ok(Self {
            kind: SeatKind::Authority,
            facade,
            home,
            _runtime: Some(runtime),
        })
    }

    /// Attach to an explicit link, code, or URL.
    async fn connect(target: &str, request: &SeatRequest) -> Result<Self, SeatError> {
        let home = resolve_home(request.home.clone())?;
        let target = target.trim();

        let bundle = if target.starts_with("vibex://") {
            let link =
                RemotePairingCodeLink::parse(target).map_err(|error| SeatError::Rejected {
                    detail: error.to_string(),
                })?;
            claim_pairing_code_link_with_identity(link, "Vibex TUI", false, None)
                .await
                .map_err(|error| SeatError::Rejected {
                    detail: error.message,
                })?
        } else if target.contains("://") {
            // A bare URL is treated as a saved-credential lookup; a URL alone
            // carries no auth, so it can only work when we already paired.
            return Self::from_saved_url(&home, target).await;
        } else {
            claim_pairing_code_with_identity(target, target, "Vibex TUI", false, None)
                .await
                .map_err(|error| SeatError::Rejected {
                    detail: error.message,
                })?
        };

        // A bare code carries no certificate, so the claim used the loopback
        // plain-HTTP bootstrap path and the seat keeps that shape.
        let pinned: Option<String> = None;
        let seat = Self::from_bundle(&bundle, pinned.clone(), &home)?;
        save_bundle_credential(&home, &bundle, pinned.as_deref())?;
        Ok(seat)
    }

    /// Use the credential saved by a previous pairing.
    async fn saved_credential(home: &Path) -> Result<Option<Self>, SeatError> {
        let path = credential_path(home);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(SeatError::Environment {
                    detail: format!("could not read {}: {error}", path.display()),
                });
            }
        };
        let stored: StoredCredential =
            serde_json::from_str(&raw).map_err(|error| SeatError::Environment {
                detail: format!("{} is not a valid credential: {error}", path.display()),
            })?;
        let identity = ClientDeviceIdentity::from_private_key_base64(
            stored.device_id.clone(),
            &stored.private_key,
        )
        .map_err(|error| SeatError::Environment {
            detail: error.message,
        })?;
        let remote = RemoteClientConfig::new(stored.server_url.clone(), stored.auth.clone())
            .with_device_identity(identity);
        let mut remote = remote;
        remote.expected_server_id = stored.server_id.clone();
        remote.expected_server_identity_public_key = stored.server_identity_public_key.clone();
        remote.pinned_tls_certificate_der = stored.pinned_tls_certificate_der.clone();
        remote.client_id = "vibex-tui".to_string();
        remote.client_type = RemoteClientType::Native;
        remote.allow_insecure_local_dev = stored.allow_insecure_local_dev;
        Self::from_remote_config(remote, stored.pinned_tls_certificate_der, None, home).map(Some)
    }

    async fn from_saved_url(home: &Path, url: &str) -> Result<Self, SeatError> {
        match Self::saved_credential(home).await? {
            Some(seat) if !seat.facade.capabilities().schema_version.is_empty() => Ok(seat),
            _ => Err(SeatError::Unreachable {
                url: url.to_string(),
                detail: "no saved credential matches this runtime; pair it first with `vibex connect <vibex://…>`"
                    .to_string(),
            }),
        }
    }

    /// Mint a device credential on this machine for the runtime that already
    /// owns the home.
    ///
    /// The trust anchor is the identity file on disk: reading it already
    /// requires being inside this machine's per-user file permissions, and the
    /// public key is then pinned on the transport so a different process cannot
    /// impersonate the runtime.
    async fn bootstrap_local(
        home: PathBuf,
        config: DesktopRuntimeConfig,
    ) -> Result<Self, SeatError> {
        let identity = RemoteIdentityStore::new(home.join("relay/desktop-identity.json"))
            .load_or_create()
            .map_err(|error| SeatError::Environment {
                detail: format!("could not read the runtime identity: {error}"),
            })?;
        let pinned = vibex_remote::pinned_tls_certificate_base64(&identity).ok();

        // The runtime is running, so the database is shared. WAL mode plus the
        // runtime's own 15 s busy timeout make a short-lived read safe, and this
        // is exactly how `vibex-server pairing-code` already behaves.
        let mut connection = vibex_db::open_database(&config.database_path).map_err(|error| {
            SeatError::Environment {
                detail: format!("could not open the runtime database: {error}"),
            }
        })?;
        vibex_db::apply_migrations(&mut connection).map_err(|error| SeatError::Environment {
            detail: format!("could not migrate the runtime database: {error}"),
        })?;
        let response = RemoteTrustService::create_pairing_code(
            &connection,
            RemoteCreatePairingCodeRequest {
                permission_level: RemoteDevicePermissionLevel::FullControl,
                ttl_ms: Some(120_000),
            },
        )
        .map_err(|error| SeatError::Environment {
            detail: format!("could not mint a local pairing code: {error}"),
        })?;

        let base_url = SERVER_LOOPBACK.to_string();
        let bundle = claim_pairing_code_with_identity(
            base_url.clone(),
            response.pairing_code.clone(),
            "Vibex TUI",
            true,
            None,
        )
        .await
        .map_err(|error| SeatError::Unreachable {
            url: base_url,
            detail: error.message,
        })?;

        let seat = Self::from_bundle(&bundle, pinned.clone(), &home)?;
        save_bundle_credential(&home, &bundle, pinned.as_deref())?;
        Ok(seat)
    }

    fn from_bundle(
        bundle: &vibex_remote_client::PairingCodeClientBundle,
        pinned: Option<String>,
        home: &Path,
    ) -> Result<Self, SeatError> {
        let mut remote = RemoteClientConfig::new(
            bundle.credential.server_url.clone(),
            bundle.credential.auth.clone(),
        )
        .with_device_identity(bundle.identity.clone());
        remote.expected_server_id = Some(bundle.server_id.clone());
        remote.expected_server_identity_public_key =
            bundle.credential.server_identity_public_key.clone();
        remote.pinned_tls_certificate_der = pinned.clone();
        remote.client_id = "vibex-tui".to_string();
        remote.client_type = RemoteClientType::Native;
        // Pinned TLS is chosen whenever the runtime advertised a certificate;
        // plain HTTP is only the explicit loopback bootstrap fallback.
        remote.allow_insecure_local_dev = pinned.is_none();
        Self::from_remote_config(remote, pinned, None, home)
    }

    fn from_remote_config(
        remote: RemoteClientConfig,
        pinned: Option<String>,
        _bundle: Option<&vibex_remote_client::PairingCodeClientBundle>,
        home: &Path,
    ) -> Result<Self, SeatError> {
        remote.validate().map_err(|error| SeatError::Environment {
            detail: format!("the saved runtime address is not usable: {}", error.message),
        })?;
        let base_url = remote.base_url.clone();
        let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
            remote,
            direct_candidates: vec![DirectCandidate {
                url: base_url,
                label: "local-authority".to_string(),
                priority: 0,
                tls_certificate_der: pinned.clone(),
            }],
            relay: None,
        })
        .map_err(|error| SeatError::Environment {
            detail: format!("could not build the transport: {}", error.message),
        })?;
        let facade = Arc::new(WebRemoteBackend::from_auto(transport)).facade();
        Ok(Self {
            kind: SeatKind::Remote,
            facade,
            home: home.to_path_buf(),
            _runtime: None,
        })
    }

    /// Shut down an authority runtime cleanly.
    pub async fn shutdown(&self) {
        if let Some(runtime) = &self._runtime {
            let _ = runtime.shutdown().await;
        }
    }
}

/// The desktop runtime config for a given home.
///
/// The channel follows `VIBEX_CHANNEL` the same way the desktop binary does, so
/// `vibex tui` attaches to the home the user is actually running.
pub fn home_config(home: &Path) -> Result<DesktopRuntimeConfig, SeatError> {
    let channel = std::env::var("VIBEX_CHANNEL").unwrap_or_default();
    let mut config = match channel.trim().to_ascii_lowercase().as_str() {
        "stable" => DesktopRuntimeConfig::stable_default(),
        "rc" | "release-candidate" => DesktopRuntimeConfig::release_candidate_default(),
        _ => DesktopRuntimeConfig::preview_default(),
    }
    .map_err(|error| SeatError::Environment {
        detail: format!("could not resolve the runtime configuration: {error}"),
    })?;
    config.home_dir = home.to_path_buf();
    config.database_path = home.join("vibex.db");
    // The TUI may run a delegated agent, so point the sidecar at this binary,
    // exactly as `vibex-server` does.
    config.delegation_sidecar_command = std::env::current_exe().ok();
    Ok(config)
}

/// Resolve the Vibex home directory the same way the other binaries do.
pub fn resolve_home(explicit: Option<PathBuf>) -> Result<PathBuf, SeatError> {
    if let Some(home) = explicit {
        return Ok(home);
    }
    if let Ok(home) = std::env::var("VIBEX_HOME")
        && !home.trim().is_empty()
    {
        return Ok(PathBuf::from(home));
    }
    if let Ok(database) = std::env::var("VIBEX_DB_PATH")
        && !database.trim().is_empty()
        && let Some(parent) = Path::new(&database).parent()
    {
        return Ok(parent.to_path_buf());
    }
    let base = std::env::var("HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| SeatError::Environment {
            detail: "could not determine a home directory; set VIBEX_HOME".to_string(),
        })?;
    let channel = std::env::var("VIBEX_CHANNEL").unwrap_or_default();
    let leaf = match channel.trim().to_ascii_lowercase().as_str() {
        "stable" => "desktop-stable",
        "rc" | "release-candidate" => "desktop-rc",
        _ => "desktop-preview",
    };
    Ok(base.join(".vibex").join(leaf))
}

/// Where the local device credential lives.
pub fn credential_path(home: &Path) -> PathBuf {
    home.join("local-clients").join("tui-credential.json")
}

/// The on-disk credential. It never contains a private key in a form the user
/// would paste anywhere, and the file is created mode 0600.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StoredCredential {
    server_url: String,
    auth: vibex_core::RemoteAuthProof,
    device_id: vibex_core::DeviceId,
    private_key: String,
    server_id: Option<String>,
    server_identity_public_key: Option<String>,
    pinned_tls_certificate_der: Option<String>,
    #[serde(default)]
    allow_insecure_local_dev: bool,
}

/// Persist a freshly claimed credential with owner-only permissions.
///
/// The private key is the client's own device key, not a provider secret, and
/// it is what lets the runtime re-recognise this machine without a new pairing
/// code. Mode 0600 is set at creation so there is no window where the file is
/// world-readable.
fn save_bundle_credential(
    home: &Path,
    bundle: &vibex_remote_client::PairingCodeClientBundle,
    pinned: Option<&str>,
) -> Result<(), SeatError> {
    let path = credential_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| SeatError::Environment {
            detail: format!("could not create {}: {error}", parent.display()),
        })?;
    }
    let stored = StoredCredential {
        server_url: bundle.credential.server_url.clone(),
        auth: bundle.credential.auth.clone(),
        device_id: bundle.identity.device_id().clone(),
        private_key: bundle.identity.private_key_base64(),
        server_id: Some(bundle.server_id.clone()),
        server_identity_public_key: bundle.credential.server_identity_public_key.clone(),
        pinned_tls_certificate_der: pinned.map(str::to_string),
        allow_insecure_local_dev: pinned.is_none(),
    };
    let encoded = serde_json::to_vec(&stored).map_err(|error| SeatError::Environment {
        detail: format!("could not encode the credential: {error}"),
    })?;
    write_private_file(&path, &encoded).map_err(|error| SeatError::Environment {
        detail: format!("could not write {}: {error}", path.display()),
    })
}

fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
        file.flush()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_resolution_prefers_the_explicit_path() {
        let home = resolve_home(Some(PathBuf::from("/tmp/explicit"))).unwrap();
        assert_eq!(home, PathBuf::from("/tmp/explicit"));
    }

    #[test]
    fn credential_path_lives_under_local_clients() {
        let path = credential_path(Path::new("/data/vibex"));
        assert!(path.ends_with("local-clients/tui-credential.json"));
    }

    #[test]
    fn locked_home_message_offers_every_way_out() {
        let error = SeatError::HomeLocked {
            home: PathBuf::from("/home/dev/.vibex/desktop-preview"),
        };
        let text = error.to_string();
        assert!(text.contains("Remote Access"));
        assert!(text.contains("quit the desktop app"));
        assert!(text.contains("vibex connect"));
    }

    #[test]
    fn unreachable_message_names_the_address() {
        let error = SeatError::Unreachable {
            url: "http://127.0.0.1:8765".to_string(),
            detail: "connection refused".to_string(),
        };
        assert!(error.to_string().contains("127.0.0.1:8765"));
    }

    #[test]
    fn bootstrap_addresses_are_loopback_only() {
        // A LAN or public default here would silently expose the runtime.
        assert!(SERVER_LOOPBACK.contains("127.0.0.1"));
        assert!(DESKTOP_DIRECT_LOOPBACK.contains("127.0.0.1"));
    }
}

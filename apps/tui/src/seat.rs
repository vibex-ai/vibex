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
    claim_pairing_code_with_identity, claim_pairing_code_with_identity_and_certificate,
};
use vibex_tui::SeatKind;

/// Errors that describe an actionable situation rather than a stack trace.
#[derive(Debug)]
pub enum SeatError {
    /// Another runtime owns this home and it is not accepting local clients.
    HomeLocked {
        home: PathBuf,
        /// The endpoints that were dialled before giving up, so the message
        /// names the doors that turned out to be closed instead of guessing at
        /// one of them.
        endpoints: Vec<String>,
        /// Why the last attempt failed.
        detail: String,
    },
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
            SeatError::HomeLocked {
                home,
                endpoints,
                detail,
            } => write!(
                formatter,
                "Another Vibex runtime already owns {} and is not accepting \
                 clients on this machine.\n\n\
                 Tried: {}\n\
                 Last error: {}\n\n\
                 Either:\n\
                 \x20 1. open Settings → General in the desktop app and turn on Vibex TUI, then retry\n\
                 \x20 2. quit the desktop app and retry\n\
                 \x20 3. connect to another runtime with `vibex connect <vibex://…>`",
                home.display(),
                endpoints.join(", "),
                detail
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
    /// What the runtime on the other end is.
    ///
    /// The interface answers two different questions with it — which clients
    /// this runtime can pair, and whether it can advertise itself on the local
    /// network — and neither is derivable from the seat: the same authority
    /// seat is a desktop shell on one machine and a headless daemon on another.
    /// `Unknown` is a real answer: a peer that predates the field is rendered
    /// without a kind rather than guessed at.
    pub runtime_kind: vibex_core::RemoteServerKind,
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
    /// Serve this channel instead of the one the binary was packaged as.
    ///
    /// `--channel` is the explicit spelling of `VIBEX_CHANNEL`, and it wins
    /// over both, because a flag on this run is a stronger statement than an
    /// exported variable. `--home` still wins over all three.
    pub channel: Option<String>,
    /// Force the authority seat even when a remote credential exists.
    pub prefer_authority: bool,
    /// Take the home if it has become free, and stay a client if it has not.
    ///
    /// This is what `Ctrl+T` restarts the client with. It differs from
    /// `prefer_authority` on purpose: the reader acted on an offer that was
    /// true a moment ago, so a runtime that has come back since must leave them
    /// with a working client rather than a refusal.
    pub take_over: bool,
}

/// The channel this binary was packaged as.
///
/// `scripts/build-channel.mjs` exports `VIBEX_CHANNEL` for the cargo invocation
/// that builds a channel package, so the value is baked into the binary the
/// same way the desktop app bakes its release identity. A developer build has
/// no channel and is a preview build.
///
/// This is what makes an installed RC or stable client open the home its own
/// desktop app owns: before it existed, a `vibex` copied out of the RC package
/// still looked in `desktop-preview` unless the reader exported an environment
/// variable first.
pub const COMPILED_CHANNEL: Option<&str> = option_env!("VIBEX_CHANNEL");

/// The loopback address a desktop runtime serves clients running on this
/// machine at.
///
/// It is the desktop's pinned-certificate Gateway narrowed to the loopback
/// address, so a same-machine client authenticates the runtime with the
/// identity key it reads from the home instead of trusting the loopback
/// interface. Unlike the Direct listener it is not part of Remote Access: it is
/// there whenever the desktop is.
pub const DESKTOP_LOCAL_CLIENT_LOOPBACK: &str = "https://127.0.0.1:1429";

/// The loopback address a desktop runtime listens on when Remote Access →
/// Direct is enabled.
pub const DESKTOP_DIRECT_LOOPBACK: &str = "http://127.0.0.1:1428";

/// The loopback address a headless server listens on by default.
pub const SERVER_LOOPBACK: &str = "http://127.0.0.1:8765";

/// One endpoint a local bootstrap may dial.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalCandidate {
    endpoint: String,
    /// Whether the runtime terminates TLS there with the certificate derived
    /// from its identity file. A route without a pin is only ever the explicit
    /// loopback development exception.
    pinned: bool,
}

/// Where a headless server answers local clients: `VIBEX_BIND_ADDR` when it is
/// set, the documented default otherwise.
fn server_loopback_endpoint() -> String {
    std::env::var("VIBEX_BIND_ADDR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.contains("://") {
                value
            } else {
                format!("http://{value}")
            }
        })
        .unwrap_or_else(|| SERVER_LOOPBACK.to_string())
}

/// Every endpoint a runtime owning this home may answer local clients on, in
/// the order they should be tried.
///
/// The desktop flavours serve same-machine clients on the pinned-certificate
/// Gateway first. The Direct listener stays behind it as a fallback, so a
/// desktop that predates the local-client listener keeps working instead of
/// regressing into "not accepting local clients". A headless server has neither
/// listener and moves with `VIBEX_BIND_ADDR`.
fn local_candidates(flavour: HomeFlavour) -> Vec<LocalCandidate> {
    match flavour {
        HomeFlavour::Preview | HomeFlavour::ReleaseCandidate | HomeFlavour::Stable => vec![
            LocalCandidate {
                endpoint: DESKTOP_LOCAL_CLIENT_LOOPBACK.to_string(),
                pinned: true,
            },
            LocalCandidate {
                endpoint: DESKTOP_DIRECT_LOOPBACK.to_string(),
                pinned: false,
            },
        ],
        HomeFlavour::Server => vec![LocalCandidate {
            endpoint: server_loopback_endpoint(),
            pinned: false,
        }],
    }
}

/// Where a runtime that owns a given home answers local clients.
///
/// This is the first candidate: the endpoint a fresh bootstrap dials, and the
/// one `status` should name. Dialling a listener the runtime does not have
/// produces a connection error that looks like "the runtime is not accepting
/// clients", which is the exact confusion the seat messages exist to avoid.
pub fn loopback_endpoint(flavour: HomeFlavour) -> String {
    local_candidates(flavour)
        .into_iter()
        .next()
        .map(|candidate| candidate.endpoint)
        .unwrap_or_else(|| SERVER_LOOPBACK.to_string())
}

/// The endpoints a failure message should name, in the order they were dialled.
fn local_endpoint_list(flavour: HomeFlavour) -> Vec<String> {
    local_candidates(flavour)
        .into_iter()
        .map(|candidate| candidate.endpoint)
        .collect()
}

impl Seat {
    /// Resolve the seat and construct the facade.
    pub async fn resolve(request: SeatRequest) -> Result<Self, SeatError> {
        // An explicit target always wins: the user asked for a specific
        // runtime, so do not second-guess them with local discovery.
        if let Some(target) = request.connect.as_deref() {
            return Self::connect(target, &request).await;
        }

        let channel = requested_channel(&request);
        let home = resolve_home_for_channel(request.home.clone(), channel.as_deref())?;
        let config = home_config(&home)?;

        if request.prefer_authority || request.take_over || !credential_path(&home).exists() {
            match Self::take_authority(&home, &config).await? {
                Some(seat) => return Ok(seat),
                None if request.prefer_authority => {
                    return Err(SeatError::HomeLocked {
                        endpoints: local_endpoint_list(HomeFlavour::of(&home)),
                        home,
                        detail: "`--local` was requested, so no client endpoint was dialled"
                            .to_string(),
                    });
                }
                // A takeover the reader asked for may have been overtaken by a
                // runtime starting up again; carrying on as a client is the
                // answer that leaves them with something that works.
                None => {}
            }
        }

        // A runtime owns the home. Reuse a saved credential when there is one.
        //
        // A credential can outlive the endpoint it was minted for, and both ways
        // that happens are recoverable here, so a dead one is not reported until
        // they have been ruled out: a free lock means this client can be the
        // runtime the credential was for, and a taken lock means the runtime
        // moved its door and discovery can find it.
        match Self::saved_credential(&home).await {
            Ok(Some(seat)) => return Ok(seat),
            Ok(None) => {}
            Err(error @ SeatError::Unreachable { .. }) => {
                if let Some(seat) = Self::take_authority(&home, &config).await? {
                    return Ok(seat);
                }
                return match Self::bootstrap_local(home.clone(), config).await {
                    Ok(seat) => Ok(seat),
                    // Discovery is a second opinion about the same runtime, so
                    // its connection failure is not a second failure to report.
                    Err(SeatError::Unreachable { .. }) => Err(error),
                    Err(other) => Err(other),
                };
            }
            Err(error) => return Err(error),
        }

        // Otherwise bootstrap a device credential for this machine.
        match Self::bootstrap_local(home.clone(), config).await {
            Ok(seat) => Ok(seat),
            Err(SeatError::Unreachable { url, detail }) => {
                // The runtime is running but not listening on any endpoint this
                // client knows: the lock is the signal, so report the actionable
                // message rather than the last connection error alone.
                Err(SeatError::HomeLocked {
                    endpoints: local_endpoint_list(HomeFlavour::of(&home)),
                    home,
                    detail: format!("{url}: {detail}"),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Become the authority, if nothing else owns the home.
    ///
    /// `None` means another process holds it, which is an answer rather than a
    /// retry: the lock is the only honest way to find out whether this machine
    /// already has a runtime.
    async fn take_authority(
        home: &Path,
        config: &DesktopRuntimeConfig,
    ) -> Result<Option<Self>, SeatError> {
        match DesktopHomeLock::acquire(home, &config.application_id) {
            Ok(lock) => {
                // Release immediately: `DesktopRuntime::start` takes the lock
                // itself and would deadlock against our own handle.
                drop(lock);
                Self::authority(config.clone(), home.to_path_buf())
                    .await
                    .map(Some)
            }
            Err(error) if error.code == "desktop_runtime_home_locked" => Ok(None),
            Err(error) => Err(SeatError::Environment {
                detail: format!("could not probe the runtime home lock: {error}"),
            }),
        }
    }

    /// Start the runtime in this process and wrap it as the authority seat.
    async fn authority(mut config: DesktopRuntimeConfig, home: PathBuf) -> Result<Self, SeatError> {
        config.acquire_home_lock = true;
        // A locally started runtime must accept its own clients, otherwise the
        // next `vibex tui` on this machine hits the same wall.
        config.remote_gateway.service.enabled = true;
        // Read before the config is moved into the runtime: which flavour this
        // home is decides what the pairing page may offer.
        let runtime_kind = config.remote_gateway.server_kind;
        let runtime =
            DesktopRuntime::start(config)
                .await
                .map_err(|error| SeatError::Environment {
                    detail: format!("could not start the runtime: {error}"),
                })?;
        let facade = Arc::new(NativeBackend::new(runtime.clone())).facade();
        Ok(Self {
            kind: SeatKind::Authority,
            runtime_kind,
            facade,
            home,
            _runtime: Some(runtime),
        })
    }

    /// Attach to an explicit link, code, or URL.
    async fn connect(target: &str, request: &SeatRequest) -> Result<Self, SeatError> {
        let channel = requested_channel(request);
        let home = resolve_home_for_channel(request.home.clone(), channel.as_deref())?;
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
        let seat = Self::from_bundle(&bundle, pinned.clone(), &home).await?;
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
        Self::from_remote_config(
            remote,
            stored.pinned_tls_certificate_der,
            stored.server_kind,
            home,
        )
        .await
        .map(Some)
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
    /// impersonate the runtime. The certificate the runtime terminates TLS with
    /// is derived from that same file, which is why a pinned route needs no
    /// operator-supplied link.
    ///
    /// Which endpoint answers is discovered by dialling rather than by reading
    /// the runtime's settings: the client has to behave the same against a
    /// desktop whose home it can read and a runtime whose configuration it never
    /// sees, and the settings file is not a promise that a listener came up.
    async fn bootstrap_local(
        home: PathBuf,
        config: DesktopRuntimeConfig,
    ) -> Result<Self, SeatError> {
        let identity = RemoteIdentityStore::new(home.join("relay/desktop-identity.json"))
            .load_or_create()
            .map_err(|error| SeatError::Environment {
                detail: format!("could not read the runtime identity: {error}"),
            })?;
        let candidates = local_candidates(HomeFlavour::of(&home));

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

        let mut last = SeatError::Unreachable {
            url: local_endpoint_list(HomeFlavour::of(&home)).join(", "),
            detail: "no local endpoint is configured for this home".to_string(),
        };
        for candidate in candidates {
            // A pairing code is single use, so every attempt mints its own
            // rather than letting a failed dial burn the one code for the next
            // candidate.
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
            // Only a listener that actually terminates TLS can be pinned. Under
            // the loopback development policy the runtime serves plain HTTP, and
            // asking for a pin there is rejected by the client's own validation.
            let pin = candidate
                .pinned
                .then(|| vibex_remote::pinned_tls_certificate_base64(&identity).ok())
                .flatten();
            let claim = claim_pairing_code_with_identity_and_certificate(
                candidate.endpoint.clone(),
                response.pairing_code.clone(),
                "Vibex TUI",
                !candidate.pinned,
                pin.clone(),
                None,
            )
            .await;
            let bundle = match claim {
                Ok(bundle) => bundle,
                Err(error) => {
                    last = SeatError::Unreachable {
                        url: candidate.endpoint,
                        detail: error.message,
                    };
                    continue;
                }
            };
            let seat = Self::from_bundle(&bundle, pin.clone(), &home).await?;
            save_bundle_credential(&home, &bundle, pin.as_deref())?;
            return Ok(seat);
        }
        Err(last)
    }

    async fn from_bundle(
        bundle: &vibex_remote_client::PairingCodeClientBundle,
        pinned: Option<String>,
        home: &Path,
    ) -> Result<Self, SeatError> {
        let url = bundle.credential.server_url.clone();
        // The scheme decides the security model, not the availability of a
        // certificate: a runtime in the `loopback_http` policy serves plain
        // HTTP and has no use for a pin, while pinning a plain-HTTP URL is
        // rejected outright by `RemoteClientConfig::validate`.
        let is_tls = url.starts_with("https://") || url.starts_with("wss://");
        let pin = if is_tls { pinned } else { None };
        let mut remote = RemoteClientConfig::new(url, bundle.credential.auth.clone())
            .with_device_identity(bundle.identity.clone());
        remote.expected_server_id = Some(bundle.server_id.clone());
        // The identity public key is pinned over the wire when the route is
        // encrypted. Over plain loopback HTTP the same key still comes from the
        // local identity file, so the comparison is meaningful there too.
        remote.expected_server_identity_public_key =
            bundle.credential.server_identity_public_key.clone();
        remote.pinned_tls_certificate_der = pin.clone();
        remote.client_id = "vibex-tui".to_string();
        remote.client_type = RemoteClientType::Native;
        remote.allow_insecure_local_dev = !is_tls;
        Self::from_remote_config(remote, pin, bundle.server_kind, home).await
    }

    /// Build the remote seat and attach it.
    ///
    /// The connection is opened here rather than left to the interface. A
    /// facade nobody connected answers every read with
    /// `remote_reselection_canceled`, which reaches the reader as an empty
    /// runtime catalogue and an empty session list — "Agent setup: unavailable"
    /// — instead of as a connection problem. Attaching at resolution turns that
    /// into one honest error before the interface is drawn, and every caller
    /// (`vibex`, `vibex-server tui`, `vibex-desktop tui`) gets a seat that is
    /// ready to run.
    async fn from_remote_config(
        remote: RemoteClientConfig,
        pinned: Option<String>,
        runtime_kind: vibex_core::RemoteServerKind,
        home: &Path,
    ) -> Result<Self, SeatError> {
        remote.validate().map_err(|error| SeatError::Environment {
            detail: format!("the saved runtime address is not usable: {}", error.message),
        })?;
        let base_url = remote.base_url.clone();
        let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
            remote,
            direct_candidates: vec![DirectCandidate {
                url: base_url.clone(),
                label: "local-authority".to_string(),
                priority: 0,
                tls_certificate_der: pinned.clone(),
            }],
            relay: None,
        })
        .map_err(|error| SeatError::Environment {
            detail: format!("could not build the transport: {}", error.message),
        })?;
        let backend = WebRemoteBackend::from_auto(transport);
        backend
            .connect()
            .await
            .map_err(|error| SeatError::Unreachable {
                url: base_url,
                detail: error.message,
            })?;
        let facade = Arc::new(backend).facade();
        Ok(Self {
            kind: SeatKind::Remote,
            runtime_kind,
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

/// Which runtime flavour a home belongs to.
///
/// The leaf of the path is authoritative. The desktop channels each own a
/// dedicated home (`desktop-preview`, `desktop-rc`, `desktop-stable`) and the
/// runtime refuses to start if the flavour and the leaf disagree, so deriving
/// the flavour from the path is the only way `--home` can work at all. A home
/// that is not a channel home is a server home, which has no isolation rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeFlavour {
    Preview,
    ReleaseCandidate,
    Stable,
    Server,
}

impl HomeFlavour {
    /// The directory leaf each flavour requires, or `None` for a server home.
    pub const fn leaf(self) -> Option<&'static str> {
        match self {
            HomeFlavour::Preview => Some("desktop-preview"),
            HomeFlavour::ReleaseCandidate => Some("desktop-rc"),
            HomeFlavour::Stable => Some("desktop-stable"),
            HomeFlavour::Server => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            HomeFlavour::Preview => "preview",
            HomeFlavour::ReleaseCandidate => "rc",
            HomeFlavour::Stable => "stable",
            HomeFlavour::Server => "server",
        }
    }

    /// Classify a home by its last path component.
    pub fn of(home: &Path) -> Self {
        match home
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
        {
            "desktop-preview" => Self::Preview,
            "desktop-rc" => Self::ReleaseCandidate,
            "desktop-stable" => Self::Stable,
            _ => Self::Server,
        }
    }
}

/// The desktop runtime config for a given home.
///
/// The flavour is derived from the home itself, and each channel is built
/// through its own constructor so the runtime's isolation check passes. A home
/// that is not a channel home gets the server flavour, which is what makes
/// `--home /tmp/scratch` a working throwaway runtime.
pub fn home_config(home: &Path) -> Result<DesktopRuntimeConfig, SeatError> {
    let mut config = match HomeFlavour::of(home) {
        // `isolated_*(base)` derives `<base>/<leaf>`, so the parent is the base.
        HomeFlavour::Preview => {
            DesktopRuntimeConfig::isolated_preview(home.parent().unwrap_or(home))
        }
        HomeFlavour::ReleaseCandidate => {
            DesktopRuntimeConfig::isolated_release_candidate(home.parent().unwrap_or(home))
        }
        HomeFlavour::Stable => {
            DesktopRuntimeConfig::isolated_release_stable(home.parent().unwrap_or(home))
        }
        HomeFlavour::Server => {
            let mut config =
                DesktopRuntimeConfig::headless_from_environment().map_err(|error| {
                    SeatError::Environment {
                        detail: format!("could not resolve the runtime configuration: {error}"),
                    }
                })?;
            config.home_dir = home.to_path_buf();
            config.database_path = home.join("vibex.db");
            config
        }
    };
    // The derived home must be the one that was asked for; silently running
    // against a different directory is worse than refusing.
    if config.home_dir != home {
        return Err(SeatError::Environment {
            detail: format!(
                "the {} runtime home is {} but {} was requested",
                HomeFlavour::of(home).label(),
                config.home_dir.display(),
                home.display()
            ),
        });
    }
    // The TUI may run a delegated agent, so point the sidecar at this binary,
    // exactly as `vibex-server` does.
    config.delegation_sidecar_command = std::env::current_exe().ok();
    Ok(config)
}

/// A diagnosis of what `vibex` would do with this home, without doing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatProbe {
    pub home: PathBuf,
    pub flavour: HomeFlavour,
    pub seat: SeatKind,
    /// Why the seat was chosen, in the user's terms.
    pub detail: String,
    /// The address a remote seat would use, when there is one.
    pub endpoint: Option<String>,
}

/// Report which seat this home would produce, without starting anything.
///
/// `status` exists to answer "why is this not working", so it must succeed
/// precisely when the real run would fail.
pub fn probe(home: &Path) -> SeatProbe {
    let flavour = HomeFlavour::of(home);
    let application_id = application_id(flavour);
    match DesktopHomeLock::acquire(home, application_id) {
        Ok(lock) => {
            drop(lock);
            SeatProbe {
                home: home.to_path_buf(),
                flavour,
                seat: SeatKind::Authority,
                detail: "no runtime owns this home; `vibex` would start one".to_string(),
                endpoint: None,
            }
        }
        Err(error) if error.code == "desktop_runtime_home_locked" => {
            let credential = credential_path(home);
            let (detail, endpoint) = if credential.exists() {
                (
                    "a runtime owns this home; `vibex` would attach with the saved credential"
                        .to_string(),
                    Some(read_saved_url(&credential).unwrap_or_else(|| loopback_endpoint(flavour))),
                )
            } else {
                (
                    "a runtime owns this home and no local credential exists yet; `vibex` would mint one over loopback, which needs the runtime to accept local clients"
                        .to_string(),
                    Some(loopback_endpoint(flavour)),
                )
            };
            SeatProbe {
                home: home.to_path_buf(),
                flavour,
                seat: SeatKind::Remote,
                detail,
                endpoint,
            }
        }
        Err(error) => SeatProbe {
            home: home.to_path_buf(),
            flavour,
            seat: SeatKind::Remote,
            detail: format!("the home lock could not be probed: {error}"),
            endpoint: None,
        },
    }
}

fn application_id(flavour: HomeFlavour) -> &'static str {
    match flavour {
        HomeFlavour::Preview => "dev.vibex.desktop.preview",
        HomeFlavour::ReleaseCandidate => "dev.vibex.desktop.rc",
        HomeFlavour::Stable => "dev.vibex.desktop",
        HomeFlavour::Server => "dev.vibex.server",
    }
}

fn read_saved_url(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let stored: StoredCredential = serde_json::from_str(&raw).ok()?;
    Some(stored.server_url)
}

/// Resolve the Vibex home directory the same way the other binaries do.
///
/// An explicit path is used **verbatim**: `--home` is the escape hatch for
/// pointing at a scratch or server home, so appending a channel leaf to it
/// would defeat the purpose. The environment path is also verbatim, for the
/// same reason `VIBEX_HOME` is honoured by every other binary. Only the
/// fallback derives a channel home under `~/.vibex`.
///
/// The channel comes from [`COMPILED_CHANNEL`] first, because that is the
/// channel the reader installed: a client from the RC package must open
/// `desktop-rc` without anybody exporting anything. `VIBEX_CHANNEL` overrides
/// it so one installed client can still be pointed at another channel's home,
/// and `status` reports which channel won.
pub fn resolve_home(explicit: Option<PathBuf>) -> Result<PathBuf, SeatError> {
    let channel = channel_override();
    resolve_home_for_channel(explicit, channel.as_deref())
}

/// The channel named for this run, or `None` to use the packaged one.
///
/// A value the resolver does not recognise is kept rather than dropped: the
/// caller is about to fail, and `vibex status` has to explain which spelling it
/// could not use instead of silently falling back to preview.
fn channel_override() -> Option<String> {
    std::env::var("VIBEX_CHANNEL")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

/// The channel this run asked for: the flag first, then the environment, and
/// `None` when neither named one and the packaged channel should win.
pub fn requested_channel(request: &SeatRequest) -> Option<String> {
    request
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .or_else(channel_override)
}

/// The home the channel named by `--channel` or `VIBEX_CHANNEL` uses.
pub fn resolve_home_for_channel(
    explicit: Option<PathBuf>,
    channel: Option<&str>,
) -> Result<PathBuf, SeatError> {
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
    // `HOME` is a POSIX convention, and Windows sets `USERPROFILE` instead, so
    // the platform's own answer is what this reads. Resolving `HOME` directly
    // made the packaged client unable to find a home at all on a stock Windows
    // shell, while the desktop app — which resolves through the same library —
    // found one without trouble.
    let base = vibex_tui::user_home::user_home().ok_or_else(|| SeatError::Environment {
        detail: "could not determine a home directory; set VIBEX_HOME".to_string(),
    })?;
    let leaf =
        channel_home_leaf(channel.or(COMPILED_CHANNEL)).ok_or_else(|| SeatError::Environment {
            detail: format!(
                "unknown channel {:?}; use `stable`, `rc`, or `preview`",
                channel.or(COMPILED_CHANNEL).unwrap_or_default()
            ),
        })?;
    Ok(base.join(".vibex").join(leaf))
}

/// The home leaf a channel name selects, or `None` when the name is not one of
/// the three channels this client can serve.
fn channel_home_leaf(channel: Option<&str>) -> Option<&'static str> {
    match channel
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("stable") => Some("desktop-stable"),
        Some("rc") | Some("release-candidate") => Some("desktop-rc"),
        Some("preview") | None => Some("desktop-preview"),
        Some(_) => None,
    }
}

/// The channel this run resolves to, as the label `status` prints.
pub fn resolved_channel(channel: Option<&str>) -> &'static str {
    match channel_home_leaf(channel.or(COMPILED_CHANNEL)) {
        Some("desktop-stable") => "stable",
        Some("desktop-rc") => "rc",
        _ => "preview",
    }
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
    /// What the runtime called itself when this credential was claimed. Kept
    /// beside the credential because the kind is a property of the runtime, not
    /// of the connection, and re-reading it would mean being online. A
    /// credential written before the field exists decodes as `Unknown`.
    #[serde(default)]
    server_kind: vibex_core::RemoteServerKind,
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
        server_kind: bundle.server_kind,
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

    /// The packaged client has to find a home on a platform that does not
    /// export `HOME`, which is what a stock Windows shell looks like. The
    /// resolver reads the platform's own answer, so the fallback is the user's
    /// home rather than a variable that may not exist.
    #[test]
    fn the_default_home_comes_from_the_platform_not_home() {
        let leaf = channel_home_leaf(COMPILED_CHANNEL).expect("the packaged channel is known");
        match vibex_tui::user_home::user_home() {
            Some(user) => assert_eq!(
                resolve_home_for_channel(None, COMPILED_CHANNEL).unwrap(),
                user.join(".vibex").join(leaf)
            ),
            // No home at all is the one case the resolver is allowed to refuse.
            None => assert!(resolve_home_for_channel(None, COMPILED_CHANNEL).is_err()),
        }
    }

    #[test]
    fn every_channel_has_its_own_home_leaf() {
        assert_eq!(channel_home_leaf(Some("stable")), Some("desktop-stable"));
        assert_eq!(channel_home_leaf(Some("rc")), Some("desktop-rc"));
        assert_eq!(
            channel_home_leaf(Some("release-candidate")),
            Some("desktop-rc")
        );
        assert_eq!(channel_home_leaf(Some("preview")), Some("desktop-preview"));
        // A developer build carries no channel and is a preview build, which is
        // what keeps `cargo run -p vibex-client` pointed at the dev home.
        assert_eq!(channel_home_leaf(None), Some("desktop-preview"));
        // A misspelling is refused rather than silently served by preview, so
        // the reader learns that nothing was opened instead of editing the
        // wrong home.
        assert_eq!(channel_home_leaf(Some("nightly")), None);
    }

    #[test]
    fn a_named_channel_beats_the_packaged_one() {
        // `--channel` is the flag, then `VIBEX_CHANNEL`, then the packaged
        // value: a reader who exported the variable for one run means it.
        let request = SeatRequest {
            channel: Some("stable".to_string()),
            ..SeatRequest::default()
        };
        assert_eq!(requested_channel(&request).as_deref(), Some("stable"));
        let request = SeatRequest {
            channel: Some("  RC  ".to_string()),
            ..SeatRequest::default()
        };
        assert_eq!(requested_channel(&request).as_deref(), Some("rc"));
    }

    #[test]
    fn the_flavour_comes_from_the_home_leaf() {
        // The runtime refuses to start when the flavour and the leaf disagree,
        // so the leaf has to win over any environment setting.
        assert_eq!(
            HomeFlavour::of(Path::new("/h/.vibex/desktop-preview")),
            HomeFlavour::Preview
        );
        assert_eq!(
            HomeFlavour::of(Path::new("/h/.vibex/desktop-rc")),
            HomeFlavour::ReleaseCandidate
        );
        assert_eq!(
            HomeFlavour::of(Path::new("/h/.vibex/desktop-stable")),
            HomeFlavour::Stable
        );
        assert_eq!(
            HomeFlavour::of(Path::new("/tmp/scratch")),
            HomeFlavour::Server
        );
    }

    #[test]
    fn every_channel_home_config_is_accepted_by_the_runtime() {
        // This is the check `--home` used to fail: `preview_default()` returns
        // the environment home, and overwriting `home_dir` afterwards trips the
        // isolation rule.
        for home in [
            "/tmp/vibex-probe/desktop-preview",
            "/tmp/vibex-probe/desktop-rc",
            "/tmp/vibex-probe/desktop-stable",
        ] {
            let config = home_config(Path::new(home)).expect("the config resolves");
            assert_eq!(config.home_dir, Path::new(home));
            config
                .validate()
                .unwrap_or_else(|error| panic!("{home} was rejected: {error}"));
        }
    }

    #[test]
    fn a_non_channel_home_gets_a_server_config() {
        let config = home_config(Path::new("/tmp/vibex-scratch")).expect("the config resolves");
        assert_eq!(config.home_dir, Path::new("/tmp/vibex-scratch"));
        assert_eq!(
            config.database_path,
            Path::new("/tmp/vibex-scratch/vibex.db")
        );
        config.validate().expect("a server home validates");
    }

    #[test]
    fn a_probe_never_starts_a_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("desktop-preview");
        let probe = probe(&home);
        assert_eq!(probe.seat, SeatKind::Authority);
        assert_eq!(probe.flavour, HomeFlavour::Preview);
        assert!(probe.detail.contains("would start one"), "{}", probe.detail);
        // Probing must not leave a lock behind: the real run has to be able to
        // take it.
        let lock = DesktopHomeLock::acquire(&home, application_id(HomeFlavour::Preview))
            .expect("the probe released the lock");
        drop(lock);
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
            endpoints: local_endpoint_list(HomeFlavour::Preview),
            detail: "http://127.0.0.1:1428: connection refused".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("Settings → General"));
        assert!(text.contains("turn on Vibex TUI"));
        assert!(text.contains("quit the desktop app"));
        assert!(text.contains("vibex connect"));
        // The doors that were actually tried, so the report is not a guess.
        assert!(text.contains(DESKTOP_LOCAL_CLIENT_LOOPBACK), "{text}");
        assert!(text.contains(DESKTOP_DIRECT_LOOPBACK), "{text}");
        assert!(text.contains("connection refused"), "{text}");
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
        assert!(DESKTOP_LOCAL_CLIENT_LOOPBACK.contains("127.0.0.1"));
        // Same-machine clients reach the runtime over TLS, never in the clear,
        // so the pin derived from the identity file is always in play.
        assert!(DESKTOP_LOCAL_CLIENT_LOOPBACK.starts_with("https://"));
    }

    #[test]
    fn the_pinned_local_listener_is_tried_before_the_plaintext_one() {
        // The pinned listener is the one the desktop maintains for this machine
        // and it is the only one that authenticates the runtime. Direct is the
        // fallback for a desktop that predates it, so it must not shadow it.
        let candidates = local_candidates(HomeFlavour::Preview);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].endpoint, DESKTOP_LOCAL_CLIENT_LOOPBACK);
        assert!(candidates[0].pinned);
        assert_eq!(candidates[1].endpoint, DESKTOP_DIRECT_LOOPBACK);
        assert!(!candidates[1].pinned);
    }

    #[test]
    fn a_plain_http_route_is_never_pinned() {
        // `RemoteClientConfig::validate` rejects a pin on a plain-HTTP URL, and
        // a runtime in the loopback policy serves exactly that.
        let mut remote = RemoteClientConfig::new("http://127.0.0.1:8791", auth_proof());
        remote.allow_insecure_local_dev = false;
        remote.pinned_tls_certificate_der = None;
        assert!(
            remote.validate().is_err(),
            "plain loopback HTTP must need an explicit opt-in"
        );
        remote.allow_insecure_local_dev = true;
        assert!(
            remote.validate().is_ok(),
            "the loopback development exception must be usable"
        );
    }

    fn auth_proof() -> vibex_core::RemoteAuthProof {
        vibex_core::RemoteAuthProof {
            device_id: vibex_core::DeviceId::new(),
            auth_token: "test-token".to_string(),
        }
    }

    #[test]
    fn each_flavour_dials_its_own_listener() {
        // The desktop's own listeners and the headless server's listen in
        // different places; dialling the wrong one reads as "not accepting
        // local clients".
        assert_eq!(
            loopback_endpoint(HomeFlavour::Preview),
            DESKTOP_LOCAL_CLIENT_LOOPBACK
        );
        assert_eq!(
            loopback_endpoint(HomeFlavour::ReleaseCandidate),
            DESKTOP_LOCAL_CLIENT_LOOPBACK
        );
        assert_eq!(
            loopback_endpoint(HomeFlavour::Stable),
            DESKTOP_LOCAL_CLIENT_LOOPBACK
        );
        assert_eq!(loopback_endpoint(HomeFlavour::Server), SERVER_LOOPBACK);
    }

    #[test]
    fn a_server_home_has_exactly_one_local_listener() {
        // A headless runtime moves with `VIBEX_BIND_ADDR` and serves plain HTTP
        // behind it; there is no second, pinned door to try.
        let candidates = local_candidates(HomeFlavour::Server);
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].pinned);
    }

    /// A remote seat has to be attached before the interface gets it.
    ///
    /// A facade nobody connected answers every read with
    /// `remote_reselection_canceled`, which the interface renders as an empty
    /// runtime catalogue ("Agent setup: unavailable") and an empty session list
    /// rather than as a connection problem: the client looks like it works and
    /// shows nothing. This walks the whole path — pin, claim, build, attach —
    /// against a real pinned listener.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remote_seat_arrives_connected() {
        use vibex_remote::{
            RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteTrustService,
        };

        let directory = tempfile::tempdir().expect("temp home");
        let home = directory.path();
        let db_path = home.join("vibex.db");
        let dispatcher =
            RemoteDispatcher::new(vibex_remote::RemoteServiceConfig::loopback_disabled());
        let gateway = RemoteGateway::new(
            RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
            dispatcher,
            &db_path,
            home.join("relay/desktop-identity.json"),
        );
        let listener = gateway
            .start_local_client_gateway(0)
            .await
            .expect("the loopback listener starts");
        let endpoint = format!("https://127.0.0.1:{}", listener.bound_addr.port());

        let mut connection = vibex_db::open_database(&db_path).expect("open the database");
        vibex_db::apply_migrations(&mut connection).expect("migrate");
        let response = RemoteTrustService::create_pairing_code(
            &connection,
            RemoteCreatePairingCodeRequest {
                permission_level: RemoteDevicePermissionLevel::FullControl,
                ttl_ms: Some(120_000),
            },
        )
        .expect("mint a pairing code");
        let bundle = claim_pairing_code_with_identity_and_certificate(
            endpoint,
            response.pairing_code.clone(),
            "Vibex TUI",
            false,
            Some(listener.tls_certificate_base64.clone()),
            None,
        )
        .await
        .expect("claim over the pinned route");

        let seat = Seat::from_bundle(&bundle, Some(listener.tls_certificate_base64.clone()), home)
            .await
            .expect("a reachable runtime produces a seat");

        assert_eq!(seat.kind, SeatKind::Remote);
        assert!(
            !seat.facade.capabilities().schema_version.is_empty(),
            "the seat must arrive attached; an unattached facade reads as an \
             unavailable runtime rather than as a connection problem"
        );
        gateway.stop().await.expect("stop the Gateway");
    }

    /// Taking the home is offered only when nothing else owns it.
    ///
    /// A stale credential must not turn a client into a second authority: the
    /// lock is the answer, and `None` is how the caller learns to look for the
    /// runtime's new door instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn taking_the_home_defers_to_an_existing_owner() {
        let directory = tempfile::tempdir().expect("temp home");
        let home = directory.path().join("desktop-preview");
        std::fs::create_dir_all(&home).expect("create the home");
        let config = home_config(&home).expect("the config resolves");

        let lock = DesktopHomeLock::acquire(&home, &config.application_id).expect("take the home");
        let taken = Seat::take_authority(&home, &config)
            .await
            .expect("probing the lock is not an error");
        assert!(
            taken.is_none(),
            "a home another process owns must never be taken over"
        );
        drop(lock);
    }

    /// An unreachable endpoint is a seat error, not a seat that fails later.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreachable_remote_seat_reports_the_endpoint() {
        // Nothing listens here, so the attach has to fail loudly instead of
        // handing the interface a facade whose every read fails far from the
        // cause.
        let mut remote = RemoteClientConfig::new("http://127.0.0.1:1", auth_proof());
        remote.allow_insecure_local_dev = true;
        remote.client_type = RemoteClientType::Native;
        let error = match Seat::from_remote_config(
            remote,
            None,
            vibex_core::RemoteServerKind::Unknown,
            Path::new("/data/vibex"),
        )
        .await
        {
            Ok(_) => panic!("an unreachable runtime cannot produce a seat"),
            Err(error) => error,
        };
        let text = error.to_string();
        assert!(matches!(error, SeatError::Unreachable { .. }), "{text}");
        assert!(text.contains("127.0.0.1:1"), "{text}");
    }
}

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::signal;
use vibex_core::{
    RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel, RemotePairingCodeLink, VibexError,
    unix_timestamp_ms,
};
use vibex_db::{apply_migrations, open_database};
use vibex_desktop_runtime::{
    DesktopHomeLock, DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade,
};
use vibex_remote::{RemoteIdentity, RemoteIdentityStore, RemoteTrustService};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SERVER_STATE_FILE: &str = "server-state.json";
const PAIRING_SCHEMA_VERSION: &str = "vibex-server-pairing.v1";
const STATUS_SCHEMA_VERSION: &str = "vibex-server-status.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerState {
    pid: u32,
    port: u16,
    version: String,
    started_at_ms: i64,
}

/// Owns `<home>/server-state.json` for the lifetime of one `serve` process.
/// A crash leaves the file behind on purpose: `status` can then report a stale
/// pid instead of pretending nothing happened, and `stop` can refuse to signal
/// a pid it cannot verify.
struct ServerStateGuard {
    path: PathBuf,
    pid: u32,
}

impl ServerStateGuard {
    fn install(config: &DesktopRuntimeConfig, port: u16) -> Result<Self, VibexError> {
        let pid = std::process::id();
        let state = ServerState {
            pid,
            port,
            version: VERSION.to_string(),
            started_at_ms: unix_timestamp_ms(),
        };
        fs::create_dir_all(&config.home_dir).map_err(|_| {
            VibexError::storage(
                "server_state_directory_failed",
                "the runtime state directory could not be created",
            )
        })?;
        let path = server_state_path(config);
        let temporary = path.with_extension("json.tmp");
        let encoded = serde_json::to_vec(&state).map_err(|_| {
            VibexError::storage(
                "server_state_encode_failed",
                "the runtime state could not be encoded",
            )
        })?;
        fs::write(&temporary, encoded).map_err(|_| {
            VibexError::storage(
                "server_state_write_failed",
                "the runtime state could not be written",
            )
        })?;
        #[cfg(windows)]
        let _ = fs::remove_file(&path);
        fs::rename(&temporary, &path).map_err(|_| {
            VibexError::storage(
                "server_state_commit_failed",
                "the runtime state could not be committed",
            )
        })?;
        Ok(Self { path, pid })
    }
}

impl Drop for ServerStateGuard {
    fn drop(&mut self) {
        let owned = fs::read(&self.path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<ServerState>(&raw).ok())
            .is_some_and(|state| state.pid == self.pid);
        if owned {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusJson {
    schema_version: &'static str,
    running: bool,
    pid: Option<u32>,
    port: u16,
    version: &'static str,
    home: String,
    health: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingJson {
    schema_version: &'static str,
    server_id: String,
    server_version: &'static str,
    pairing_code: String,
    pairing_link: String,
    tls_certificate_der: Option<String>,
    endpoint: String,
    expires_at_ms: i64,
}

impl fmt::Debug for PairingJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The code and the link both carry the one-time secret. `--json` writes
        // them to stdout because that is the command's contract, but a debug
        // log must never reproduce them.
        formatter
            .debug_struct("PairingJson")
            .field("schema_version", &self.schema_version)
            .field("server_id", &self.server_id)
            .field("server_version", &self.server_version)
            .field("pairing_code", &"<redacted>")
            .field("pairing_link", &"<redacted>")
            .field(
                "tls_certificate_der",
                &self.tls_certificate_der.as_ref().map(|_| "<redacted>"),
            )
            .field("endpoint", &self.endpoint)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    // The delegation broker spawns this same executable once per session that
    // uses sub-agents, so the headless runtime serves delegation from the
    // binary that already owns the agent manager.
    if arguments.len() == 1 && arguments[0] == "--agent-delegation-mcp" {
        if let Err(error) = vibex_agent::run_delegation_mcp_stdio() {
            eprintln!("Agent delegation MCP sidecar failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The browser MCP stdio sidecar is a stateless bridge to the runtime's own
    // loopback endpoint, so the headless server serves it from the same binary
    // that hosts the endpoint.
    if arguments.len() == 1 && arguments[0] == "--browser-mcp" {
        if let Err(error) = vibex_browser::stdio::run_browser_mcp_stdio() {
            eprintln!("Browser MCP sidecar failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The computer-use helper is spawned by the runtime that owns the desktop
    // session, and a headless deployment on a host with a desktop session is a
    // supported shape. The same binary therefore serves the helper, the stdio
    // bridge and the command line.
    if arguments.len() == 1 && arguments[0] == "--computer-helper" {
        if let Err(error) = vibex_computer::helper::run_helper_from_environment() {
            eprintln!("Computer-use helper failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if arguments.len() == 1 && arguments[0] == "--computer-mcp" {
        if let Err(error) = vibex_computer::stdio::run_computer_mcp_stdio() {
            eprintln!("Computer MCP sidecar failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "computer")
    {
        match vibex_computer::cli::run(&arguments[1..]) {
            Ok(output) => {
                println!("{output}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("{error}");
                if let Some(hint) = &error.recovery_hint {
                    eprintln!("{hint}");
                }
                std::process::exit(2);
            }
        }
    }
    let command = Command::parse(arguments)?;
    match command {
        Command::Help => print_help(),
        Command::Version => println!("vibex-server {VERSION}"),
        Command::ConfigCheck => {
            let config = headless_config()?;
            config_check(&config)?;
            println!("configuration=valid");
            print_config_summary(&config);
        }
        Command::PairingCode {
            permission,
            ttl_ms,
            json,
        } => {
            let config = headless_config()?;
            config_check(&config)?;
            let response = create_pairing_code(&config, permission, ttl_ms)?;
            let certificate = pinned_certificate_for_config(&config)?;
            if json {
                print_pairing_json(&config, &response, certificate)?;
            } else {
                print_pairing_entry(&config, None, &response, certificate);
                println!("warning=one-time code; it is not stored in plaintext");
            }
        }
        Command::Serve { pairing } => serve(pairing).await?,
        Command::Tui { local } => tui(local).await?,
        Command::Status { json } => status(json)?,
        Command::Stop => stop()?,
        Command::Revoke { device_id, reason } => revoke(&device_id, reason.as_deref())?,
    }
    Ok(())
}

#[derive(Debug)]
enum Command {
    Help,
    Version,
    Serve {
        pairing: bool,
    },
    /// Run the character-grid client against this deployment.
    Tui {
        /// Start (or reuse) the in-process runtime instead of attaching to an
        /// already running daemon.
        local: bool,
    },
    Status {
        json: bool,
    },
    Stop,
    PairingCode {
        permission: RemoteDevicePermissionLevel,
        ttl_ms: Option<u32>,
        json: bool,
    },
    Revoke {
        device_id: String,
        reason: Option<String>,
    },
    ConfigCheck,
}

impl Command {
    fn parse(args: Vec<String>) -> Result<Self, VibexError> {
        if args.is_empty() {
            return Ok(Self::Serve { pairing: true });
        }
        if args.iter().any(|arg| arg == "--help" || arg == "-h") {
            return Ok(Self::Help);
        }
        if args.iter().any(|arg| arg == "--version" || arg == "-V") {
            return Ok(Self::Version);
        }
        let command = args[0].as_str();
        match command {
            "serve" => Ok(Self::Serve {
                pairing: !args.iter().any(|arg| arg == "--no-pairing"),
            }),
            "status" => Ok(Self::Status {
                json: args.iter().any(|arg| arg == "--json"),
            }),
            "stop" => Ok(Self::Stop),
            "tui" => Ok(Self::Tui {
                local: args.iter().any(|arg| arg == "--local"),
            }),
            "config-check" => Ok(Self::ConfigCheck),
            "pairing-code" => {
                let permission = option_value(&args, "--permission")
                    .map(|value| parse_permission(&value))
                    .transpose()?
                    .unwrap_or(RemoteDevicePermissionLevel::FullControl);
                let ttl_ms = option_value(&args, "--ttl-ms")
                    .map(|value| {
                        value.parse().map_err(|_| {
                            VibexError::validation(
                                "server_ttl_invalid",
                                "--ttl-ms must be an unsigned integer",
                            )
                        })
                    })
                    .transpose()?;
                Ok(Self::PairingCode {
                    permission,
                    ttl_ms,
                    json: args.iter().any(|arg| arg == "--json"),
                })
            }
            "revoke" => {
                let device_id = args.get(1).cloned().ok_or_else(|| {
                    VibexError::validation(
                        "server_device_id_missing",
                        "revoke requires a device id",
                    )
                })?;
                Ok(Self::Revoke {
                    device_id,
                    reason: option_value(&args, "--reason"),
                })
            }
            _ => Err(VibexError::validation(
                "server_command_unknown",
                "unknown vibex-server command; use --help",
            )),
        }
    }
}

async fn serve(print_pairing: bool) -> Result<(), Box<dyn Error>> {
    let config = headless_config()?;
    config_check(&config)?;
    let runtime = DesktopRuntime::start(config.clone()).await?;
    let status = runtime.remote().gateway().status();
    let port = status
        .bound_addr
        .map(|address| address.port())
        .unwrap_or_else(|| configured_port(&config));
    let _state = ServerStateGuard::install(&config, port)?;
    let identity = runtime.remote().gateway().identity()?;
    println!("server_id={}", identity.server_id());
    println!("pid={}", std::process::id());
    println!(
        "server_identity_public_key={}",
        identity.public_key_base64()
    );
    print_bound_endpoint(&config, status.bound_addr);
    if print_pairing {
        match create_pairing_code(&config, RemoteDevicePermissionLevel::FullControl, None) {
            Ok(response) => {
                let certificate = runtime.remote().gateway().pinned_tls_certificate_base64()?;
                print_pairing_entry(&config, status.bound_addr, &response, certificate);
            }
            Err(error) => eprintln!("pairing_code=unavailable error_code={}", error.code),
        }
    }
    println!("runtime=ready");
    wait_for_shutdown_signal().await?;
    runtime.shutdown().await?;
    println!("runtime=stopped");
    Ok(())
}

/// Run the character-grid client.
///
/// The home lock permits exactly one runtime per home, so the shape is decided
/// by whether a daemon already owns it:
///
/// * no daemon — start one in this process and serve the interface from the
///   in-process authority seat;
/// * daemon already running — attach to it over loopback with a locally
///   bootstrapped device credential.
///
/// `--local` forces the first shape and fails loudly when the home is taken,
/// which is what a container operator scripting `docker exec` wants.
async fn tui(local: bool) -> Result<(), Box<dyn Error>> {
    let config = headless_config()?;
    config_check(&config)?;
    let request = vibex_client::seat::SeatRequest {
        connect: None,
        home: Some(config.home_dir.clone()),
        prefer_authority: local,
        // The headless `tui` entry point is not the interactive client that
        // watches for a handover offer, so it never takes a home over.
        take_over: false,
    };
    let seat = vibex_client::seat::Seat::resolve(request).await?;
    let options = vibex_tui::TuiOptions {
        seat: seat.kind,
        runtime_kind: seat.runtime_kind,
        ..vibex_tui::TuiOptions::default()
    };
    let result = vibex_tui::run(seat.facade.clone(), options);
    seat.shutdown().await;
    match result {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{}: {}", error.code, error.message).into()),
    }
}

async fn wait_for_shutdown_signal() -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = signal::ctrl_c() => result?,
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    signal::ctrl_c().await?;
    Ok(())
}

fn config_check(config: &DesktopRuntimeConfig) -> Result<(), VibexError> {
    if !config.home_dir.is_absolute() {
        return Err(VibexError::validation(
            "server_home_must_be_absolute",
            "VIBEX_HOME or its derived runtime home must be an absolute path",
        ));
    }
    config.validate()
}

fn create_pairing_code(
    config: &DesktopRuntimeConfig,
    permission: RemoteDevicePermissionLevel,
    ttl_ms: Option<u32>,
) -> Result<vibex_core::RemoteCreatePairingCodeResponse, VibexError> {
    std::fs::create_dir_all(&config.home_dir).map_err(|_| {
        VibexError::storage(
            "server_home_create_failed",
            "headless runtime home could not be created",
        )
    })?;
    let mut connection = open_database(&config.database_path)?;
    apply_migrations(&mut connection)?;
    RemoteTrustService::create_pairing_code(
        &connection,
        RemoteCreatePairingCodeRequest {
            permission_level: permission,
            ttl_ms,
        },
    )
}

/// Prints the one-time code together with everything a client needs to reach
/// this runtime: the certificate fingerprint, the copyable `vibex://` pairing
/// link, and a scannable rendering of that link.
///
/// The link is the only channel that carries a self-signed certificate, and it
/// goes from this console to the operator's screen. A client that pairs from it
/// pins the certificate before its first request, so nothing about the trust
/// decision depends on the network.
fn print_pairing_entry(
    config: &DesktopRuntimeConfig,
    bound: Option<SocketAddr>,
    response: &vibex_core::RemoteCreatePairingCodeResponse,
    certificate: Option<String>,
) {
    println!("pairing_code={}", response.pairing_code);
    println!("pairing_expires_at_ms={}", response.pairing.expires_at_ms);
    println!("permission={:?}", response.pairing.permission_level);
    let link = match pairing_link(config, bound, &response.pairing_code, certificate) {
        Ok(link) => link,
        Err(error) => {
            println!("pairing_link=unavailable");
            eprintln!("pairing_link_unavailable={}", error.message);
            return;
        }
    };
    if let Ok(Some(fingerprint)) = link.tls_fingerprint() {
        println!("tls_fingerprint={fingerprint}");
    }
    match link.encode() {
        Ok(encoded) => {
            println!("pairing_link={encoded}");
            println!("pairing_hint=desktop: paste the pairing link; mobile: scan the QR below");
            // Reed-Solomon level L keeps the symbol as small as possible: the
            // code is scanned off a clean screen, not off damaged paper.
            match qrcode::QrCode::with_error_correction_level(
                encoded.as_bytes(),
                qrcode::EcLevel::L,
            ) {
                Ok(code) => {
                    let rendered = code.render::<qrcode::render::unicode::Dense1x2>().build();
                    print!("{rendered}");
                    if !rendered.ends_with('\n') {
                        println!();
                    }
                }
                Err(_) => eprintln!("pairing_qr=unavailable"),
            }
        }
        Err(error) => eprintln!("pairing_link=unavailable error_code={}", error.code),
    }
}

fn print_pairing_json(
    config: &DesktopRuntimeConfig,
    response: &vibex_core::RemoteCreatePairingCodeResponse,
    certificate: Option<String>,
) -> Result<(), VibexError> {
    let link = pairing_link(config, None, &response.pairing_code, certificate)?;
    let encoded_link = link.encode()?;
    let identity = load_remote_identity(config)?;
    let payload = PairingJson {
        schema_version: PAIRING_SCHEMA_VERSION,
        server_id: identity.server_id().to_string(),
        server_version: VERSION,
        pairing_code: response.pairing_code.clone(),
        pairing_link: encoded_link,
        tls_certificate_der: link.tls_certificate_der.clone(),
        endpoint: link.server_url.clone(),
        expires_at_ms: response.pairing.expires_at_ms,
    };
    println!(
        "{}",
        serde_json::to_string(&payload).map_err(|_| {
            VibexError::storage(
                "server_pairing_encode_failed",
                "the pairing entry could not be encoded",
            )
        })?
    );
    Ok(())
}

fn pairing_link(
    config: &DesktopRuntimeConfig,
    bound: Option<SocketAddr>,
    pairing_code: &str,
    certificate: Option<String>,
) -> Result<RemotePairingCodeLink, VibexError> {
    let endpoint = advertised_endpoint(config, bound).ok_or_else(|| {
        VibexError::validation(
            "server_pairing_link_endpoint_unavailable",
            "no address is known for the pairing link; set VIBEX_PUBLIC_HOST to the address clients should use",
        )
    })?;
    RemotePairingCodeLink::new(endpoint, pairing_code, certificate)
}

/// Address clients should use, which is not always the bind address: a
/// wildcard bind (`0.0.0.0`) is not dialable, so it falls back to the default
/// route interface.
fn advertised_endpoint(config: &DesktopRuntimeConfig, bound: Option<SocketAddr>) -> Option<String> {
    let scheme = if config.remote_gateway.tls_policy.requires_https() {
        "https"
    } else {
        "http"
    };
    if let Some(candidate) = config
        .remote_gateway
        .pairing_routes
        .direct_candidates
        .first()
    {
        return Some(with_bound_port(&candidate.url, bound));
    }
    let address = bound.or_else(|| config.remote_gateway.service.bind_addr.parse().ok())?;
    let ip = if address.ip().is_unspecified() {
        default_route_ip()?
    } else {
        address.ip()
    };
    let host = match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    Some(format!("{scheme}://{host}:{}", address.port()))
}

/// `VIBEX_PUBLIC_HOST` may omit the port, in which case the listener port is
/// the only sensible default. An explicit port, `443`, and a hostname that is
/// fronted by a proxy on the default port are all left untouched.
fn with_bound_port(url: &str, bound: Option<SocketAddr>) -> String {
    let url = url.trim_end_matches('/');
    let Some(port) = bound.map(|address| address.port()) else {
        return url.to_string();
    };
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let has_explicit_port = match rest.strip_prefix('[') {
        Some(rest) => rest.contains("]:"),
        None => rest.contains(':'),
    };
    if has_explicit_port || port == 443 {
        return url.to_string();
    }
    format!("{scheme}://{rest}:{port}")
}

/// Source address of the default route. A UDP `connect` performs no handshake
/// and sends no packet; it only asks the kernel which address it would use.
fn default_route_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket
        .local_addr()
        .ok()
        .map(|address| address.ip())
        .filter(|ip| !ip.is_unspecified())
}

/// Certificate the runtime terminates TLS with when it serves its own pinned
/// certificate. Loading the identity here keeps `pairing-code` working without
/// a running runtime.
fn pinned_certificate_for_config(
    config: &DesktopRuntimeConfig,
) -> Result<Option<String>, VibexError> {
    if config.remote_gateway.tls_policy != vibex_remote::RemoteGatewayTlsPolicy::PinnedCertificate {
        return Ok(None);
    }
    std::fs::create_dir_all(&config.home_dir).map_err(|_| {
        VibexError::storage(
            "server_home_create_failed",
            "headless runtime home could not be created",
        )
    })?;
    let identity = load_remote_identity(config)?;
    Ok(Some(vibex_remote::pinned_tls_certificate_base64(
        &identity,
    )?))
}

fn load_remote_identity(config: &DesktopRuntimeConfig) -> Result<RemoteIdentity, VibexError> {
    let path = config.home_dir.join("relay/desktop-identity.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| {
            VibexError::storage(
                "server_identity_directory_failed",
                "headless runtime identity directory could not be created",
            )
        })?;
    }
    RemoteIdentityStore::new(path).load_or_create()
}

fn status(json: bool) -> Result<(), VibexError> {
    let config = headless_config()?;
    config_check(&config)?;
    fs::create_dir_all(&config.home_dir).map_err(|_| {
        VibexError::storage(
            "server_state_directory_failed",
            "the runtime state directory could not be created",
        )
    })?;
    let state = read_server_state(&config)?;
    let state_alive = state
        .as_ref()
        .is_some_and(|state| process_matches(state.pid));
    let lock_held = home_lock_held(&config)?;
    let running = state_alive || lock_held;
    let pid = if state_alive {
        state.as_ref().map(|state| state.pid)
    } else {
        None
    };
    let port = if state_alive {
        state.as_ref().map(|state| state.port)
    } else {
        None
    }
    .unwrap_or_else(|| configured_port(&config));
    let health = if running {
        probe_health(&config, port)
    } else {
        "stopped"
    };

    if json {
        let payload = StatusJson {
            schema_version: STATUS_SCHEMA_VERSION,
            running,
            pid,
            port,
            version: VERSION,
            home: config.home_dir.to_string_lossy().into_owned(),
            health,
        };
        println!(
            "{}",
            serde_json::to_string(&payload).map_err(|_| {
                VibexError::storage(
                    "server_status_encode_failed",
                    "the runtime status could not be encoded",
                )
            })?
        );
    } else {
        println!("running={running}");
        if let Some(pid) = pid {
            println!("pid={pid}");
        }
        println!("port={port}");
        println!("version={VERSION}");
        println!("home={}", config.home_dir.display());
        println!("health={health}");
        println!(
            "configured_bind={}",
            config.remote_gateway.service.bind_addr
        );
    }
    Ok(())
}

fn stop() -> Result<(), VibexError> {
    let config = headless_config()?;
    config_check(&config)?;
    fs::create_dir_all(&config.home_dir).map_err(|_| {
        VibexError::storage(
            "server_state_directory_failed",
            "the runtime state directory could not be created",
        )
    })?;
    let path = server_state_path(&config);
    let Some(state) = read_server_state(&config)? else {
        if home_lock_held(&config)? {
            return Err(VibexError::conflict(
                "server_pid_missing",
                "the runtime is running but has no state file; stop it from the console that started it",
            ));
        }
        println!("already_stopped=true");
        return Ok(());
    };
    let lock_held = home_lock_held(&config)?;
    if config.acquire_home_lock && !lock_held {
        // Nothing owns this home any more. The state is stale even if the pid
        // still exists: never signal a stranger because of a recycled pid.
        let _ = fs::remove_file(&path);
        println!("already_stopped=true");
        return Ok(());
    }
    if !process_matches(state.pid) {
        if lock_held {
            return Err(VibexError::conflict(
                "server_pid_missing",
                "the runtime owns this home but its state file does not name a matching process",
            ));
        }
        // A crash leaves the state behind; the pid may now belong to something
        // else. Never signal a pid that is not this runtime.
        let _ = fs::remove_file(&path);
        println!("already_stopped=true");
        return Ok(());
    }

    terminate_process(state.pid)?;
    wait_for_exit(state.pid, Duration::from_secs(10));
    if process_matches(state.pid) {
        kill_process(state.pid)?;
        wait_for_exit(state.pid, Duration::from_secs(2));
    }
    if process_matches(state.pid) {
        return Err(VibexError::storage(
            "server_stop_failed",
            "the runtime did not exit after SIGTERM and SIGKILL",
        ));
    }
    let _ = fs::remove_file(&path);
    println!("stopped=true");
    println!("pid={}", state.pid);
    Ok(())
}

fn wait_for_exit(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while process_matches(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> Result<(), VibexError> {
    signal_process(pid, libc::SIGTERM)
}

#[cfg(unix)]
fn kill_process(pid: u32) -> Result<(), VibexError> {
    signal_process(pid, libc::SIGKILL)
}

#[cfg(unix)]
fn signal_process(pid: u32, signal: i32) -> Result<(), VibexError> {
    let result = unsafe { libc::kill(pid as i32, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(VibexError::storage(
            "server_signal_failed",
            "the runtime process could not be signalled",
        ))
    }
}

#[cfg(not(unix))]
fn terminate_process(_pid: u32) -> Result<(), VibexError> {
    Err(VibexError::capability(
        "server_stop_unsupported",
        "stopping the runtime is only supported on Unix hosts",
    ))
}

#[cfg(not(unix))]
fn kill_process(_pid: u32) -> Result<(), VibexError> {
    Err(VibexError::capability(
        "server_stop_unsupported",
        "stopping the runtime is only supported on Unix hosts",
    ))
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    pid > 0 && unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(target_os = "linux")]
fn process_matches(pid: u32) -> bool {
    if !process_is_alive(pid) {
        return false;
    }
    let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let arguments = raw
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    command_is_serve(&arguments)
}

/// A bare `vibex-server` starts `serve` by default; every other subcommand is
/// a client, a sidecar, or a one-shot command that must never be signalled.
#[cfg(target_os = "linux")]
fn command_is_serve(arguments: &[&[u8]]) -> bool {
    let Some(executable) = arguments.first() else {
        return false;
    };
    if !executable.ends_with(b"vibex-server") {
        return false;
    }
    arguments
        .get(1)
        .is_none_or(|command| *command == b"serve".as_slice())
}

/// macOS has no `/proc`; `ps` is the portable way to read the command line
/// back before signalling a pid.
#[cfg(all(unix, not(target_os = "linux")))]
fn process_matches(pid: u32) -> bool {
    use std::process::Command;
    if !process_is_alive(pid) {
        return false;
    }
    let Ok(output) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let command = String::from_utf8_lossy(&output.stdout);
    let mut arguments = command.split_whitespace();
    let Some(executable) = arguments.next() else {
        return false;
    };
    if !executable.ends_with("vibex-server") {
        return false;
    }
    arguments.next().is_none_or(|argument| argument == "serve")
}

#[cfg(not(unix))]
fn process_matches(_pid: u32) -> bool {
    false
}

fn server_state_path(config: &DesktopRuntimeConfig) -> PathBuf {
    config.home_dir.join(SERVER_STATE_FILE)
}

fn read_server_state(config: &DesktopRuntimeConfig) -> Result<Option<ServerState>, VibexError> {
    let path = server_state_path(config);
    match fs::read(&path) {
        Ok(raw) => match serde_json::from_slice(&raw) {
            Ok(state) => Ok(Some(state)),
            Err(_) => {
                // A truncated state file must not wedge the CLI; the next
                // `serve` recreates it.
                let _ = fs::remove_file(&path);
                Ok(None)
            }
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(VibexError::storage(
            "server_state_read_failed",
            "the runtime state file could not be read",
        )),
    }
}

fn home_lock_held(config: &DesktopRuntimeConfig) -> Result<bool, VibexError> {
    if !config.acquire_home_lock {
        return Ok(false);
    }
    match DesktopHomeLock::acquire(&config.home_dir, &config.application_id) {
        Ok(lock) => {
            drop(lock);
            Ok(false)
        }
        Err(error) if error.code == "desktop_runtime_home_locked" => Ok(true),
        Err(error) => Err(error),
    }
}

fn configured_port(config: &DesktopRuntimeConfig) -> u16 {
    config
        .remote_gateway
        .service
        .bind_addr
        .parse::<SocketAddr>()
        .map(|address| address.port())
        .unwrap_or(0)
}

fn probe_health(config: &DesktopRuntimeConfig, port: u16) -> &'static str {
    if port == 0 {
        return "unreachable";
    }
    let bind_ip = config
        .remote_gateway
        .service
        .bind_addr
        .parse::<SocketAddr>()
        .ok()
        .map(|address| address.ip());
    let ip = match bind_ip {
        Some(IpAddr::V4(ip)) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(IpAddr::V6(ip)) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        Some(ip) => ip,
        None => IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    match TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(500)) {
        Ok(_) => "ok",
        Err(_) => "unreachable",
    }
}

fn revoke(device_id: &str, reason: Option<&str>) -> Result<(), VibexError> {
    let config = DesktopRuntimeConfig::headless_from_environment()?;
    let mut connection = open_database(&config.database_path)?;
    apply_migrations(&mut connection)?;
    let device_id = vibex_core::DeviceId::parse(device_id.to_string())?;
    let device = RemoteTrustService::revoke_device(
        &connection,
        vibex_core::RemoteRevokeDeviceRequest {
            device_id,
            reason: reason.map(str::to_string),
        },
    )?;
    println!("revoked_device={}", device.device_id);
    Ok(())
}

fn print_config_summary(config: &DesktopRuntimeConfig) {
    println!("home={}", config.home_dir.display());
    println!("database={}", config.database_path.display());
    println!("bind={}", config.remote_gateway.service.bind_addr);
    println!(
        "deployment={}",
        config.remote_gateway.deployment_mode.wire_name()
    );
    println!("tls={}", config.remote_gateway.tls_policy.wire_name());
}

fn print_bound_endpoint(config: &DesktopRuntimeConfig, bound: Option<SocketAddr>) {
    let scheme = if config.remote_gateway.tls_policy.requires_https() {
        "https"
    } else {
        "http"
    };
    println!(
        "endpoint={scheme}://{}",
        bound.map(|addr| addr.to_string()).unwrap_or_else(|| config
            .remote_gateway
            .service
            .bind_addr
            .clone())
    );
}

fn option_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

/// Headless configuration with the delegation sidecar defaulted to this
/// binary, which serves `--agent-delegation-mcp` itself. Deployments can still
/// override the executable through `VIBEX_DELEGATION_SIDECAR_COMMAND`.
fn headless_config() -> Result<DesktopRuntimeConfig, VibexError> {
    let mut config = DesktopRuntimeConfig::headless_from_environment()?;
    if config.delegation_sidecar_command.is_none() {
        config.delegation_sidecar_command = std::env::current_exe().ok();
    }
    Ok(config)
}

fn parse_permission(value: &str) -> Result<RemoteDevicePermissionLevel, VibexError> {
    match value {
        "read-only" | "readonly" => Ok(RemoteDevicePermissionLevel::ReadOnly),
        "approve-only" | "approve" => Ok(RemoteDevicePermissionLevel::ApproveOnly),
        "full-control" | "full" => Ok(RemoteDevicePermissionLevel::FullControl),
        _ => Err(VibexError::validation(
            "server_permission_invalid",
            "permission must be read-only, approve-only, or full-control",
        )),
    }
}

fn print_help() {
    println!("vibex-server {VERSION}");
    println!("Usage: vibex-server [serve|status|stop|pairing-code|revoke|config-check|tui]");
    println!("  --agent-delegation-mcp                run the Agent delegation MCP stdio sidecar");
    println!("  --browser-mcp                         run the embedded browser MCP stdio sidecar");
    println!(
        "  --computer-helper                     run the computer-use engine helper (spawned by the runtime)"
    );
    println!("  --computer-mcp                        run the computer-use MCP stdio sidecar");
    println!("  computer <command>                    drive the desktop of a running runtime");
    println!("  serve [--no-pairing]                 run the authoritative headless runtime");
    println!("  pairing-code [--permission full-control] [--ttl-ms N] [--json]");
    println!("                                       mint a one-time code plus its pairing link");
    println!("  status [--json]                      report runtime state and loopback health");
    println!("  stop                                 stop the runtime owned by this home");
    println!("  revoke DEVICE_ID [--reason TEXT]     revoke a paired device");
    println!("  config-check                          validate VIBEX_* deployment settings");
    println!("  tui [--local]                        run the terminal client; attaches to a");
    println!(
        "                                        running daemon, or hosts the runtime with --local"
    );
    println!("Environment: VIBEX_HOME, VIBEX_DB_PATH, VIBEX_BIND_ADDR, VIBEX_DEPLOYMENT_MODE,");
    println!("  VIBEX_PUBLIC_HOST, VIBEX_ALLOWED_HOSTS, VIBEX_ALLOWED_ORIGINS, VIBEX_TLS_MODE,");
    println!("  VIBEX_TLS_CERT_FILE, VIBEX_TLS_KEY_FILE and VIBEX_*_LIMIT settings.");
    println!("VIBEX_TLS_MODE=pinned_certificate serves a self-signed certificate derived from the");
    println!("  runtime identity, so no CA is needed on the client; VIBEX_PUBLIC_HOST should then");
    println!("  name the address clients reach, for example 192.168.1.10:8765.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairing_payload() -> PairingJson {
        PairingJson {
            schema_version: PAIRING_SCHEMA_VERSION,
            server_id: "server-test".to_string(),
            server_version: VERSION,
            pairing_code: "123-456-789".to_string(),
            pairing_link: "vibex://pair#/code/secret".to_string(),
            tls_certificate_der: Some("certificate".to_string()),
            endpoint: "https://127.0.0.1:8765".to_string(),
            expires_at_ms: 42,
        }
    }

    #[test]
    fn pairing_json_debug_hides_the_one_time_secret() {
        let debug = format!("{:?}", pairing_payload());
        assert!(!debug.contains("123-456-789"));
        assert!(!debug.contains("vibex://pair"));
        assert!(debug.contains("server-test"));
    }

    #[test]
    fn status_json_has_the_stable_schema() {
        let payload = StatusJson {
            schema_version: STATUS_SCHEMA_VERSION,
            running: true,
            pid: Some(7),
            port: 8765,
            version: VERSION,
            home: "/tmp/vibex".to_string(),
            health: "ok",
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&payload).unwrap()).unwrap();
        assert_eq!(value["schemaVersion"], "vibex-server-status.v1");
        assert_eq!(value["running"], true);
        assert_eq!(value["pid"], 7);
        assert_eq!(value["port"], 8765);
        assert_eq!(value["health"], "ok");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_bare_server_process_counts_as_serve() {
        assert!(command_is_serve(&[b"vibex-server".as_slice()]));
        assert!(command_is_serve(&[
            b"/opt/vibex-server".as_slice(),
            b"serve".as_slice(),
            b"--no-pairing".as_slice()
        ]));
        assert!(!command_is_serve(&[
            b"/opt/vibex-server".as_slice(),
            b"tui".as_slice()
        ]));
        assert!(!command_is_serve(&[
            b"/opt/vibex-server".as_slice(),
            b"--help".as_slice()
        ]));
    }

    #[test]
    fn command_parsing_accepts_json_and_stop() {
        assert!(matches!(
            Command::parse(vec!["status".to_string(), "--json".to_string()]).unwrap(),
            Command::Status { json: true }
        ));
        assert!(matches!(
            Command::parse(vec!["stop".to_string()]).unwrap(),
            Command::Stop
        ));
        assert!(matches!(
            Command::parse(vec!["pairing-code".to_string(), "--json".to_string()]).unwrap(),
            Command::PairingCode { json: true, .. }
        ));
    }
}

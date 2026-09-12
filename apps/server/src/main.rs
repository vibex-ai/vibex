use std::error::Error;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use tokio::signal;
use vibex_core::{
    RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel, RemotePairingCodeLink, VibexError,
};
use vibex_db::{apply_migrations, open_database};
use vibex_desktop_runtime::{
    DesktopHomeLock, DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade,
};
use vibex_remote::{RemoteIdentity, RemoteIdentityStore, RemoteTrustService};

const VERSION: &str = env!("CARGO_PKG_VERSION");

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
        Command::PairingCode { permission, ttl_ms } => {
            let config = headless_config()?;
            config_check(&config)?;
            let response = create_pairing_code(&config, permission, ttl_ms)?;
            let certificate = pinned_certificate_for_config(&config)?;
            print_pairing_entry(&config, None, &response, certificate);
            println!("warning=one-time code; it is not stored in plaintext");
        }
        Command::Serve { pairing } => serve(pairing).await?,
        Command::Status => status()?,
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
    Status,
    PairingCode {
        permission: RemoteDevicePermissionLevel,
        ttl_ms: Option<u32>,
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
            "status" => Ok(Self::Status),
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
                Ok(Self::PairingCode { permission, ttl_ms })
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
    let identity = runtime.remote().gateway().identity()?;
    println!("server_id={}", identity.server_id());
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

fn status() -> Result<(), VibexError> {
    let config = DesktopRuntimeConfig::headless_from_environment()?;
    config_check(&config)?;
    let running = if config.acquire_home_lock {
        match DesktopHomeLock::acquire(&config.home_dir, &config.application_id) {
            Ok(lock) => {
                drop(lock);
                false
            }
            Err(error) if error.code == "desktop_runtime_home_locked" => true,
            Err(error) => return Err(error),
        }
    } else {
        false
    };
    println!("running={running}");
    println!(
        "configured_bind={}",
        config.remote_gateway.service.bind_addr
    );
    println!("active_connections=unavailable_without_runtime_probe");
    Ok(())
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
    println!("Usage: vibex-server [serve|status|pairing-code|revoke|config-check]");
    println!("  serve [--no-pairing]                 run the authoritative headless runtime");
    println!("  pairing-code [--permission full-control] [--ttl-ms N]");
    println!("                                       mint a one-time code plus its pairing link");
    println!("  revoke DEVICE_ID [--reason TEXT]     revoke a paired device");
    println!("  config-check                          validate VIBEX_* deployment settings");
    println!("Environment: VIBEX_HOME, VIBEX_DB_PATH, VIBEX_BIND_ADDR, VIBEX_DEPLOYMENT_MODE,");
    println!("  VIBEX_PUBLIC_HOST, VIBEX_ALLOWED_HOSTS, VIBEX_ALLOWED_ORIGINS, VIBEX_TLS_MODE,");
    println!("  VIBEX_TLS_CERT_FILE, VIBEX_TLS_KEY_FILE and VIBEX_*_LIMIT settings.");
    println!("VIBEX_TLS_MODE=pinned_certificate serves a self-signed certificate derived from the");
    println!("  runtime identity, so no CA is needed on the client; VIBEX_PUBLIC_HOST should then");
    println!("  name the address clients reach, for example 192.168.1.10:8765.");
}

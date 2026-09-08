use std::error::Error;
use std::net::SocketAddr;
use tokio::signal;
use vibex_core::{RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel, VibexError};
use vibex_db::{apply_migrations, open_database};
use vibex_desktop_runtime::{
    DesktopHomeLock, DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade,
};
use vibex_remote::RemoteTrustService;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let command = Command::parse(std::env::args().skip(1).collect())?;
    match command {
        Command::Help => print_help(),
        Command::Version => println!("vibex-server {VERSION}"),
        Command::ConfigCheck => {
            let config = DesktopRuntimeConfig::headless_from_environment()?;
            config_check(&config)?;
            println!("configuration=valid");
            print_config_summary(&config);
        }
        Command::PairingCode { permission, ttl_ms } => {
            let config = DesktopRuntimeConfig::headless_from_environment()?;
            config_check(&config)?;
            let response = create_pairing_code(&config, permission, ttl_ms)?;
            println!("pairing_code={}", response.pairing_code);
            println!("expires_at_ms={}", response.pairing.expires_at_ms);
            println!("permission={:?}", response.pairing.permission_level);
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
    let config = DesktopRuntimeConfig::headless_from_environment()?;
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
                println!("pairing_code={}", response.pairing_code);
                println!("pairing_expires_at_ms={}", response.pairing.expires_at_ms);
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
    println!("  revoke DEVICE_ID [--reason TEXT]     revoke a paired device");
    println!("  config-check                          validate VIBEX_* deployment settings");
    println!("Environment: VIBEX_HOME, VIBEX_DB_PATH, VIBEX_BIND_ADDR, VIBEX_DEPLOYMENT_MODE,");
    println!("  VIBEX_PUBLIC_HOST, VIBEX_ALLOWED_HOSTS, VIBEX_ALLOWED_ORIGINS, VIBEX_TLS_MODE,");
    println!("  VIBEX_TLS_CERT_FILE, VIBEX_TLS_KEY_FILE and VIBEX_*_LIMIT settings.");
}

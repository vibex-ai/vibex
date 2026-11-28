use std::error::Error;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use vibex_relay_server::{RelayServerConfig, build_router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("vibex-relay-server");
        println!("  VIBEX_RELAY_BIND_ADDR=127.0.0.1:9700");
        println!("  RELAY_PORT=9700");
        println!(
            "  VIBEX_RELAY_MAX_TRAFFIC_BYTES=<bytes|KB|MB|GB|TB> (optional, disabled by default)"
        );
        println!("  VIBEX_RELAY_TRAFFIC_LIMIT_MODE=daily|weekly|monthly|total (default: monthly)");
        println!("  VIBEX_RELAY_TRAFFIC_STATE_PATH=<path> (optional JSON state file)");
        return Ok(());
    }

    // The server's own diagnostics (`warn!` on a rejected frame, an expired
    // room, a failed route) are the only way to see why a device cannot reach
    // its desktop, and they were all dropped: the crate emits `tracing` events
    // but installs no subscriber. Default to warnings on stderr, overridable
    // with VIBEX_RELAY_LOG.
    let filter = std::env::var("VIBEX_RELAY_LOG")
        .unwrap_or_else(|_| "vibex_relay_server=info,warn".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();

    let config = RelayServerConfig::from_env()?;
    let bind_addr = config.bind_addr;
    let router = build_router(config);
    let listener = TcpListener::bind(bind_addr).await?;

    tracing::info!(%bind_addr, "vibex relay server listening");
    // Connect info carries the direct peer address, which is what decides
    // whether a forwarding header may be trusted for the per-address limits.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

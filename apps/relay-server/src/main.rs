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
        return Ok(());
    }

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

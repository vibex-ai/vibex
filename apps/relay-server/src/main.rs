use std::error::Error;

use tokio::net::TcpListener;
use vibex_relay_server::{RelayServerConfig, try_build_router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("vibex-relay-server");
        println!("  VIBEX_RELAY_BIND_ADDR=127.0.0.1:9700");
        println!("  VIBEX_RELAY_WEB_STATIC_DIR=/app/web");
        println!("  RELAY_PORT=9700");
        return Ok(());
    }

    let config = RelayServerConfig::from_env()?;
    let bind_addr = config.bind_addr;
    let router = try_build_router(config)?;
    let listener = TcpListener::bind(bind_addr).await?;

    tracing::info!(%bind_addr, "vibex relay server listening");
    axum::serve(listener, router).await?;
    Ok(())
}

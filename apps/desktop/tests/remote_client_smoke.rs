#![cfg(not(target_family = "wasm"))]

//! Stack-level smoke for the desktop remote-client mode: a real gateway, the
//! operator pairing-code claim endpoint, a `WebRemoteBackend` built from the
//! stored credential, and a typed RPC round-trip — the exact path the new
//! "Remote Runtime" settings page drives.

use std::time::Duration;

use tokio::net::TcpListener;
use vibex_agent::AgentManager;
use vibex_backend::AgentBackend as _;
use vibex_core::{RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel};
use vibex_db::{apply_migrations, open_database};
use vibex_desktop::remote_client::{DesktopRemoteCredentialStore, claim_server_pairing_code};
use vibex_remote::{
    RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteServiceConfig, RemoteTrustService,
};
use vibex_remote_client::RemoteConnectionState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_code_claim_connects_web_remote_backend() {
    let root = std::env::temp_dir().join(format!(
        "vibex-desktop-remote-client-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create test directory");
    let db_path = root.join("db.sqlite");
    let identity_path = root.join("identity.json");
    let mut conn = open_database(&db_path).expect("open test database");
    apply_migrations(&mut conn).expect("apply test migrations");

    let port_reservation = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve gateway port");
    let gateway_address = port_reservation.local_addr().expect("reserved address");
    drop(port_reservation);
    let service = RemoteServiceConfig {
        enabled: true,
        bind_addr: gateway_address.to_string(),
        ..RemoteServiceConfig::default()
    };
    let agent_manager = std::sync::Arc::new(AgentManager::new(&db_path).expect("agent manager"));
    let dispatcher = RemoteDispatcher::with_agent_manager(service.clone(), agent_manager);
    let mut gateway_config = RemoteGatewayConfig::loopback_enabled(gateway_address.to_string());
    gateway_config.service = service;
    let gateway = RemoteGateway::new(gateway_config, dispatcher, &db_path, &identity_path);
    gateway.start().await.expect("start gateway");

    // The operator mints the code exactly like `vibex-server serve` does.
    let pairing = RemoteTrustService::create_pairing_code(
        &conn,
        RemoteCreatePairingCodeRequest {
            permission_level: RemoteDevicePermissionLevel::FullControl,
            ttl_ms: Some(60_000),
        },
    )
    .expect("create pairing code");

    // The client claims it through the real HTTP endpoint via the same
    // function the Remote Runtime settings page drives. Loopback HTTP is the
    // explicit development exception. The bundle pins the server identity and
    // binds the grant to a client-generated key.
    let base_url = format!("http://{gateway_address}");
    let credential =
        claim_server_pairing_code(base_url.clone(), pairing.pairing_code.clone(), true)
            .await
            .expect("claim pairing code");
    assert!(
        !credential.record.auth.auth_token.is_empty(),
        "the claim must return an auth token"
    );
    assert_eq!(
        credential.expected_server_id,
        gateway.identity().expect("id").server_id()
    );

    // The credential persists and restores into an identical record.
    let store = DesktopRemoteCredentialStore::new(&root);
    store.save(&credential).expect("save credential");
    let restored = store.load().expect("restore credential");
    assert_eq!(restored, credential);

    // Reuse of the one-time code is rejected by the same endpoint.
    let reused = claim_server_pairing_code(base_url, pairing.pairing_code, true).await;
    assert!(reused.is_err(), "a claimed code must be single-use");

    // The backend built from the credential performs the full handshake.
    let backend = credential.backend().expect("remote backend");
    let server_info = tokio::time::timeout(Duration::from_secs(10), backend.connect())
        .await
        .expect("connect within timeout")
        .expect("handshake");
    assert_eq!(server_info.server_id, credential.expected_server_id);
    assert_eq!(
        backend.connection_state().state,
        RemoteConnectionState::Online
    );

    // A typed RPC round-trip proves the authenticated business channel.
    let sessions = tokio::time::timeout(Duration::from_secs(10), backend.list_sessions(false))
        .await
        .expect("list within timeout")
        .expect("list sessions");
    assert!(sessions.is_empty(), "a fresh runtime has no sessions");

    gateway.stop().await.expect("stop gateway");
    let _ = std::fs::remove_dir_all(&root);
}

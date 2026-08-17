#![cfg(not(target_family = "wasm"))]

use std::time::Duration;

use vibex_core::{RemoteAuthProof, RemoteDevicePermissionLevel};
use vibex_remote::{RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteServiceConfig};
use vibex_remote_client::{
    AutoRemoteTransport, AutoRemoteTransportConfig, DirectCandidate, DirectWebSocketTransport,
    RemoteClientConfig, RemoteConnectionState, RemoteTransport, ZeroConfigLanPairingSession,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_config_pairing_connects_over_pinned_lan_without_other_routes() {
    let root = std::env::temp_dir().join(format!(
        "vibex-local-lan-smoke-{}-{}",
        std::process::id(),
        vibex_core::unix_timestamp_ms()
    ));
    std::fs::create_dir_all(&root).expect("create test directory");
    let dispatcher = RemoteDispatcher::new(RemoteServiceConfig::loopback_disabled());
    let gateway = RemoteGateway::new(
        RemoteGatewayConfig::loopback_enabled("127.0.0.1:0"),
        dispatcher,
        root.join("vibex.db"),
        root.join("desktop-identity.json"),
    );
    let first_local = gateway
        .start_local_lan_gateway(0)
        .await
        .expect("start local LAN Gateway");
    gateway.stop_local_lan_gateway().await;
    let local = gateway
        .start_local_lan_gateway(0)
        .await
        .expect("restart local LAN Gateway");
    assert_eq!(
        local.tls_certificate_base64,
        first_local.tls_certificate_base64
    );
    let window = gateway
        .start_zero_config_lan_pairing(RemoteDevicePermissionLevel::FullControl, 60_000)
        .await
        .expect("start zero-config pairing");
    assert!(
        gateway
            .current_config()
            .pairing_routes
            .direct_candidates
            .is_empty()
    );
    assert!(
        gateway
            .current_config()
            .pairing_routes
            .relay_candidate
            .is_none()
    );

    let bootstrap = gateway
        .zero_config_lan_pairing_listener_addr()
        .expect("zero-config listener");
    let mut session = ZeroConfigLanPairingSession::start(
        format!("http://127.0.0.1:{}", bootstrap.port()),
        &window.discovery.server_id,
        &window.discovery.server_identity_public_key,
        "LAN-only test phone",
    )
    .await
    .expect("start encrypted pairing session");
    let request_id = gateway
        .zero_config_lan_pairing_window_status()
        .expect("pairing window status")
        .pending_requests
        .into_iter()
        .next()
        .expect("pending pairing request")
        .request_id;
    gateway
        .approve_zero_config_lan_pairing_request(&request_id)
        .expect("approve pairing request");
    tokio::time::sleep(Duration::from_millis(510)).await;
    let status = session.poll().await.expect("poll approved pairing");
    let claim = session
        .claim_approved(status)
        .await
        .expect("claim pairing grant");

    assert_eq!(
        claim.local_network_url,
        format!("https://127.0.0.1:{}", local.bound_addr.port())
    );
    let device_id = claim.response.device.device_id;
    let device_identity = vibex_remote_client::ClientDeviceIdentity::from_private_key_base64(
        device_id.clone(),
        &claim.identity.private_key_base64(),
    )
    .expect("bind claimed device identity");
    let mut remote = RemoteClientConfig::new(
        &claim.local_network_url,
        RemoteAuthProof {
            device_id,
            auth_token: claim.response.device_grant_token,
        },
    )
    .with_device_identity(device_identity);
    remote.expected_server_id = Some(claim.offer.summary.server_id);
    remote.expected_server_identity_public_key =
        Some(claim.offer.summary.server_identity_public_key);
    remote.pinned_tls_certificate_der = Some(claim.lan_gateway_tls_certificate.clone());
    let direct = DirectWebSocketTransport::new(remote.clone()).expect("construct pinned probe");
    direct.probe().await.expect("probe pinned LAN route");
    direct
        .connect()
        .await
        .expect("connect pinned LAN route directly");
    direct
        .disconnect()
        .await
        .expect("disconnect direct LAN route");
    let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
        remote,
        direct_candidates: vec![DirectCandidate {
            url: claim.local_network_url,
            label: "local-network".to_string(),
            priority: 0,
            tls_certificate_der: Some(claim.lan_gateway_tls_certificate),
        }],
        relay: None,
    })
    .expect("construct LAN-only transport");

    let server = transport.connect().await.expect("connect pinned LAN route");
    assert_eq!(server.selected_protocol.major, 2);
    assert_eq!(transport.state().state, RemoteConnectionState::Online);
    transport
        .heartbeat()
        .await
        .expect("heartbeat pinned LAN route");
    transport.disconnect().await.expect("disconnect LAN route");
    gateway.stop().await.expect("stop Gateway");
    let _ = std::fs::remove_dir_all(root);
}

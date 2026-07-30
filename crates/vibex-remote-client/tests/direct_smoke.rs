#![cfg(not(target_family = "wasm"))]

use std::time::Duration;

use tokio::net::TcpListener;
use vibex_core::{
    DeviceId, RemoteAuthProof, RemoteClaimPairingOfferRequest, RemoteControlMessageV2,
    RemoteCreatePairingOfferRequest, RemoteDevicePermissionLevel,
};
use vibex_db::{apply_migrations, open_database};
use vibex_remote::{
    RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteServiceConfig, RemoteTrustService,
};
use vibex_remote_client::{
    ActiveRemoteRoute, AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity,
    DirectCandidate, DirectWebSocketTransport, RemoteClientConfig, RemoteConnectionState,
    RemoteTransport, RemoteTransportEvent, claim_pairing_offer,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_gateway_pair_and_v2_handshake_smoke() {
    let root = std::env::temp_dir().join(format!("vibex-remote-client-{}", std::process::id()));
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
    let dispatcher = RemoteDispatcher::new(service.clone());
    let mut gateway_config = RemoteGatewayConfig::loopback_enabled(gateway_address.to_string());
    gateway_config.service = service;
    let gateway = RemoteGateway::new(gateway_config, dispatcher, &db_path, &identity_path);
    let address = gateway
        .start()
        .await
        .expect("start gateway")
        .expect("bound address");
    assert_eq!(address, gateway_address);
    let identity = gateway.identity().expect("gateway identity");

    let device_identity_seed =
        ClientDeviceIdentity::generate(DeviceId::new()).expect("client identity");
    let offer = RemoteTrustService::create_pairing_offer(
        &conn,
        &identity,
        RemoteCreatePairingOfferRequest {
            permission_level: RemoteDevicePermissionLevel::FullControl,
            ttl_ms: Some(60_000),
            direct_candidates: Vec::new(),
            relay_candidate: None,
        },
    )
    .expect("create pairing offer");

    let base_url = format!("http://{address}");
    let mut probe_config = RemoteClientConfig::new(
        &base_url,
        RemoteAuthProof {
            device_id: DeviceId::new(),
            auth_token: "probe-does-not-authenticate".to_string(),
        },
    );
    probe_config.allow_insecure_local_dev = true;
    probe_config.expected_server_id = Some(offer.offer.summary.server_id.clone());
    probe_config.expected_server_identity_public_key =
        Some(offer.offer.summary.server_identity_public_key.clone());
    let probe_transport =
        DirectWebSocketTransport::new(probe_config).expect("construct probe transport");
    let probe = probe_transport
        .select_direct_candidate(vec![DirectCandidate {
            url: base_url.clone(),
            label: "loopback".to_string(),
            priority: 0,
        }])
        .await
        .expect("probe gateway without consuming offer");
    assert_eq!(probe.info.server_id, offer.offer.summary.server_id);

    let claim = claim_pairing_offer(
        &base_url,
        RemoteClaimPairingOfferRequest {
            offer_id: offer.offer.summary.offer_id.clone(),
            one_time_challenge: offer.offer.one_time_challenge.clone(),
            expected_server_id: offer.offer.summary.server_id.clone(),
            expected_server_identity_public_key: offer
                .offer
                .summary
                .server_identity_public_key
                .clone(),
            display_name: "direct-smoke".to_string(),
            device_identity_public_key: device_identity_seed.public_key_base64(),
            claim_nonce: "claim_nonce_direct_smoke_abcdefghijklmnopqrstuvwxyz".to_string(),
        },
        true,
    )
    .await
    .expect("claim request");

    let device_identity = ClientDeviceIdentity::from_private_key_base64(
        claim.device.device_id.clone(),
        &device_identity_seed.private_key_base64(),
    )
    .expect("bind client identity to claimed device");
    let mut config = RemoteClientConfig::new(
        &base_url,
        RemoteAuthProof {
            device_id: claim.device.device_id.clone(),
            auth_token: claim.device_grant_token,
        },
    )
    .with_device_identity(device_identity);
    config.allow_insecure_local_dev = true;
    config.expected_server_id = Some(offer.offer.summary.server_id);
    config.expected_server_identity_public_key =
        Some(offer.offer.summary.server_identity_public_key);
    config.reconnect_initial = Duration::from_millis(100);
    config.reconnect_max = Duration::from_millis(100);
    config.max_reconnect_attempts = 5;
    let transport =
        DirectWebSocketTransport::new(config.clone()).expect("construct direct transport");
    let server_info = transport.connect().await.expect("connect direct transport");

    assert_eq!(server_info.selected_protocol.major, 2);
    assert_eq!(transport.state().state, RemoteConnectionState::Online);
    transport.heartbeat().await.expect("heartbeat");
    transport.disconnect().await.expect("disconnect");
    assert_eq!(transport.state().state, RemoteConnectionState::Offline);
    let reconnected = transport
        .reconnect()
        .await
        .expect("reconnect direct transport");
    assert_eq!(reconnected.server_id, server_info.server_id);
    assert_eq!(transport.state().state, RemoteConnectionState::Online);
    transport
        .heartbeat()
        .await
        .expect("heartbeat after reconnect");
    transport.disconnect().await.expect("final disconnect");

    let auto = AutoRemoteTransport::new(AutoRemoteTransportConfig {
        remote: config,
        direct_candidates: vec![DirectCandidate {
            url: base_url,
            label: "loopback-restart".to_string(),
            priority: 0,
        }],
        relay: None,
    })
    .expect("construct auto transport");
    auto.connect().await.expect("connect auto transport");
    gateway.stop().await.expect("stop gateway for recovery");
    let close = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = auto
                .next_domain_event()
                .await
                .expect("read gateway close")
                .expect("gateway close event");
            if matches!(
                event,
                RemoteTransportEvent::Closed
                    | RemoteTransportEvent::Control(RemoteControlMessageV2::Close(_))
            ) {
                break event;
            }
        }
    })
    .await
    .expect("gateway close timeout");
    assert!(matches!(
        close,
        RemoteTransportEvent::Closed
            | RemoteTransportEvent::Control(RemoteControlMessageV2::Close(_))
    ));
    let retry_started = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if auto.state().reconnect_attempt >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        retry_started.is_ok(),
        "automatic recovery did not retry after the first failed selection: state={:?}, route={:?}",
        auto.state(),
        auto.active_route(),
    );

    let restarted_address = gateway
        .start()
        .await
        .expect("restart gateway")
        .expect("restarted gateway address");
    assert_eq!(restarted_address, gateway_address);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if auto.state().state == RemoteConnectionState::Online
                && auto.active_route() == ActiveRemoteRoute::Direct
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Auto transport did not recover after gateway restart");
    auto.heartbeat()
        .await
        .expect("heartbeat after automatic route recovery");
    auto.disconnect().await.expect("disconnect auto transport");

    gateway.stop().await.expect("stop gateway");
    let _ = std::fs::remove_dir_all(root);
}

#![cfg(not(target_family = "wasm"))]

//! Self-contained bisect for the harness handshake failure: assemble gateway
//! exactly like the harness (DesktopRuntime), claim via RemoteTrustService,
//! and connect with the native transport, printing each stage.

use vibex_core::{
    DeviceId, RemoteAuthProof, RemoteCreatePairingOfferRequest, RemoteDevicePermissionLevel,
    RemotePairingCandidate, RemotePairingTransport,
};
use vibex_db::open_database;
use vibex_desktop_runtime::{DesktopRuntime, DesktopRuntimeConfig};
use vibex_remote::{RemoteGatewayConfig, RemoteTrustService};
use vibex_remote_client::{
    AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity, DirectCandidate,
    DirectWebSocketTransport, RemoteClientConfig, RemoteTransport, pairing_claim_request,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "diagnostic bisect"]
async fn e2e_handshake_bisect() {
    let home = std::env::temp_dir().join(format!(
        "vibex-bisect-{}-{}",
        std::process::id(),
        vibex_core::RequestId::new().as_str()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let mut config = DesktopRuntimeConfig::isolated_test(&home);
    let mut gateway_config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
    let variant = std::env::var("BISECT_VARIANT").unwrap_or_default();
    let public_url = "https://dev-home.tail525c5d.ts.net".to_string();
    let tailnet_candidate = RemotePairingCandidate {
        transport: RemotePairingTransport::Tailnet,
        url: public_url.clone(),
        relay_room_id: None,
        relay_pc_peer_id: None,
        relay_pc_public_key: None,
    };
    if variant.contains("proxy") {
        gateway_config.tls_policy = vibex_remote::RemoteGatewayTlsPolicy::TrustedHttpsProxy;
        gateway_config
            .allowed_hosts
            .push("dev-home.tail525c5d.ts.net".to_string());
        gateway_config
            .allowed_origins
            .push("https://dev-home.tail525c5d.ts.net".to_string());
        gateway_config
            .allowed_origins
            .push("https://dev-home.tail525c5d.ts.net:8443".to_string());
    }
    if variant.contains("routes") {
        gateway_config.pairing_routes.direct_candidates = vec![tailnet_candidate.clone()];
    }
    config.remote_gateway = gateway_config;
    let runtime = DesktopRuntime::start(config).await.unwrap();
    let gateway = runtime.remote().gateway();
    let address = gateway.status().bound_addr.expect("gateway bound");
    let base_url = format!("http://{address}");
    let identity = gateway.identity().unwrap();
    println!("stage1 gateway up at {base_url}");

    let seed = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
    let connection = open_database(&runtime.config().database_path).unwrap();
    let offer = RemoteTrustService::create_pairing_offer(
        &connection,
        &identity,
        RemoteCreatePairingOfferRequest {
            permission_level: RemoteDevicePermissionLevel::FullControl,
            ttl_ms: Some(120_000),
            direct_candidates: vec![if variant.contains("tailnet_offer") {
                tailnet_candidate.clone()
            } else {
                RemotePairingCandidate {
                    transport: RemotePairingTransport::Direct,
                    url: base_url.clone(),
                    relay_room_id: None,
                    relay_pc_peer_id: None,
                    relay_pc_public_key: None,
                }
            }],
            relay_candidate: None,
        },
    )
    .unwrap()
    .offer;
    let claim = pairing_claim_request(
        &offer,
        "Bisect client",
        seed.public_key_base64(),
        vibex_core::RequestId::new().into_string(),
    )
    .unwrap();
    let claimed = RemoteTrustService::claim_pairing_offer(&connection, claim).unwrap();
    drop(connection);
    println!(
        "stage2 claimed device {}",
        claimed.device.device_id.as_str()
    );

    let device_identity = ClientDeviceIdentity::from_private_key_base64(
        claimed.device.device_id.clone(),
        &seed.private_key_base64(),
    )
    .unwrap();
    let mut remote = RemoteClientConfig::new(
        &base_url,
        RemoteAuthProof {
            device_id: claimed.device.device_id.clone(),
            auth_token: claimed.device_grant_token.clone(),
        },
    )
    .with_device_identity(device_identity);
    remote.allow_insecure_local_dev = true;
    remote.expected_server_id = Some(identity.server_id().to_string());
    remote.expected_server_identity_public_key = Some(identity.public_key_base64());

    println!("stage3 trying plain DirectWebSocketTransport...");
    let direct = DirectWebSocketTransport::new(remote.clone()).unwrap();
    match direct.connect().await {
        Ok(info) => println!("stage3 DIRECT_OK epoch={}", info.session_epoch),
        Err(error) => println!(
            "stage3 DIRECT_FAILED code={} msg={}",
            error.code, error.message
        ),
    }
    direct.disconnect().await.ok();

    println!("stage4 trying AutoRemoteTransport...");
    let auto = AutoRemoteTransport::new(AutoRemoteTransportConfig {
        remote,
        direct_candidates: vec![DirectCandidate {
            url: base_url.clone(),
            label: "bisect".to_string(),
            priority: 0,
            tls_certificate_der: None,
        }],
        relay: None,
    })
    .unwrap();
    match auto.connect().await {
        Ok(info) => println!("stage4 AUTO_OK epoch={}", info.session_epoch),
        Err(error) => println!(
            "stage4 AUTO_FAILED code={} msg={}",
            error.code, error.message
        ),
    }
    auto.disconnect().await.ok();
    let _ = std::fs::remove_dir_all(&home);
}

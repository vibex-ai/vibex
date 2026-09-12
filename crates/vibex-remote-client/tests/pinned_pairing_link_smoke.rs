#![cfg(not(target_family = "wasm"))]

//! End-to-end coverage for the operator-facing pairing link against a runtime
//! that serves its own certificate.
//!
//! The link is the only channel that carries the certificate, so these tests
//! pin down the three properties the feature rests on: the listener really
//! speaks TLS under the `pinned_certificate` policy, pairing from a link works
//! without any CA, and a certificate that did not come from the link is
//! refused.

use std::net::SocketAddr;
use std::path::PathBuf;

use vibex_core::{
    RemoteAuthProof, RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel,
    RemotePairingCodeLink,
};
use vibex_db::{apply_migrations, open_database};
use vibex_remote::{
    RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteGatewayDeploymentMode,
    RemoteGatewayTlsPolicy, RemoteServiceConfig, RemoteTrustService,
};
use vibex_remote_client::{
    ClientDeviceIdentity, DirectWebSocketTransport, RemoteClientConfig, RemoteTransport,
    claim_pairing_code_link_with_identity, claim_pairing_code_with_identity,
};

struct PinnedRuntime {
    root: PathBuf,
    gateway: RemoteGateway,
    bound: SocketAddr,
}

impl PinnedRuntime {
    fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.bound.port())
    }

    fn certificate(&self) -> String {
        self.gateway
            .pinned_tls_certificate_base64()
            .expect("certificate derivation")
            .expect("pinned policy serves a certificate")
    }

    fn mint_pairing_code(&self) -> String {
        let mut connection = open_database(&self.root.join("vibex.db")).expect("open database");
        apply_migrations(&mut connection).expect("apply migrations");
        RemoteTrustService::create_pairing_code(
            &connection,
            RemoteCreatePairingCodeRequest {
                permission_level: RemoteDevicePermissionLevel::FullControl,
                ttl_ms: Some(60_000),
            },
        )
        .expect("mint pairing code")
        .pairing_code
    }
}

/// LAN deployment on a loopback address: the production shape
/// (`VIBEX_DEPLOYMENT_MODE=lan`, `VIBEX_TLS_MODE=pinned_certificate`) without
/// listening on a real network interface.
async fn start_pinned_runtime(name: &str) -> PinnedRuntime {
    let root = std::env::temp_dir().join(format!(
        "vibex-pinned-link-{name}-{}-{}",
        std::process::id(),
        vibex_core::unix_timestamp_ms()
    ));
    std::fs::create_dir_all(&root).expect("create test directory");
    let mut config = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");
    config.deployment_mode = RemoteGatewayDeploymentMode::Lan;
    config.tls_policy = RemoteGatewayTlsPolicy::PinnedCertificate;
    let gateway = RemoteGateway::new(
        config,
        RemoteDispatcher::new(RemoteServiceConfig::loopback_disabled()),
        root.join("vibex.db"),
        root.join("desktop-identity.json"),
    );
    let bound = gateway
        .start()
        .await
        .expect("start pinned Gateway")
        .expect("pinned Gateway binds a listener");
    PinnedRuntime {
        root,
        gateway,
        bound,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_link_pins_the_self_signed_certificate_end_to_end() {
    let runtime = start_pinned_runtime("connect").await;
    let certificate = runtime.certificate();
    let link = RemotePairingCodeLink::new(
        runtime.url(),
        runtime.mint_pairing_code(),
        Some(certificate.clone()),
    )
    .expect("build pairing link");
    let encoded = link.encode().expect("encode pairing link");
    assert!(encoded.starts_with("vibex://pair#/code/"));
    let parsed = RemotePairingCodeLink::parse(&encoded).expect("parse pairing link");

    let bundle = claim_pairing_code_link_with_identity(parsed, "Pinned smoke desktop", false)
        .await
        .expect("claim pairing link");
    assert_eq!(bundle.credential.server_url, runtime.url());

    let device_id = bundle.credential.auth.device_id.clone();
    let identity = ClientDeviceIdentity::from_private_key_base64(
        device_id.clone(),
        &bundle.identity.private_key_base64(),
    )
    .expect("bind claimed identity");
    let mut config = RemoteClientConfig::new(
        runtime.url(),
        RemoteAuthProof {
            device_id,
            auth_token: bundle.credential.auth.auth_token.clone(),
        },
    )
    .with_device_identity(identity);
    config.expected_server_id = Some(bundle.server_id.clone());
    config.expected_server_identity_public_key =
        bundle.credential.server_identity_public_key.clone();
    config.pinned_tls_certificate_der = Some(certificate);

    let transport = DirectWebSocketTransport::new(config).expect("construct pinned transport");
    transport.probe().await.expect("probe pinned route");
    transport.connect().await.expect("connect pinned route");
    transport
        .disconnect()
        .await
        .expect("disconnect pinned route");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_without_the_pin_cannot_trust_the_runtime() {
    let runtime = start_pinned_runtime("unpinned").await;
    // If `pinned_certificate` silently served plaintext, this claim would
    // succeed; it must instead fail on the self-signed certificate.
    claim_pairing_code_with_identity(
        runtime.url(),
        runtime.mint_pairing_code(),
        "Unpinned smoke client",
        false,
    )
    .await
    .expect_err("an unpinned client must not accept a self-signed runtime");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claim_with_a_certificate_from_another_runtime_is_rejected() {
    let foreign = start_pinned_runtime("foreign").await;
    let foreign_certificate = foreign.certificate();
    let runtime = start_pinned_runtime("target").await;
    let link = RemotePairingCodeLink::new(
        runtime.url(),
        runtime.mint_pairing_code(),
        Some(foreign_certificate),
    )
    .expect("build pairing link");

    claim_pairing_code_link_with_identity(link, "Mismatched smoke client", false)
        .await
        .expect_err("a certificate that did not come from this runtime must be refused");
}

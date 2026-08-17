#![cfg(not(target_family = "wasm"))]

//! Native diagnostic probe for the E2E gateway harness. Reads the same
//! credential bundle the browser client uses and attempts the v2 handshake
//! with the native transport, isolating wasm-specific failures from
//! gateway/proxy issues. Run while the harness is up:
//!
//! ```text
//! VIBEX_E2E_BUNDLE_FILE=... VIBEX_E2E_PROBE_URL=https://... \
//! cargo test -p vibex-remote-client --test e2e_native_probe --locked -- --ignored --nocapture
//! ```

use vibex_core::{DeviceId, RemoteAuthProof};
use vibex_remote_client::{
    AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity, DirectCandidate,
    RemoteClientConfig, RemoteTransport,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "diagnostic probe; requires a running e2e_gateway_harness"]
async fn e2e_native_probe() {
    let bundle_file = std::env::var("VIBEX_E2E_BUNDLE_FILE").expect("VIBEX_E2E_BUNDLE_FILE");
    let probe_url = std::env::var("VIBEX_E2E_PROBE_URL").expect("VIBEX_E2E_PROBE_URL");
    let bundle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle_file).expect("read bundle")).unwrap();

    let device_id =
        DeviceId::parse(bundle["record"]["auth"]["deviceId"].as_str().unwrap()).expect("device id");
    let identity = ClientDeviceIdentity::from_private_key_base64(
        device_id.clone(),
        bundle["identityPrivateKey"].as_str().unwrap(),
    )
    .expect("identity");
    let mut config = RemoteClientConfig::new(
        &probe_url,
        RemoteAuthProof {
            device_id,
            auth_token: bundle["record"]["auth"]["authToken"]
                .as_str()
                .unwrap()
                .to_string(),
        },
    )
    .with_device_identity(identity);
    config.expected_server_id = Some(bundle["expectedServerId"].as_str().unwrap().to_string());
    config.expected_server_identity_public_key = Some(
        bundle["record"]["serverIdentityPublicKey"]
            .as_str()
            .unwrap()
            .to_string(),
    );
    config.allow_insecure_local_dev = probe_url.starts_with("http://");

    let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
        remote: config,
        direct_candidates: vec![DirectCandidate {
            url: probe_url.clone(),
            label: "native-probe".to_string(),
            priority: 0,
            tls_certificate_der: None,
        }],
        relay: None,
    })
    .expect("auto transport");
    match transport.connect().await {
        Ok(info) => {
            println!(
                "NATIVE_PROBE_OK server_id={} epoch={}",
                info.server_id, info.session_epoch
            );
            transport.disconnect().await.ok();
        }
        Err(error) => {
            println!(
                "NATIVE_PROBE_FAILED code={} message={}",
                error.code, error.message
            );
            panic!("native probe failed");
        }
    }
}

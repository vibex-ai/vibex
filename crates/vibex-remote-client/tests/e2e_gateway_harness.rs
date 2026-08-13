#![cfg(not(target_family = "wasm"))]

//! Long-running local controlled-environment harness for the four-target GPUI
//! workflow E2E matrix (`scripts/e2e-workflows.mjs`).
//!
//! This is not a regular test: it boots the full `DesktopRuntime` (agent
//! submission coordinator, runtime selection, provider services, terminals,
//! RemoteGateway and RelayClientRuntime) with the gateway listener enabled,
//! writes a `vibex-remote-client-credentials.v1` bundle for the client under
//! test, and then serves a loopback control API for the runner's fixture and
//! recovery hooks until the process is killed.  Launch it through
//! `scripts/e2e-local-env/run-target.mjs`; direct invocation:
//!
//! ```text
//! VIBEX_E2E_ROOT=... VIBEX_E2E_GATEWAY_PORT=... VIBEX_E2E_CONTROL_PORT=... \
//! VIBEX_E2E_BUNDLE_FILE=... VIBEX_E2E_PUBLIC_URL=https://host.ts.net \
//! VIBEX_E2E_TRANSPORT=direct VIBEX_E2E_CLIENT_TYPE=mobile \
//! cargo test -p vibex-remote-client --test e2e_gateway_harness --locked -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use vibex_core::{
    DeviceId, RelayPeerId, RemoteCreatePairingOfferRequest, RemoteDevicePermissionLevel,
    RemotePairingCandidate, RemotePairingTransport, RemoteRevokeDeviceRequest, WorkspaceMode,
};
use vibex_db::{RemoteDeviceRepository, SessionRepository, WorkspaceRepository, open_database};
use vibex_desktop_runtime::{
    DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade, RelayClientConnectionState,
    RelayClientSettingsUpdate,
};
use vibex_remote::{
    RemoteGatewayConfig, RemoteGatewayDeploymentMode, RemoteGatewayTlsPolicy, RemoteTrustService,
};
use vibex_remote_client::{ClientDeviceIdentity, pairing_claim_request};

fn env_required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn env_optional(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct HarnessState {
    db_path: PathBuf,
    root: PathBuf,
    transport: String,
    runtime: Arc<DesktopRuntime>,
    client_device_id: DeviceId,
    disposable_offer_candidate: RemotePairingCandidate,
    fixture: tokio::sync::Mutex<Option<FixtureState>>,
}

struct FixtureState {
    disposable_device_id: DeviceId,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running local E2E harness; launched by scripts/e2e-local-env/run-target.mjs"]
async fn e2e_gateway_harness() {
    let root = PathBuf::from(env_required("VIBEX_E2E_ROOT"));
    let gateway_port: u16 = env_required("VIBEX_E2E_GATEWAY_PORT").parse().unwrap();
    let control_port: u16 = env_required("VIBEX_E2E_CONTROL_PORT").parse().unwrap();
    let bundle_file = PathBuf::from(env_required("VIBEX_E2E_BUNDLE_FILE"));
    let transport = env_required("VIBEX_E2E_TRANSPORT");
    let client_type = env_optional("VIBEX_E2E_CLIENT_TYPE").unwrap_or_else(|| "mobile".into());
    let public_url = env_required("VIBEX_E2E_PUBLIC_URL");
    let relay_url = env_optional("VIBEX_E2E_RELAY_URL");
    let public_relay_url = env_optional("VIBEX_E2E_PUBLIC_RELAY_URL");
    assert!(
        matches!(transport.as_str(), "direct" | "relay"),
        "VIBEX_E2E_TRANSPORT must be direct or relay"
    );

    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create harness root");

    let public_host = public_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let mut gateway_config =
        RemoteGatewayConfig::loopback_enabled(format!("127.0.0.1:{gateway_port}"));
    gateway_config.deployment_mode = RemoteGatewayDeploymentMode::Loopback;
    gateway_config.tls_policy = RemoteGatewayTlsPolicy::TrustedHttpsProxy;
    gateway_config.allowed_hosts.push(public_host.clone());
    gateway_config
        .allowed_origins
        .push(format!("https://{public_host}"));
    for origin in env_optional("VIBEX_E2E_EXTRA_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .filter(|origin| !origin.is_empty())
    {
        gateway_config.allowed_origins.push(origin.to_string());
    }
    let direct_candidate = RemotePairingCandidate {
        transport: RemotePairingTransport::Tailnet,
        url: public_url.clone(),
        relay_room_id: None,
        relay_pc_peer_id: None,
        relay_pc_public_key: None,
    };
    if transport == "direct" {
        gateway_config.pairing_routes.direct_candidates = vec![direct_candidate.clone()];
    } else {
        // Relay-only run: keep the Direct listener down so workflow traffic
        // cannot silently use the Direct route.
        gateway_config.service.enabled = false;
        // The gateway advertises `device_pairing` only when a pairing route is
        // configured, and the Relay route needs the persistent room settings.
        // Boot the runtime once to materialize them, shut it down, and restart
        // below with the Relay pairing route in the gateway configuration.
        let relay_url_value = relay_url
            .clone()
            .expect("VIBEX_E2E_RELAY_URL is required for relay transport");
        let public_relay_url_value = public_relay_url
            .clone()
            .expect("VIBEX_E2E_PUBLIC_RELAY_URL is required for relay transport");
        let mut bootstrap_config = DesktopRuntimeConfig::isolated_test(&root);
        bootstrap_config.remote_gateway = RemoteGatewayConfig::default();
        let bootstrap = DesktopRuntime::start(bootstrap_config)
            .await
            .expect("bootstrap desktop runtime");
        let relay = bootstrap.relay();
        relay
            .update_settings(RelayClientSettingsUpdate {
                enabled: Some(true),
                relay_url: Some(Some(relay_url_value)),
                ..RelayClientSettingsUpdate::default()
            })
            .await
            .expect("configure relay client during bootstrap");
        relay.start().await.expect("start bootstrap relay client");
        for attempt in 0..200 {
            if relay.get_status().await.state == RelayClientConnectionState::Connected {
                break;
            }
            assert!(attempt < 199, "bootstrap relay client did not connect");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let settings = relay.get_settings().await;
        let status = relay.get_status().await;
        relay.stop().await.expect("stop bootstrap relay client");
        DesktopRuntimeFacade::shutdown(bootstrap.as_ref())
            .await
            .expect("shutdown bootstrap runtime");
        gateway_config.pairing_routes.relay_candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: public_relay_url_value,
            relay_room_id: Some(settings.room_id),
            relay_pc_peer_id: Some(settings.pc_peer_id),
            relay_pc_public_key: Some(status.pc_public_key),
        });
    }

    let mut runtime_config = DesktopRuntimeConfig::isolated_test(&root);
    // Agents must be enabled for real session creation and runtime selection;
    // installation falls back to PATH binaries when downloads are unavailable.
    runtime_config.install_managed_adapters = true;
    let pairing_routes = gateway_config.pairing_routes.clone();
    runtime_config.remote_gateway = gateway_config;
    let runtime = DesktopRuntime::start(runtime_config)
        .await
        .expect("start desktop runtime");
    let db_path = runtime.config().database_path.clone();
    let gateway = runtime.remote().gateway();
    gateway
        .set_pairing_routes(pairing_routes)
        .expect("restore controlled E2E pairing routes after startup reconciliation");
    let desktop_identity = gateway.identity().expect("desktop identity");

    let mut relay_offer_candidate = None;
    let mut relay_bundle = Value::Null;
    if transport == "relay" {
        let relay_url = relay_url.expect("VIBEX_E2E_RELAY_URL is required for relay transport");
        let public_relay_url =
            public_relay_url.expect("VIBEX_E2E_PUBLIC_RELAY_URL is required for relay transport");
        let relay = runtime.relay();
        relay
            .update_settings(RelayClientSettingsUpdate {
                enabled: Some(true),
                relay_url: Some(Some(relay_url.clone())),
                ..RelayClientSettingsUpdate::default()
            })
            .await
            .expect("configure relay client");
        relay.start().await.expect("start relay client");
        for attempt in 0..200 {
            if relay.get_status().await.state == RelayClientConnectionState::Connected {
                break;
            }
            assert!(attempt < 199, "desktop relay client did not connect");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let settings = relay.get_settings().await;
        let status = relay.get_status().await;
        relay_offer_candidate = Some(RemotePairingCandidate {
            transport: RemotePairingTransport::SelfHostedRelay,
            url: relay_url.clone(),
            relay_room_id: Some(settings.room_id.clone()),
            relay_pc_peer_id: Some(settings.pc_peer_id.clone()),
            relay_pc_public_key: Some(status.pc_public_key.clone()),
        });
        relay_bundle = json!({
            "url": public_relay_url,
            "roomId": settings.room_id,
            "localPeerId": RelayPeerId::new(),
            "pcPeerId": settings.pc_peer_id,
            "pcPublicKey": status.pc_public_key,
        });
    }

    // Pair the client under test and write its credential bundle.
    let seed = ClientDeviceIdentity::generate(DeviceId::new()).expect("client identity");
    let connection = open_database(&db_path).expect("open for pairing");
    let offer = RemoteTrustService::create_pairing_offer(
        &connection,
        &desktop_identity,
        RemoteCreatePairingOfferRequest {
            permission_level: RemoteDevicePermissionLevel::FullControl,
            ttl_ms: Some(120_000),
            direct_candidates: if transport == "direct" {
                vec![direct_candidate.clone()]
            } else {
                Vec::new()
            },
            relay_candidate: relay_offer_candidate.clone(),
        },
    )
    .expect("create client pairing offer")
    .offer;
    let claim = pairing_claim_request(
        &offer,
        "Vibex E2E client",
        seed.public_key_base64(),
        vibex_core::RequestId::new().into_string(),
    )
    .expect("build claim request");
    let claimed =
        RemoteTrustService::claim_pairing_offer(&connection, claim).expect("claim client offer");
    drop(connection);

    let bundle = json!({
        "schemaVersion": "vibex-remote-client-credentials.v1",
        "record": {
            "serverUrl": public_url,
            "auth": {
                "deviceId": claimed.device.device_id,
                "authToken": claimed.device_grant_token,
            },
            "deviceIdentityPublicKey": seed.public_key_base64(),
            "serverIdentityPublicKey": desktop_identity.public_key_base64(),
        },
        "identityPrivateKey": seed.private_key_base64(),
        "expectedServerId": desktop_identity.server_id(),
        "clientType": client_type,
        "allowInsecureLocalDev": false,
        "route": {
            "directCandidates": if transport == "direct" { json!([public_url]) } else { json!([]) },
            "relay": relay_bundle,
        },
    });
    std::fs::write(&bundle_file, serde_json::to_vec_pretty(&bundle).unwrap())
        .expect("write credential bundle");

    let state = Arc::new(HarnessState {
        db_path,
        root: root.clone(),
        transport,
        runtime,
        client_device_id: claimed.device.device_id.clone(),
        disposable_offer_candidate: direct_candidate,
        fixture: tokio::sync::Mutex::new(None),
    });
    let router = Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/fixture/setup", post(fixture_setup))
        .route("/fixture/cleanup", post(fixture_cleanup))
        .route("/recovery/disconnect", post(recovery_disconnect))
        .route("/recovery/reconnect", post(recovery_reconnect))
        .with_state(state);
    let listener = TcpListener::bind(("127.0.0.1", control_port))
        .await
        .expect("bind control listener");
    println!("HARNESS_READY control=127.0.0.1:{control_port}");
    axum::serve(listener, router).await.expect("control server");
}

async fn fixture_setup(State(state): State<Arc<HarnessState>>) -> Json<Value> {
    let workspace_root = state.root.join("fixture-workspace");
    if !workspace_root.exists() {
        std::fs::create_dir_all(&workspace_root).expect("create fixture workspace");
        git(&workspace_root, &["init", "--initial-branch=main"]);
        git(&workspace_root, &["config", "user.name", "Vibex E2E"]);
        git(
            &workspace_root,
            &["config", "user.email", "e2e@vibex.invalid"],
        );
        std::fs::write(
            workspace_root.join("README.md"),
            "# Vibex E2E fixture workspace\n\nDisposable workspace for workflow E2E runs.\n",
        )
        .unwrap();
        std::fs::write(
            workspace_root.join("notes.txt"),
            "e2e fixture baseline line\n",
        )
        .unwrap();
        git(&workspace_root, &["add", "."]);
        git(&workspace_root, &["commit", "-m", "fixture baseline"]);
        // Leave one tracked file dirty so the Git surface has changes to
        // stage and commit during the workflow run.
        std::fs::write(
            workspace_root.join("notes.txt"),
            "e2e fixture baseline line\ne2e dirty change for git workflow\n",
        )
        .unwrap();
    }

    let connection = open_database(&state.db_path).expect("open for fixture");
    let (_, workspace) = WorkspaceRepository::ensure(
        &connection,
        workspace_root.to_str().unwrap(),
        WorkspaceMode::CurrentCheckout,
    )
    .expect("ensure fixture workspace");
    let existing = SessionRepository::list(&connection, false).expect("list sessions");
    let fixture_session_exists = existing
        .iter()
        .any(|session| session.workspace_id == workspace.id);
    if !fixture_session_exists {
        // Fresh homes have enabled ACP profiles without configured models, so
        // the runtime option catalog is empty until models are probed. Probe
        // through the real adapter and persist the result like the desktop
        // model selector would.
        let providers = state.runtime.providers().service();
        let manager = state.runtime.agent().manager();
        for profile in providers.list_profiles().expect("list provider profiles") {
            if profile.kind != vibex_core::ProviderKind::Acp
                || profile.status != vibex_core::ProviderProfileStatus::Enabled
                || !profile.configured_models.is_empty()
            {
                continue;
            }
            let Ok(models) = manager
                .list_models(vibex_core::AgentModelListRequest {
                    agent_id: Some(profile.agent_id.clone()),
                    provider_profile_id: Some(profile.id.clone()),
                    session_id: None,
                })
                .await
            else {
                continue;
            };
            if models.models.is_empty() {
                continue;
            }
            providers
                .update_profile(vibex_core::ProviderProfileUpdateRequest {
                    provider_profile_id: profile.id.clone(),
                    display_name: None,
                    status: None,
                    account_alias: None,
                    base_url: None,
                    default_model: models.models.first().cloned(),
                    small_model: None,
                    large_model: None,
                    configured_models: Some(
                        models
                            .models
                            .iter()
                            .map(|model| vibex_core::ProviderConfiguredModel {
                                id: model.clone(),
                                display_name: None,
                                enabled: true,
                                wire_api: None,
                                capabilities: Default::default(),
                            })
                            .collect(),
                    ),
                    reasoning_effort: None,
                    sandbox_defaults: None,
                    network_defaults: None,
                    permission_defaults: None,
                    provider_options: None,
                })
                .expect("configure probed models");
        }
        // Create the session through the real runtime path so runtime
        // selection state is initialized like any product session.
        let catalog = state
            .runtime
            .agent()
            .runtime_catalog()
            .list()
            .await
            .expect("list runtime options");
        let option = catalog
            .options
            .first()
            .expect("at least one runtime option");
        state
            .runtime
            .agent()
            .manager()
            .create_session(vibex_core::CreateAgentSessionRequest {
                runtime: option.selection.clone(),
                workspace_root: workspace.root_path.clone(),
                workspace_mode: workspace.mode,
                title: Some("E2E fixture session".to_string()),
                safety: Some(vibex_core::AgentSessionSafety::workspace_write_ask_on_risk()),
            })
            .await
            .expect("create fixture session");
    }

    let mut fixture = state.fixture.lock().await;
    if fixture.is_none() {
        let identity = state
            .runtime
            .remote()
            .gateway()
            .identity()
            .expect("desktop identity for fixture");
        let disposable_seed = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
        let offer = RemoteTrustService::create_pairing_offer(
            &connection,
            &identity,
            RemoteCreatePairingOfferRequest {
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                ttl_ms: Some(60_000),
                direct_candidates: vec![state.disposable_offer_candidate.clone()],
                relay_candidate: None,
            },
        )
        .expect("create disposable offer")
        .offer;
        let claim = pairing_claim_request(
            &offer,
            "E2E disposable device",
            disposable_seed.public_key_base64(),
            vibex_core::RequestId::new().into_string(),
        )
        .unwrap();
        let claimed = RemoteTrustService::claim_pairing_offer(&connection, claim)
            .expect("claim disposable offer");
        *fixture = Some(FixtureState {
            disposable_device_id: claimed.device.device_id,
        });
    }
    let disposable_device_id = fixture
        .as_ref()
        .map(|fixture| fixture.disposable_device_id.clone())
        .unwrap();
    drop(fixture);

    // The device list is ordered by `updated_at_ms DESC` and the connected E2E
    // client refreshes its own row on every authenticated request, so by the
    // time the Management surface fetches devices the client occupies index 0.
    // Report the index the disposable device will hold at that point.
    let devices = RemoteDeviceRepository::list(&connection).expect("list devices");
    let device_index = devices
        .iter()
        .filter(|record| record.detail.device_id != state.client_device_id)
        .position(|record| record.detail.device_id == disposable_device_id)
        .expect("disposable device present")
        + 1;

    Json(json!({
        "schemaVersion": "vibex-workflow-fixture.v1",
        "disposable": true,
        "workspaceIndex": 0,
        "sessionIndex": 0,
        "deviceIndex": device_index,
    }))
}

async fn fixture_cleanup(State(state): State<Arc<HarnessState>>) -> Json<Value> {
    let mut fixture = state.fixture.lock().await;
    if let Some(fixture_state) = fixture.take() {
        let connection = open_database(&state.db_path).expect("open for cleanup");
        let _ = RemoteTrustService::revoke_device(
            &connection,
            RemoteRevokeDeviceRequest {
                device_id: fixture_state.disposable_device_id,
                reason: Some("e2e fixture cleanup".to_string()),
            },
        );
    }
    Json(json!({ "ok": true }))
}

async fn recovery_disconnect(State(state): State<Arc<HarnessState>>) -> Json<Value> {
    if state.transport == "relay" {
        state
            .runtime
            .relay()
            .stop()
            .await
            .expect("stop relay client");
    } else {
        state
            .runtime
            .remote()
            .gateway()
            .stop()
            .await
            .expect("stop direct gateway");
    }
    Json(json!({ "ok": true }))
}

async fn recovery_reconnect(State(state): State<Arc<HarnessState>>) -> Json<Value> {
    if state.transport == "relay" {
        state
            .runtime
            .relay()
            .start()
            .await
            .expect("restart relay client");
    } else {
        state
            .runtime
            .remote()
            .gateway()
            .start()
            .await
            .expect("restart direct gateway")
            .expect("direct gateway address");
    }
    Json(json!({ "ok": true }))
}

#![cfg(all(feature = "e2e-test-support", not(target_family = "wasm")))]

//! Long-running product-pairing harness launched by
//! `scripts/e2e-local-env/run-product-pairing.mjs`.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use gpui::TestApp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc, oneshot},
};
use vibex_core::{
    AgentCommandConfig, AgentRuntimeKind, DeviceId, RemoteAuditListRequest, RemoteAuditOutcome,
    RemoteDevicePermissionLevel, RemoteDeviceStatus, RemoteRevokeDeviceRequest, WorkspaceMode,
};
use vibex_db::{
    RemoteAuditRepository, RemoteDeviceRepository, SessionRepository, WorkspaceRepository,
    open_database,
};
use vibex_desktop::remote_access_pairing::{
    RemoteAccessPairingE2eAction, RemoteAccessPairingE2eDriver, RemoteAccessPairingE2eSnapshot,
};
use vibex_desktop_runtime::{
    DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade, RemoteConnectivityMethod,
};
use vibex_remote::RemoteTrustService;
use vibex_remote_client::{ClientDeviceIdentity, pairing_claim_request};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(90);
const FIXTURE_DEVICE_NAME: &str = "Vibex E2E disposable device";
const FIXTURE_ACP: &str = include_str!("fixtures/product_pairing_acp.py");

fn env_required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

fn sha256(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

fn device_identity_sha256(device_id: &DeviceId) -> String {
    sha256(device_id.as_str())
}

fn git(root: &Path, args: &[&str]) -> Result<(), ApiError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .map_err(|_| ApiError::internal("fixture_git_launch_failed"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ApiError::internal("fixture_git_command_failed"))
    }
}

fn install_deterministic_acp_fixture(runtime: &DesktopRuntime, root: &Path) {
    let fixture_path = root.join("product-pairing-acp.py");
    std::fs::write(&fixture_path, FIXTURE_ACP).expect("write deterministic ACP fixture");
    let service = runtime.providers().service();
    for definition in vibex_core::builtin_agent_definitions()
        .into_iter()
        .filter(|definition| definition.runtime_kind == AgentRuntimeKind::Acp)
    {
        service
            .reconcile_agent_acp_runtime(
                definition.id,
                AgentCommandConfig {
                    command: "python3".to_string(),
                    args: vec![fixture_path.to_string_lossy().into_owned()],
                },
            )
            .expect("configure deterministic ACP fixture");
    }
}

enum PairingControlRequest {
    Snapshot {
        reply: oneshot::Sender<RemoteAccessPairingE2eSnapshot>,
    },
    Action {
        action: RemoteAccessPairingE2eAction,
        reply: oneshot::Sender<Result<RemoteAccessPairingE2eSnapshot, String>>,
    },
    CopyLink {
        reply: oneshot::Sender<Result<String, String>>,
    },
}

#[derive(Clone)]
struct HarnessState {
    pairing: mpsc::UnboundedSender<PairingControlRequest>,
    runtime: Arc<DesktopRuntime>,
    root: PathBuf,
    db_path: PathBuf,
    recovery_method: RemoteConnectivityMethod,
    fixture: Arc<Mutex<Option<FixtureState>>>,
    shutdown_requested: Arc<AtomicBool>,
}

struct FixtureState {
    disposable_device_id: DeviceId,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
}

impl ApiError {
    fn bad_request(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
        }
    }

    fn internal(code: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
        }
    }

    fn unavailable(code: &'static str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(json!({ "ok": false, "errorCode": self.code })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[test]
#[ignore = "long-running product pairing E2E harness"]
fn product_pairing_harness() {
    let root = PathBuf::from(env_required("VIBEX_E2E_ROOT"));
    let control_port = env_required("VIBEX_E2E_CONTROL_PORT")
        .parse::<u16>()
        .expect("VIBEX_E2E_CONTROL_PORT must be a port");
    let recovery_method = match env_required("VIBEX_E2E_TRANSPORT").as_str() {
        "tailscale" => RemoteConnectivityMethod::TailscaleServe,
        "direct" | "direct-relay-fallback" => RemoteConnectivityMethod::Direct,
        "relay" | "relay-no-tailscale" => RemoteConnectivityMethod::SelfHostedRelay,
        _ => panic!("VIBEX_E2E_TRANSPORT is invalid"),
    };

    if !env_flag("VIBEX_E2E_PRESERVE_ROOT") {
        let _ = std::fs::remove_dir_all(&root);
    }
    std::fs::create_dir_all(&root).expect("create harness root");
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("create harness Tokio runtime");
    let mut runtime_config = DesktopRuntimeConfig::isolated_test(&root);
    runtime_config.install_managed_adapters = true;
    let runtime = tokio_runtime
        .block_on(DesktopRuntime::start(runtime_config))
        .expect("start Desktop runtime");
    install_deterministic_acp_fixture(&runtime, &root);

    let mut app = TestApp::new();
    let tokio_handle = tokio_runtime.handle().clone();
    app.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init_from_handle(cx, tokio_handle);
    });
    let mut window = app
        .open_window(|window, cx| RemoteAccessPairingE2eDriver::new(runtime.clone(), window, cx));

    let (pairing_tx, mut pairing_rx) = mpsc::unbounded_channel();
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let state = HarnessState {
        pairing: pairing_tx,
        runtime: runtime.clone(),
        root: root.clone(),
        db_path: runtime.config().database_path.clone(),
        recovery_method,
        fixture: Arc::new(Mutex::new(None)),
        shutdown_requested: shutdown_requested.clone(),
    };
    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_for_server = ready.clone();
    tokio_runtime.spawn(async move {
        let router = Router::new()
            .route("/health", get(health))
            .route("/pairing/snapshot", get(pairing_snapshot))
            .route("/pairing/action", post(pairing_action))
            .route("/pairing/link", post(pairing_link))
            .route("/fixture/setup", post(fixture_setup))
            .route("/fixture/cleanup", post(fixture_cleanup))
            .route("/trust/summary", get(trust_summary))
            .route("/trust/revoke", post(trust_revoke))
            .route("/identity/summary", get(identity_summary))
            .route("/recovery/disconnect", post(recovery_disconnect))
            .route("/recovery/reconnect", post(recovery_reconnect))
            .route("/lifecycle/shutdown", post(lifecycle_shutdown))
            .with_state(state);
        let listener = TcpListener::bind(("127.0.0.1", control_port))
            .await
            .expect("bind control listener");
        ready_for_server.store(true, std::sync::atomic::Ordering::Release);
        axum::serve(listener, router)
            .await
            .expect("serve harness control API");
    });

    while !ready.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("PRODUCT_PAIRING_HARNESS_READY");

    while !shutdown_requested.load(Ordering::Acquire) {
        while let Ok(request) = pairing_rx.try_recv() {
            match request {
                PairingControlRequest::Snapshot { reply } => {
                    let snapshot = window.read(|driver, cx| driver.snapshot(cx));
                    let _ = reply.send(snapshot);
                }
                PairingControlRequest::Action { action, reply } => {
                    let result = window
                        .update(|driver, window, cx| driver.dispatch(action, window, cx))
                        .map(|_| window.read(|driver, cx| driver.snapshot(cx)))
                        .map_err(|error| error.code);
                    let _ = reply.send(result);
                }
                PairingControlRequest::CopyLink { reply } => {
                    let result = window
                        .update(|driver, _, cx| driver.copy_pairing_link_once(cx))
                        .map_err(|error| error.code);
                    let _ = reply.send(result);
                }
            }
        }
        app.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    tokio_runtime
        .block_on(runtime.shutdown())
        .expect("shutdown Desktop runtime");
}

async fn request_snapshot(state: &HarnessState) -> ApiResult<RemoteAccessPairingE2eSnapshot> {
    let (reply, receiver) = oneshot::channel();
    state
        .pairing
        .send(PairingControlRequest::Snapshot { reply })
        .map_err(|_| ApiError::unavailable("pairing_driver_unavailable"))?;
    tokio::time::timeout(CONTROL_TIMEOUT, receiver)
        .await
        .map_err(|_| ApiError::unavailable("pairing_driver_timeout"))?
        .map_err(|_| ApiError::unavailable("pairing_driver_unavailable"))
}

async fn request_action(
    state: &HarnessState,
    action: RemoteAccessPairingE2eAction,
) -> ApiResult<RemoteAccessPairingE2eSnapshot> {
    let (reply, receiver) = oneshot::channel();
    state
        .pairing
        .send(PairingControlRequest::Action { action, reply })
        .map_err(|_| ApiError::unavailable("pairing_driver_unavailable"))?;
    tokio::time::timeout(CONTROL_TIMEOUT, receiver)
        .await
        .map_err(|_| ApiError::unavailable("pairing_action_timeout"))?
        .map_err(|_| ApiError::unavailable("pairing_driver_unavailable"))?
        .map_err(|_| ApiError::bad_request("pairing_action_rejected"))
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "schemaVersion": "remote-access-pairing-harness.v1",
        "ready": true
    }))
}

async fn pairing_snapshot(
    State(state): State<HarnessState>,
) -> ApiResult<Json<RemoteAccessPairingE2eSnapshot>> {
    Ok(Json(request_snapshot(&state).await?))
}

async fn pairing_action(
    State(state): State<HarnessState>,
    Json(action): Json<RemoteAccessPairingE2eAction>,
) -> ApiResult<Json<RemoteAccessPairingE2eSnapshot>> {
    Ok(Json(request_action(&state, action).await?))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingLinkResponse {
    schema_version: &'static str,
    value: String,
}

async fn pairing_link(State(state): State<HarnessState>) -> ApiResult<impl IntoResponse> {
    let (reply, receiver) = oneshot::channel();
    state
        .pairing
        .send(PairingControlRequest::CopyLink { reply })
        .map_err(|_| ApiError::unavailable("pairing_driver_unavailable"))?;
    let value = tokio::time::timeout(CONTROL_TIMEOUT, receiver)
        .await
        .map_err(|_| ApiError::unavailable("pairing_link_timeout"))?
        .map_err(|_| ApiError::unavailable("pairing_driver_unavailable"))?
        .map_err(|_| ApiError::bad_request("pairing_link_unavailable"))?;
    Ok((
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
        ],
        Json(PairingLinkResponse {
            schema_version: "remote-access-pairing-link.v1",
            value,
        }),
    ))
}

async fn fixture_setup(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    prepare_workspace_fixture(&state).await?;
    let mut fixture = state.fixture.lock().await;
    if fixture.is_none() {
        let connection = open_database(&state.db_path)
            .map_err(|_| ApiError::internal("fixture_database_open_failed"))?;
        let existing = RemoteDeviceRepository::list(&connection)
            .map_err(|_| ApiError::internal("fixture_device_list_failed"))?
            .into_iter()
            .find(|record| {
                record.detail.display_name == FIXTURE_DEVICE_NAME
                    && record.detail.status == RemoteDeviceStatus::Active
            })
            .map(|record| record.detail.device_id);
        let disposable_device_id = if let Some(device_id) = existing {
            device_id
        } else {
            drop(connection);
            let response = state
                .runtime
                .remote_connectivity()
                .create_pairing_offer(RemoteDevicePermissionLevel::ReadOnly, 60_000)
                .map_err(|_| ApiError::internal("fixture_offer_failed"))?;
            let seed = ClientDeviceIdentity::generate(DeviceId::new())
                .map_err(|_| ApiError::internal("fixture_identity_failed"))?;
            let claim = pairing_claim_request(
                &response.offer,
                FIXTURE_DEVICE_NAME,
                seed.public_key_base64(),
                vibex_core::RequestId::new().into_string(),
            )
            .map_err(|_| ApiError::internal("fixture_claim_build_failed"))?;
            let connection = open_database(&state.db_path)
                .map_err(|_| ApiError::internal("fixture_database_open_failed"))?;
            RemoteTrustService::claim_pairing_offer(&connection, claim)
                .map_err(|_| ApiError::internal("fixture_claim_failed"))?
                .device
                .device_id
        };
        *fixture = Some(FixtureState {
            disposable_device_id,
        });
    }
    let disposable_device_id = fixture
        .as_ref()
        .expect("fixture initialized")
        .disposable_device_id
        .clone();
    let connection = open_database(&state.db_path)
        .map_err(|_| ApiError::internal("fixture_database_open_failed"))?;
    let device_index = RemoteDeviceRepository::list(&connection)
        .map_err(|_| ApiError::internal("fixture_device_list_failed"))?
        .iter()
        .position(|record| record.detail.device_id == disposable_device_id)
        .ok_or_else(|| ApiError::internal("fixture_device_missing"))?;
    Ok(Json(json!({
        "schemaVersion": "vibex-workflow-fixture.v1",
        "disposable": true,
        "workspaceIndex": 0,
        "sessionIndex": 0,
        "deviceIndex": device_index
    })))
}

async fn prepare_workspace_fixture(state: &HarnessState) -> ApiResult<()> {
    let workspace_root = state.root.join("fixture-workspace");
    if !workspace_root.exists() {
        std::fs::create_dir_all(&workspace_root)
            .map_err(|_| ApiError::internal("fixture_workspace_create_failed"))?;
        git(&workspace_root, &["init", "--initial-branch=main"])?;
        git(&workspace_root, &["config", "user.name", "Vibex E2E"])?;
        git(
            &workspace_root,
            &["config", "user.email", "e2e@vibex.invalid"],
        )?;
        std::fs::write(
            workspace_root.join("README.md"),
            "# Vibex E2E fixture workspace\n\nDisposable workspace for workflow E2E runs.\n",
        )
        .map_err(|_| ApiError::internal("fixture_workspace_write_failed"))?;
        std::fs::write(
            workspace_root.join("notes.txt"),
            "e2e fixture baseline line\n",
        )
        .map_err(|_| ApiError::internal("fixture_workspace_write_failed"))?;
        git(&workspace_root, &["add", "."])?;
        git(&workspace_root, &["commit", "-m", "fixture baseline"])?;
        std::fs::write(
            workspace_root.join("notes.txt"),
            "e2e fixture baseline line\ne2e dirty change for git workflow\n",
        )
        .map_err(|_| ApiError::internal("fixture_workspace_write_failed"))?;
    }

    let connection = open_database(&state.db_path)
        .map_err(|_| ApiError::internal("fixture_database_open_failed"))?;
    let (_, workspace) = WorkspaceRepository::ensure(
        &connection,
        workspace_root
            .to_str()
            .ok_or_else(|| ApiError::internal("fixture_workspace_path_invalid"))?,
        WorkspaceMode::CurrentCheckout,
    )
    .map_err(|_| ApiError::internal("fixture_workspace_ensure_failed"))?;
    let sessions = SessionRepository::list(&connection, false)
        .map_err(|_| ApiError::internal("fixture_session_list_failed"))?;
    if sessions
        .iter()
        .any(|session| session.workspace_id == workspace.id)
    {
        return Ok(());
    }
    drop(connection);

    let providers = state.runtime.providers().service();
    let manager = state.runtime.agent().manager();
    for profile in providers
        .list_profiles()
        .map_err(|_| ApiError::internal("fixture_provider_list_failed"))?
    {
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
                provider_profile_id: profile.id,
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
                        })
                        .collect(),
                ),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })
            .map_err(|_| ApiError::internal("fixture_provider_update_failed"))?;
    }
    let catalog = state
        .runtime
        .agent()
        .runtime_catalog()
        .list()
        .await
        .map_err(|_| ApiError::internal("fixture_runtime_catalog_failed"))?;
    let option = catalog
        .options
        .first()
        .ok_or_else(|| ApiError::internal("fixture_runtime_option_missing"))?;
    manager
        .create_session(vibex_core::CreateAgentSessionRequest {
            runtime: option.selection.clone(),
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            title: Some("E2E fixture session".to_string()),
            safety: Some(vibex_core::AgentSessionSafety::workspace_write_ask_on_risk()),
        })
        .await
        .map_err(|_| ApiError::internal("fixture_session_create_failed"))?;
    Ok(())
}

async fn fixture_cleanup(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    let mut fixture = state.fixture.lock().await;
    if let Some(fixture) = fixture.take() {
        let connection = open_database(&state.db_path)
            .map_err(|_| ApiError::internal("fixture_database_open_failed"))?;
        RemoteTrustService::revoke_device(
            &connection,
            RemoteRevokeDeviceRequest {
                device_id: fixture.disposable_device_id,
                reason: Some("e2e fixture cleanup".to_string()),
            },
        )
        .map_err(|_| ApiError::internal("fixture_device_cleanup_failed"))?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn trust_summary(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    let connection = open_database(&state.db_path)
        .map_err(|_| ApiError::internal("trust_database_open_failed"))?;
    let devices = RemoteDeviceRepository::list(&connection)
        .map_err(|_| ApiError::internal("trust_device_list_failed"))?;
    let active = devices
        .iter()
        .filter(|record| record.detail.status == RemoteDeviceStatus::Active)
        .count();
    let revoked = devices.len().saturating_sub(active);
    let active_device_identity_sha256 = devices
        .iter()
        .filter(|record| record.detail.status == RemoteDeviceStatus::Active)
        .map(|record| device_identity_sha256(&record.detail.device_id))
        .collect::<Vec<_>>();
    let audits = RemoteAuditRepository::list(
        &connection,
        &RemoteAuditListRequest {
            device_id: None,
            limit: Some(500),
        },
    )
    .map_err(|_| ApiError::internal("trust_audit_list_failed"))?;
    let denied = audits
        .iter()
        .filter(|record| record.outcome == RemoteAuditOutcome::Denied)
        .count();
    Ok(Json(json!({
        "schemaVersion": "remote-access-trust-summary.v1",
        "deviceCount": devices.len(),
        "activeDeviceCount": active,
        "revokedDeviceCount": revoked,
        "activeDeviceIdentitySha256": active_device_identity_sha256,
        "auditCount": audits.len(),
        "deniedAuditCount": denied
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustRevokeRequest {
    device_identity_sha256: String,
}

async fn trust_revoke(
    State(state): State<HarnessState>,
    Json(request): Json<TrustRevokeRequest>,
) -> ApiResult<Json<Value>> {
    if request.device_identity_sha256.len() != 64
        || !request
            .device_identity_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ApiError::bad_request("trust_device_hash_invalid"));
    }
    let connection = open_database(&state.db_path)
        .map_err(|_| ApiError::internal("trust_database_open_failed"))?;
    let device = RemoteDeviceRepository::list(&connection)
        .map_err(|_| ApiError::internal("trust_device_list_failed"))?
        .into_iter()
        .find(|record| {
            record.detail.status == RemoteDeviceStatus::Active
                && device_identity_sha256(&record.detail.device_id)
                    == request.device_identity_sha256
        })
        .ok_or_else(|| ApiError::bad_request("trust_device_not_found"))?;
    state
        .runtime
        .remote()
        .revoke_device(RemoteRevokeDeviceRequest {
            device_id: device.detail.device_id,
            reason: Some("e2e exact device cleanup".to_string()),
        })
        .map_err(|_| ApiError::internal("trust_device_revoke_failed"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn identity_summary(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    let identity = state
        .runtime
        .remote()
        .gateway()
        .identity()
        .map_err(|_| ApiError::internal("identity_unavailable"))?;
    Ok(Json(json!({
        "schemaVersion": "remote-access-identity-summary.v1",
        "serverIdentitySha256": sha256(format!(
            "{}:{}",
            identity.server_id(),
            identity.public_key_base64()
        ))
    })))
}

async fn recovery_disconnect(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    request_action(
        &state,
        RemoteAccessPairingE2eAction::DisableMethod {
            method: state.recovery_method,
        },
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn recovery_reconnect(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    request_action(
        &state,
        RemoteAccessPairingE2eAction::EnableMethod {
            method: state.recovery_method,
        },
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn lifecycle_shutdown(State(state): State<HarnessState>) -> ApiResult<Json<Value>> {
    let requested = state.shutdown_requested.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        requested.store(true, Ordering::Release);
    });
    Ok(Json(json!({ "ok": true })))
}

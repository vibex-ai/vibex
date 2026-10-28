//! End-to-end coverage: a real loopback listener, a real upstream, and the
//! seeded local Provider Profiles.
//!
//! The mock upstream is a second axum server on loopback. Each test configures
//! one shared Provider Profile whose Key is stored through the same secret
//! store the runtime uses, points the gateway at it, and drives a full call.
//! That exercises the parts unit tests cannot: the shape of the outbound
//! request, the auth header, the streamed frames, and the ledger row.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use vibex_config_switch::ProviderConfigService;
use vibex_config_switch::secrets;
use vibex_core::{
    AgentCredential, AgentCredentialStatus, GatewayConversationPin, GatewayFailureKind,
    GatewayPolicy, GatewayRequestOutcome, GatewayTokenWindow, ModelProviderAccountState,
    ModelProviderCatalogEntry, ModelProviderCredentialReference, ModelProviderEndpoint,
    ModelProviderEndpointKind, ModelProviderProfileId, ModelProviderProfileStatus,
    ModelProviderProxyPolicy, ProjectionSecretReference, ProviderSecretSetupState, RequestId,
};
use vibex_db::{
    GatewayConversationPinRepository, GatewayTokenWindowRepository,
    ModelGatewayRequestLogRepository, ModelProviderAccountStateRepository, apply_migrations,
    open_database,
};
use vibex_model_gateway::{
    GatewayLimits, ModelGatewayService, WireProtocol, build_router, classify_failure,
};

/// What the mock upstream recorded.
#[derive(Debug, Default)]
struct Recorded {
    path: String,
    authorization: Option<String>,
    api_key: Option<String>,
    body: Option<Value>,
}

/// How many requests the upstream saw overlapping at once.
///
/// This is the only honest way to test a concurrency limit: counting from the
/// client side measures the test's own parallelism, not what left the gateway.
#[derive(Debug, Default)]
struct Overlap {
    current: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

impl Overlap {
    fn enter(&self) {
        let now = self
            .current
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.peak
            .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
    }

    fn leave(&self) {
        self.current
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }
}

type Recorder = Arc<Mutex<Recorded>>;

#[derive(Clone)]
struct MockState {
    recorder: Recorder,
    overlap: Arc<Overlap>,
    /// The status the mock answers with.
    status: u16,
    /// The SSE body the mock answers with.
    stream: String,
    /// Whether the mock answers a non-streaming request with JSON.
    json: Option<Value>,
    /// One Authorization header the mock refuses, and with which status, so a
    /// test can make the first Key fail while the next one answers.
    refuse: Option<(String, u16)>,
    /// Write the SSE body one frame per TCP write instead of as one body.
    ///
    /// A single write usually reaches the gateway as a single chunk, which
    /// hides any per-chunk decoder state. Real vendors flush each frame as it
    /// is produced, so a frame boundary is the normal case and has to be
    /// covered.
    frame_by_frame: bool,
}

async fn mock_handler(
    State(state): State<MockState>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    state.overlap.enter();
    // Hold the request open long enough for a second one to overlap if the
    // gateway really sent it.
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    {
        let mut recorded = state.recorder.lock().await;
        recorded.path = uri.path().to_string();
        recorded.authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        recorded.api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        recorded.body = serde_json::from_slice(&body).ok();
    }
    let refused_status = state.refuse.as_ref().and_then(|(refused, status)| {
        (headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some(refused.as_str()))
        .then_some(*status)
    });
    if let Some(status) = refused_status.or((state.status != 200).then_some(state.status)) {
        state.overlap.leave();
        let mut response = Response::new(Body::from(
            json!({"error": {"message": "the upstream refused this key"}}).to_string(),
        ));
        *response.status_mut() = axum::http::StatusCode::from_u16(status).unwrap();
        return response;
    }
    if let Some(body) = state.json {
        state.overlap.leave();
        let mut response = Response::new(Body::from(body.to_string()));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        return response;
    }
    state.overlap.leave();
    let mut response = if state.frame_by_frame {
        // A delay between frames is what makes them separate reads on the
        // client side; without it the transport coalesces the whole body into
        // one chunk and any per-chunk state bug stays hidden.
        let frames = state
            .stream
            .split_inclusive("\n\n")
            .filter(|frame| !frame.trim().is_empty())
            .map(|frame| axum::body::Bytes::from(frame.to_string()))
            .collect::<Vec<_>>();
        let body = futures_util::stream::unfold(
            (frames.into_iter(), false),
            |(mut frames, first)| async move {
                if !first {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                frames
                    .next()
                    .map(|frame| (Ok::<_, std::io::Error>(frame), (frames, false)))
            },
        );
        Response::new(Body::from_stream(body))
    } else {
        Response::new(Body::from(state.stream))
    };
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response
}

async fn mock_upstream(
    status: u16,
    stream: String,
    json: Option<Value>,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    let (url, recorder, _overlap, task) = mock_upstream_with_overlap(status, stream, json).await;
    (url, recorder, task)
}

/// An upstream that accepts the request and then stays silent.
///
/// The headers arrive immediately, so the gateway commits to the connection;
/// no body byte arrives until long after any first-token deadline. This is the
/// shape of a vendor that accepts and stalls, which no error-based failover
/// can see.
async fn mock_silent_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let body = Body::from_stream(futures_util::stream::once(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: {}\n\n"))
            }));
            Response::new(body)
        }),
    );
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), task)
}

/// An upstream that refuses the first `failures` attempts and then answers.
///
/// Patience is a property of repeated attempts, so the mock has to know how
/// many times it has been called.
async fn mock_flaky_upstream(
    failures: usize,
) -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter);
            async move {
                let seen = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if seen < failures {
                    let mut response = Response::new(Body::from(
                        json!({"error": {"message": "try again"}}).to_string(),
                    ));
                    *response.status_mut() = axum::http::StatusCode::SERVICE_UNAVAILABLE;
                    return response;
                }
                Response::new(Body::from(chat_stream()))
            }
        }),
    );
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), attempts, task)
}

/// A mock that refuses one Key and serves every other one normally.
///
/// A failover needs two different answers from the same upstream, which a
/// fixed status cannot express.
async fn mock_upstream_refusing(
    refused_authorization: &str,
    stream: String,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    mock_upstream_refusing_with_status(refused_authorization, 429, stream).await
}

async fn mock_upstream_refusing_with_status(
    refused_authorization: &str,
    status: u16,
    stream: String,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    let (url, recorder, _overlap, task) =
        mock_upstream_with_refusal(refused_authorization, status, stream).await;
    (url, recorder, task)
}

async fn mock_upstream_with_refusal(
    refused_authorization: &str,
    refusal_status: u16,
    stream: String,
) -> (String, Recorder, Arc<Overlap>, tokio::task::JoinHandle<()>) {
    let recorder: Recorder = Arc::new(Mutex::new(Recorded::default()));
    let overlap = Arc::new(Overlap::default());
    let state = MockState {
        recorder: Arc::clone(&recorder),
        overlap: Arc::clone(&overlap),
        status: 200,
        stream,
        json: None,
        refuse: Some((refused_authorization.to_string(), refusal_status)),
        frame_by_frame: false,
    };
    let router = Router::new()
        .route("/{*path}", post(mock_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), recorder, overlap, task)
}

async fn mock_upstream_with_overlap(
    status: u16,
    stream: String,
    json: Option<Value>,
) -> (String, Recorder, Arc<Overlap>, tokio::task::JoinHandle<()>) {
    mock_upstream_flushing(status, stream, json, false).await
}

/// A mock that flushes every SSE frame as its own body chunk.
async fn mock_upstream_frame_by_frame(
    stream: String,
) -> (String, Recorder, tokio::task::JoinHandle<()>) {
    let (url, recorder, _overlap, task) = mock_upstream_flushing(200, stream, None, true).await;
    (url, recorder, task)
}

async fn mock_upstream_flushing(
    status: u16,
    stream: String,
    json: Option<Value>,
    frame_by_frame: bool,
) -> (String, Recorder, Arc<Overlap>, tokio::task::JoinHandle<()>) {
    let recorder: Recorder = Arc::new(Mutex::new(Recorded::default()));
    let overlap = Arc::new(Overlap::default());
    let state = MockState {
        recorder: Arc::clone(&recorder),
        overlap: Arc::clone(&overlap),
        status,
        stream,
        json,
        refuse: None,
        frame_by_frame,
    };
    let router = Router::new()
        .route("/{*path}", post(mock_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), recorder, overlap, task)
}

fn database_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vibex-gateway-it-{}.db", RequestId::new().as_str()))
}

/// Creates a shared Provider Profile with one stored Key.
/// Keeps the integration suite off the OS keychain.
///
/// A headless test process has no Secret Service session, and a keychain call
/// can block waiting for one. The host-file store is the same code path a
/// headless server uses, so the tests stay honest and deterministic.
/// Serializes host-file secret writes.
///
/// The host-file store rewrites one JSON document, so two test threads writing
/// at the same time race on the same temporary file. The lock covers the write
/// only; reads of the renamed document stay concurrent.
static TEST_SECRET_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn configure_test_secrets() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let root =
            std::env::temp_dir().join(format!("vibex-gateway-test-secrets-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        secrets::configure_provider_secret_store(
            vibex_config_switch::secrets::ProviderSecretStoreConfig::host_file(root),
        );
    });
}

fn seed_upstream(
    service: &ProviderConfigService,
    endpoint: &str,
    protocol: WireProtocol,
    models: &[&str],
    api_key: &str,
) -> ModelProviderProfileId {
    configure_test_secrets();
    let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
    {
        let _guard = TEST_SECRET_LOCK.lock().unwrap();
        secrets::store_provider_secret(&lookup_key, api_key).unwrap();
    }
    let credential = ModelProviderCredentialReference {
        id: RequestId::new(),
        display_name: "Primary".to_string(),
        status: AgentCredentialStatus::Ready,
        credential: AgentCredential::ApiKey {
            secret: ProjectionSecretReference {
                id: RequestId::new(),
                backend: secrets::provider_secret_write_backend(),
                setup_state: ProviderSecretSetupState::Available,
                lookup_key,
                redacted_hint: "stored".to_string(),
                revision: 1,
                legacy_secret_reference_id: None,
            },
            target_hint: None,
        },
        revision: 1,

        models: Vec::new(),
        proxy_url: None,
    };
    let profile = vibex_core::ModelProviderProfileCreateRequest {
        display_name: "DeepSeek".to_string(),
        vendor_hint: Some("deepseek".to_string()),
        endpoints: vec![ModelProviderEndpoint {
            id: "endpoint-api".to_string(),
            kind: ModelProviderEndpointKind::Api,
            url: endpoint.to_string(),
            wire_protocol_id: Some(protocol.wire_protocol_id().to_string()),
        }],
        proxy_policy: ModelProviderProxyPolicy::InheritSystem,
        credentials: vec![credential],
        configured_models: models
            .iter()
            .map(|model| ModelProviderCatalogEntry {
                id: (*model).to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            })
            .collect(),
        default_model_id: models.first().map(|model| (*model).to_string()),
        headers: Vec::new(),
        status: ModelProviderProfileStatus::Enabled,
    };
    service.create_model_provider_profile(profile).unwrap().id
}

/// Configures the gateway and puts one Provider behind it in a group.
///
/// The gateway has no upstream list of its own, so a request can only name a
/// model through a group. Returns that group's id, which is what a model id is
/// built from.
fn enable_gateway_at(path: &std::path::Path, policy: GatewayPolicy) {
    let mut connection = open_database(path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let settings = vibex_db::ModelGatewaySettingsRepository::get_or_create(&connection).unwrap();
    let mut next = settings.clone();
    next.revision = settings.revision + 1;
    next.policy = policy;
    vibex_db::ModelGatewaySettingsRepository::update(&connection, &next, settings.revision)
        .unwrap();
}

/// Configures the gateway and puts one Provider behind it in a group that
/// publishes that Provider's default model.
fn point_gateway_at(
    path: &std::path::Path,
    upstream: ModelProviderProfileId,
    policy: GatewayPolicy,
) -> vibex_core::ModelRouteId {
    enable_gateway_at(path, policy);
    let connection = open_database(path).unwrap();
    let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
        .unwrap()
        .expect("the upstream exists");
    let model = profile
        .default_model_id
        .filter(|model| !model.trim().is_empty())
        .or_else(|| {
            profile
                .configured_models
                .iter()
                .find(|model| model.enabled)
                .map(|model| model.id.clone())
        })
        .unwrap_or_else(|| "deepseek-chat".to_string());
    let route = vibex_core::ModelRoute {
        display_name: model.clone(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some(model.clone()),
        status: vibex_core::RouteStatus::Enabled,
        members: vec![vibex_core::ModelRouteMember::new(upstream, model, 0)],
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    route.id
}

/// Seeds a Provider that publishes one address and declares no protocol.
///
/// This is the shape a hand-entered "default API base URL" produces, and the
/// shape that broke: the gateway treated "no declaration" as "speaks all four".
fn seed_untyped_upstream(
    service: &ProviderConfigService,
    endpoint: &str,
    models: &[&str],
    api_key: &str,
) -> ModelProviderProfileId {
    configure_test_secrets();
    let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
    {
        let _guard = TEST_SECRET_LOCK.lock().unwrap();
        secrets::store_provider_secret(&lookup_key, api_key).unwrap();
    }
    let credential = ModelProviderCredentialReference {
        id: RequestId::new(),
        display_name: "Primary".to_string(),
        status: AgentCredentialStatus::Ready,
        credential: AgentCredential::ApiKey {
            secret: ProjectionSecretReference {
                id: RequestId::new(),
                backend: secrets::provider_secret_write_backend(),
                setup_state: ProviderSecretSetupState::Available,
                lookup_key,
                redacted_hint: "stored".to_string(),
                revision: 1,
                legacy_secret_reference_id: None,
            },
            target_hint: None,
        },
        revision: 1,

        models: Vec::new(),
        proxy_url: None,
    };
    let profile = vibex_core::ModelProviderProfileCreateRequest {
        display_name: "Untyped".to_string(),
        vendor_hint: None,
        endpoints: vec![ModelProviderEndpoint {
            id: "api".to_string(),
            kind: ModelProviderEndpointKind::Api,
            url: endpoint.to_string(),
            wire_protocol_id: None,
        }],
        proxy_policy: ModelProviderProxyPolicy::InheritSystem,
        credentials: vec![credential],
        configured_models: models
            .iter()
            .map(|model| ModelProviderCatalogEntry {
                id: (*model).to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            })
            .collect(),
        default_model_id: models.first().map(|model| (*model).to_string()),
        headers: Vec::new(),
        status: ModelProviderProfileStatus::Enabled,
    };
    service.create_model_provider_profile(profile).unwrap().id
}

/// The model id a session selects from a group.
fn group_model(group: &vibex_core::ModelRouteId, model: &str) -> String {
    format!("group/{}/{}", group.as_str(), model)
}

async fn gateway(
    path: &std::path::Path,
) -> (
    Arc<ModelGatewayService>,
    String,
    tokio::task::JoinHandle<()>,
) {
    let config = ProviderConfigService::new(path);
    let service = ModelGatewayService::new(path, config, GatewayLimits::default());
    let state = service.start().await.unwrap();
    assert_eq!(state.status, vibex_core::ModelGatewayStatus::Running);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let router = build_router(Arc::clone(&service));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (service, format!("http://{address}"), task)
}

fn cleanup(path: std::path::PathBuf) {
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

fn chat_stream() -> String {
    [
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{"role":"assistant","content":""}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{"content":"hello"}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{"content":" world"}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        "data: [DONE]",
    ]
    .join("\n\n")
        + "\n\n"
}

/// A Gemini `streamGenerateContent` reply.
fn gemini_stream() -> String {
    [
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"hello"}]},"index":0}]}"#,
        r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":" world"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":3}}"#,
    ]
    .join("\n\n")
        + "\n\n"
}

fn anthropic_stream() -> String {
    [
        r#"event: message_start
data: {"type":"message_start","message":{"id":"m1","model":"claude","usage":{"input_tokens":9}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ]
    .join("\n\n")
        + "\n\n"
}

/// A minimal Responses stream: one created event, one text delta, one end.
///
/// This is what the upstream of a route group sends when the Agent on the
/// other side speaks Anthropic, so the gateway has to translate it.
fn responses_stream() -> String {
    [
        r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_1","model":"gpt-5","status":"in_progress","output":[]}}"#,
        r#"event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"id":"msg_0","type":"message","role":"assistant","status":"in_progress","content":[]}}"#,
        r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_0","output_index":0,"content_index":0,"delta":"hello"}"#,
        r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","model":"gpt-5","status":"completed","output":[],"usage":{"input_tokens":9,"output_tokens":1}}}"#,
    ]
    .join("\n\n")
        + "\n\n"
}

/// A listener with no route group is running, not unavailable.
///
/// The gateway owns no Provider list, so "no upstreams" is the ordinary first
/// launch of a fresh installation rather than a configuration error. It must
/// bind, serve an empty model list, and refuse a model it has nothing for —
/// never report itself broken or delete its own bindings.
#[tokio::test]
async fn a_gateway_with_no_groups_is_running_with_an_empty_model_list() {
    let path = database_path();
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let state = service.runtime_state();
    assert_eq!(state.status, vibex_core::ModelGatewayStatus::Running);
    assert_eq!(
        state.route_count,
        Some(0),
        "no groups means nothing to route, not a broken listener"
    );
    assert!(state.base_url().is_some(), "the port is bound");

    let body: Value = reqwest::Client::new()
        .get(format!("{gateway_url}/v1/models"))
        .header(header::HOST, "127.0.0.1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().map(Vec::len),
        Some(0),
        "a listener with no groups publishes no models"
    );

    // Asking for a model it cannot serve is a request error, not a sign that
    // the gateway failed to start.
    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": "group/does-not-exist/deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
    assert_eq!(
        service.runtime_state().status,
        vibex_core::ModelGatewayStatus::Running,
        "one 404 must not take the listener down"
    );

    gateway_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn passthrough_stream_reaches_the_client_and_the_ledger() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test-123",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "say hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains("hello"), "{body}");
    assert!(body.contains("[DONE]"), "{body}");

    let recorded = recorder.lock().await;
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(
        recorded.authorization.as_deref(),
        Some("Bearer sk-test-123")
    );
    assert_eq!(recorded.body.as_ref().unwrap()["model"], "deepseek-chat");

    let logs = service.recent_requests(10).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(
        logs[0].wire_in,
        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
    );
    assert_eq!(
        logs[0].model_requested,
        group_model(&group, "deepseek-chat"),
        "the ledger records the id the Agent asked for"
    );
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Ok);
    assert_eq!(logs[0].input_tokens, 11);
    assert_eq!(logs[0].output_tokens, 3);
    let encoded = serde_json::to_string(&logs).unwrap().to_lowercase();
    assert!(!encoded.contains("sk-test-123"), "the ledger leaked a Key");
    assert!(!encoded.contains("say hi"), "the ledger leaked a prompt");

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_translated_call_is_re_encoded_for_the_client_protocol() {
    let path = database_path();
    // The upstream speaks Anthropic; the client speaks Chat.
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, anthropic_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::AnthropicMessages,
        &["claude-sonnet-4-5"],
        "sk-ant-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "claude-sonnet-4-5"),
            "messages": [{"role": "user", "content": "say hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    // Chat SSE frames, not Anthropic ones.
    assert!(body.contains("chat.completion.chunk"), "{body}");
    assert!(body.contains("\"content\":\"hi\""), "{body}");
    assert!(!body.contains("message_start"), "{body}");
    assert!(body.contains("[DONE]"), "{body}");

    let recorded = recorder.lock().await;
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(recorded.api_key.as_deref(), Some("sk-ant-test"));
    assert_eq!(
        recorded.body.as_ref().unwrap()["model"],
        "claude-sonnet-4-5"
    );
    // Anthropic requires `max_tokens`.
    assert!(recorded.body.as_ref().unwrap()["max_tokens"].is_number());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// An Anthropic client must see exactly one message start and one stop.
///
/// This is the shape a Claude Code session on a route group uses: the client
/// speaks Anthropic, the group's upstream speaks Responses, so the gateway
/// translates. Claude's adapter treats a second `message_start` as a new
/// message and forgets what it already streamed, which makes the consolidated
/// assistant message print the whole answer again — the duplicate reply the
/// user saw. A decoder rebuilt per upstream chunk produced exactly that,
/// because every chunk then looked like the first frame of a new message.
#[tokio::test]
async fn an_anthropic_client_sees_one_message_start_and_one_stop() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) =
        mock_upstream_frame_by_frame(responses_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["gpt-5"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/messages"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "gpt-5"),
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "say hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert_eq!(
        body.matches("event: message_start").count(),
        1,
        "a second message start reads as a second message to Claude: {body}"
    );
    assert_eq!(
        body.matches("event: message_stop").count(),
        1,
        "one reply, one terminal event: {body}"
    );
    assert_eq!(
        body.matches(r#""text":"hello""#).count(),
        1,
        "the answer must be delivered once: {body}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_non_streaming_client_gets_one_body() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(
        200,
        chat_stream(),
        Some(json!({
            "id": "c1",
            "object": "chat.completion",
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello world"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        })),
    )
    .await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "say hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello world");
    assert_eq!(body["usage"]["prompt_tokens"], 5);
    let recorded = recorder.lock().await;
    assert_eq!(recorded.body.as_ref().unwrap()["stream"], false);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_translated_non_streaming_client_gets_a_whole_body() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, anthropic_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    // Anthropic-only upstream: the Chat request must be translated, and the
    // client asked for one JSON body, so the gateway has to gather the stream.
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::AnthropicMessages,
        &["claude"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "claude"),
            "messages": [{"role": "user", "content": "say hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hi");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(
        recorder.lock().await.body.as_ref().unwrap()["stream"],
        true,
        "a translated non-streaming call is read from the upstream as a stream"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_invalid_key_is_marked_and_the_next_key_answers() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-good",
    );
    // A second credential that does not resolve to a stored value behaves like
    // an unusable Key; the first one still answers.
    {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials.push(ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Broken".to_string(),
            status: AgentCredentialStatus::Missing,
            credential: AgentCredential::Local { runtime: None },
            revision: 1,

            models: Vec::new(),
            proxy_url: None,
        });
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
    }
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-good")
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_preferred_key_is_tried_before_the_provider_order() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream_refusing("Bearer sk-refused", chat_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-refused",
    );
    // The second Key is valid but sits behind the refused one in the Provider's
    // order; preferring it is the only thing that makes it first.
    let preferred = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-good").unwrap();
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Backup".to_string(),
            status: AgentCredentialStatus::Ready,
            credential: AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: RequestId::new(),
                    backend: secrets::provider_secret_write_backend(),
                    setup_state: ProviderSecretSetupState::Available,
                    lookup_key,
                    redacted_hint: "stored".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            revision: 1,

            models: Vec::new(),
            proxy_url: None,
        };
        let id = credential.id.clone();
        profile.credentials.push(credential);
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
        id
    };
    let group = point_gateway_at(
        &path,
        upstream.clone(),
        GatewayPolicy {
            preferred_keys: std::collections::BTreeMap::from([(
                upstream.clone(),
                preferred.clone(),
            )]),
            ..GatewayPolicy::default()
        },
    );
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-good")
    );

    // The refused Key was never attempted, so no cooldown was recorded for it:
    // preference is routing order, not a failover that happened to work.
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert!(states.is_empty(), "{states:?}");

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_authentication_failure_cools_the_key_until_it_is_replaced() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(401, String::new(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-revoked",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].cooldown_reason, Some(GatewayFailureKind::Auth));
    // An auth failure has no duration: the Key never recovers on its own.
    assert!(states[0].cooldown_until_ms.is_none());
    assert!(!states[0].is_ready_at(vibex_core::unix_timestamp_ms()));

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_authentication_failure_fails_over_to_the_next_key() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream_refusing_with_status("Bearer sk-revoked", 401, chat_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-revoked",
    );
    config
        .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
            model_provider_profile_id: upstream.clone(),
            display_name: Some("Backup".to_string()),
            value: "sk-good".to_string(),
        })
        .unwrap();
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-good"),
        "a rejected Key must not end the request while another Key can answer"
    );
    let logs = service.recent_requests(10).unwrap();
    let trace = logs[0].route.as_ref().expect("the route is recorded");
    assert_eq!(trace.failed_attempts.len(), 1);
    assert_eq!(trace.failed_attempts[0].failure, GatewayFailureKind::Auth);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_cooling_key_is_skipped_and_the_second_key_answers() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-second",
    );
    // Add a higher-priority Key and mark it as cooling.
    let cooled = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-cooling").unwrap();
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Cooling".to_string(),
            status: AgentCredentialStatus::Ready,
            credential: AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: RequestId::new(),
                    backend: secrets::provider_secret_write_backend(),
                    setup_state: ProviderSecretSetupState::Available,
                    lookup_key,
                    redacted_hint: "stored".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            revision: 1,

            models: Vec::new(),
            proxy_url: None,
        };
        profile.credentials.insert(0, credential.clone());
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
        let mut state = ModelProviderAccountState::ready(upstream.clone(), credential.id.clone());
        state.cooldown_reason = Some(GatewayFailureKind::Rate);
        state.cooldown_until_ms = Some(vibex_core::unix_timestamp_ms() + 60_000);
        ModelProviderAccountStateRepository::upsert(&connection, &state).unwrap();
        credential.id
    };
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-second")
    );
    let logs = service.recent_requests(10).unwrap();
    assert_ne!(logs[0].credential_id.as_ref(), Some(&cooled));
    // The ledger explains the request without exposing a Key: the cooling Key
    // is counted, the answering Key is named by id, and nothing was tried and
    // failed on the way.
    let trace = logs[0].route.as_ref().expect("the route is recorded");
    assert_eq!(trace.skipped_cooling, 1);
    assert_eq!(trace.skipped_over_limit, 0);
    assert!(trace.failed_attempts.is_empty());
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Ok);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A Key that fails before the reply begins is named, with its reason, on the
/// row that records the Key which answered instead.
#[tokio::test]
async fn a_failover_records_the_refused_key_and_why_it_moved_on() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream_refusing("Bearer sk-refused", chat_stream()).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-refused",
    );
    // A second Key on the same Provider, behind the refused one.
    let backup = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-backup").unwrap();
        let credential = ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Backup".to_string(),
            status: AgentCredentialStatus::Ready,
            credential: AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: RequestId::new(),
                    backend: secrets::provider_secret_write_backend(),
                    setup_state: ProviderSecretSetupState::Available,
                    lookup_key,
                    redacted_hint: "stored".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            revision: 1,

            models: Vec::new(),
            proxy_url: None,
        };
        profile.credentials.push(credential.clone());
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
        credential.id
    };
    let refused = {
        let connection = open_database(&path).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials[0].id.clone()
    };
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-backup")
    );

    let logs = service.recent_requests(10).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Failover);
    assert_eq!(logs[0].credential_id.as_ref(), Some(&backup));
    let trace = logs[0].route.as_ref().expect("the route is recorded");
    assert_eq!(trace.skipped_cooling, 0);
    assert_eq!(trace.failed_attempts.len(), 1);
    assert_eq!(trace.failed_attempts[0].credential_id, refused);
    assert_eq!(trace.failed_attempts[0].failure, GatewayFailureKind::Rate);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A Provider that holds more than one Key is not a failover by itself.
///
/// The ledger used to call any request from a multi-Key pool a failover, which
/// made the column useless exactly when failovers were worth reading about.
#[tokio::test]
async fn a_first_try_success_on_a_multi_key_provider_is_not_a_failover() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-first",
    );
    {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let mut profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        secrets::store_provider_secret(&lookup_key, "sk-second").unwrap();
        profile.credentials.push(ModelProviderCredentialReference {
            id: RequestId::new(),
            display_name: "Secondary".to_string(),
            status: AgentCredentialStatus::Ready,
            credential: AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: RequestId::new(),
                    backend: secrets::provider_secret_write_backend(),
                    setup_state: ProviderSecretSetupState::Available,
                    lookup_key,
                    redacted_hint: "stored".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            revision: 1,

            models: Vec::new(),
            proxy_url: None,
        });
        let revision = profile.revision;
        profile.revision = revision + 1;
        vibex_db::ModelProviderProfileRepository::update(&connection, &profile, revision).unwrap();
    }
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();

    let logs = service.recent_requests(10).unwrap();
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Ok);
    assert!(
        logs[0]
            .route
            .as_ref()
            .is_some_and(|trace| trace.is_unremarkable())
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn all_keys_cooling_is_a_429_without_an_upstream_call() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-only",
    );
    {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        let mut state =
            ModelProviderAccountState::ready(upstream.clone(), profile.credentials[0].id.clone());
        state.cooldown_reason = Some(GatewayFailureKind::Credit);
        state.cooldown_until_ms = Some(vibex_core::unix_timestamp_ms() + 60_000);
        ModelProviderAccountStateRepository::upsert(&connection, &state).unwrap();
    }
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_gateway_keys_cooling");
    // Nothing reached the upstream.
    assert!(recorder.lock().await.path.is_empty());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn an_unknown_model_lists_the_closest_configured_ones() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat", "deepseek-reasoner"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepsek-chat"),
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("deepseek-chat")
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_vendor_side_file_reference_fails_loudly_before_any_call() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    // The upstream speaks Chat only, so a `file_id` image cannot cross over.
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_file", "file_id": "file-abc"}
                ]}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unsupported_image_reference");
    assert!(recorder.lock().await.path.is_empty());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_conversation_stays_on_its_pinned_key() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-first",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let client = reqwest::Client::new();
    for turn in 0..3 {
        let response = client
            .post(format!("{gateway_url}/v1/chat/completions"))
            .header(header::HOST, "127.0.0.1")
            .header("x-session-id", "conversation-1")
            .json(&json!({
                "model": group_model(&group, "deepseek-chat"),
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "hi"}
                ],
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "turn {turn}");
        let _ = response.text().await.unwrap();
    }
    assert_eq!(
        recorder.lock().await.authorization.as_deref(),
        Some("Bearer sk-first")
    );

    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let pins = connection
        .query_row(
            "SELECT COUNT(*) FROM model_gateway_conversation_pin",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(pins, 1);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn a_browser_origin_is_refused() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .header(header::ORIGIN, "http://127.0.0.1:3425")
        .json(&json!({"model": group_model(&group, "deepseek-chat"), "messages": []}))
        .send()
        .await
        .unwrap();
    assert_ne!(response.status(), 200);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[tokio::test]
async fn the_token_ceiling_stops_requests_and_the_window_rolls_over() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(
        &path,
        upstream.clone(),
        GatewayPolicy {
            token_limit: Some(vibex_core::GatewayTokenLimit {
                window: GatewayTokenWindow::Day,
                tokens: 10,
            }),
            ..GatewayPolicy::default()
        },
    );
    // The stream reports 11 input + 3 output tokens, so one call crosses a
    // ceiling of 10.
    GatewayTokenWindowRepository::record(
        &{
            let mut connection = open_database(&path).unwrap();
            apply_migrations(&mut connection).unwrap();
            connection
        },
        &upstream,
        GatewayTokenWindow::Day,
        10,
        vibex_core::unix_timestamp_ms(),
    )
    .unwrap();
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_gateway_token_ceiling_reached");
    assert!(recorder.lock().await.path.is_empty());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

#[test]
fn failure_classification_is_shared_with_the_service() {
    assert_eq!(classify_failure(401, ""), GatewayFailureKind::Auth);
    assert_eq!(
        classify_failure(429, "rate limit exceeded"),
        GatewayFailureKind::Rate
    );
}

/// One persisted Provider, for the tests that need a real foreign key.
///
/// The local default placeholders were removed, so a migrated database starts
/// with no Providers at all. A test that needs a row seeds its own rather than
/// relying on a fixture that no longer exists.
fn seed_persisted_provider(connection: &vibex_db::DbConnection) -> ModelProviderProfileId {
    let profile = vibex_core::ModelProviderProfile {
        display_name: "Test Provider".to_string(),
        status: vibex_core::ModelProviderProfileStatus::Enabled,
        ..Default::default()
    };
    vibex_db::ModelProviderProfileRepository::insert(connection, &profile).unwrap();
    profile.id
}

#[test]
fn pins_round_trip_through_the_database() {
    let path = database_path();
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let upstream = seed_persisted_provider(&connection);
    let now = vibex_core::unix_timestamp_ms();
    GatewayConversationPinRepository::upsert(
        &connection,
        &GatewayConversationPin {
            conversation_key: "k".to_string(),
            model_provider_profile_id: upstream,
            credential_id: RequestId::new(),
            chosen_at_ms: now,
            expires_at_ms: now + GatewayConversationPin::TTL_MS,
        },
    )
    .unwrap();
    assert!(
        GatewayConversationPinRepository::get(&connection, "k")
            .unwrap()
            .is_some()
    );
    cleanup(path);
}

#[test]
fn ledger_retention_is_seven_days() {
    let path = database_path();
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let upstream = seed_persisted_provider(&connection);
    let now = vibex_core::unix_timestamp_ms();
    for age in [0, vibex_core::ModelGatewayRequestLog::RETENTION_MS + 1] {
        ModelGatewayRequestLogRepository::insert(
            &connection,
            &vibex_core::ModelGatewayRequestLog {
                request_id: RequestId::new(),
                agent_id: Some("codex".to_string()),
                model_provider_profile_id: upstream.clone(),
                credential_id: None,
                model_requested: "m".to_string(),
                model_served: None,
                wire_in: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                wire_out: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                status: Some(200),
                input_tokens: 1,
                output_tokens: 1,
                cached_read_tokens: 0,
                cached_write_tokens: 0,
                reasoning_tokens: 0,
                latency_ms: Some(1),
                outcome: GatewayRequestOutcome::Ok,
                route: None,
                created_at_ms: now - age,
            },
        )
        .unwrap();
    }
    assert_eq!(
        ModelGatewayRequestLogRepository::prune(&connection, now).unwrap(),
        1
    );
    cleanup(path);
}

/// A configured concurrency limit bounds what reaches the upstream.
///
/// The mock counts overlapping requests on its own side, which is the only
/// honest measurement: a client-side counter would see the test's parallelism
/// rather than what the gateway actually sent.
#[tokio::test]
async fn a_concurrency_limit_serializes_requests_rather_than_dropping_them() {
    let path = database_path();
    let (upstream_url, recorder, overlap, upstream_task) =
        mock_upstream_with_overlap(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    // Serialize the Key to one in-flight request.
    let credential = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials[0].id.clone()
    };
    let mut policy = GatewayPolicy::default();
    policy.account_limits.insert(
        credential,
        vibex_core::GatewayAccountLimits {
            max_concurrency: Some(1),
            queue_wait_ms: Some(5_000),
            ..vibex_core::GatewayAccountLimits::default()
        },
    );
    let group = point_gateway_at(&path, upstream, policy);
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let mut handles = Vec::new();
    for _ in 0..3 {
        let client = reqwest::Client::new();
        let url = format!("{gateway_url}/v1/chat/completions");
        let group = group.clone();
        handles.push(tokio::spawn(async move {
            client
                .post(url)
                .header(header::HOST, "127.0.0.1")
                .json(&json!({
                    "model": group_model(&group, "deepseek-chat"),
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": true
                }))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    for handle in handles {
        assert_eq!(
            handle.await.unwrap(),
            200,
            "a queued request must still answer"
        );
    }
    assert_eq!(
        overlap.peak(),
        1,
        "the upstream saw {} requests at once under a limit of one",
        overlap.peak()
    );
    // All three were served, so the limiter queued rather than refused.
    assert_eq!(service.recent_requests(10).unwrap().len(), 3);
    let _ = recorder;

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Without a limit the same three requests do overlap, which proves the test
/// above measures the limit rather than the mock's own timing.
#[tokio::test]
async fn without_a_limit_the_same_requests_do_overlap() {
    let path = database_path();
    let (upstream_url, _recorder, overlap, upstream_task) =
        mock_upstream_with_overlap(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let mut handles = Vec::new();
    for _ in 0..3 {
        let client = reqwest::Client::new();
        let url = format!("{gateway_url}/v1/chat/completions");
        let group = group.clone();
        handles.push(tokio::spawn(async move {
            client
                .post(url)
                .header(header::HOST, "127.0.0.1")
                .json(&json!({
                    "model": group_model(&group, "deepseek-chat"),
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": true
                }))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    for handle in handles {
        assert_eq!(handle.await.unwrap(), 200);
    }
    assert!(
        overlap.peak() > 1,
        "the mock never overlapped, so the limit test proves nothing: peak {}",
        overlap.peak()
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// An RPM limit that a request cannot wait out is a 429 that says why.
#[tokio::test]
async fn an_exhausted_minute_is_a_429_before_the_upstream_is_called() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let credential = {
        let mut connection = open_database(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let profile = vibex_db::ModelProviderProfileRepository::get(&connection, &upstream)
            .unwrap()
            .unwrap();
        profile.credentials[0].id.clone()
    };
    let mut policy = GatewayPolicy::default();
    policy.account_limits.insert(
        credential,
        vibex_core::GatewayAccountLimits {
            max_rpm: Some(1),
            queue_wait_ms: Some(1),
            ..vibex_core::GatewayAccountLimits::default()
        },
    );
    let group = point_gateway_at(&path, upstream, policy);
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let client = reqwest::Client::new();
    for turn in 0..2 {
        let response = client
            .post(format!("{gateway_url}/v1/chat/completions"))
            .header(header::HOST, "127.0.0.1")
            .json(&json!({
                "model": group_model(&group, "deepseek-chat"),
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        if turn == 0 {
            assert_eq!(response.status(), 200);
            let _ = response.text().await;
        } else {
            assert_eq!(
                response.status(),
                429,
                "the second request in a one-request minute must be refused"
            );
            let body: Value = response.json().await.unwrap();
            assert!(
                body["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("minute"),
                "{body}"
            );
        }
    }

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Three consecutive unreachable upstreams degrade the gateway; a recovered
/// upstream then clears the state through a single probe.
///
/// The contract has two halves. While degraded, the gateway admits one request
/// at a time instead of refusing everything, because the only signal that can
/// recover it is an upstream success. If nothing is admitted, no success can be
/// observed and the state could only clear on a restart.
#[tokio::test]
async fn repeated_failures_degrade_the_gateway_and_one_probe_recovers_it() {
    let path = database_path();
    // A port nothing listens on: every attempt fails to connect.
    let dead_url = {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    };
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &dead_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let client = reqwest::Client::new();
    let group_for_call = group.clone();
    let call = |client: reqwest::Client, url: String| {
        let group = group_for_call.clone();
        async move {
            client
                .post(url)
                .header(header::HOST, "127.0.0.1")
                .json(&json!({
                    "model": group_model(&group, "deepseek-chat"),
                    "messages": [{"role": "user", "content": "hi"}],
                    "stream": true
                }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };

    for _ in 0..vibex_model_gateway::ModelGatewayService::DEGRADE_AFTER_FAILURES {
        let status = call(client.clone(), format!("{gateway_url}/v1/chat/completions")).await;
        assert!(status >= 400, "a dead upstream should fail, got {status}");
    }
    assert!(
        service.is_degraded(),
        "the gateway did not step aside after {} failures",
        vibex_model_gateway::ModelGatewayService::DEGRADE_AFTER_FAILURES
    );
    assert_eq!(
        service.runtime_state().status,
        vibex_core::ModelGatewayStatus::Degraded
    );
    assert!(
        service.runtime_state().notice.is_some(),
        "a degraded gateway must say why"
    );

    // A degraded gateway still lets exactly one request through. The upstream
    // is dead, so that probe fails — but it is the probe that keeps recovery
    // possible.
    let probe = call(client.clone(), format!("{gateway_url}/v1/chat/completions")).await;
    assert!(
        probe >= 400,
        "a probe against a dead upstream must fail, got {probe}"
    );
    assert!(
        service.is_degraded(),
        "a failed probe must leave the gateway degraded"
    );

    // A second concurrent request cannot claim the slot the first one holds.
    // Without an upstream to talk to, both fail; the point is that the gateway
    // never leaves degraded on a failure.
    let again = call(client.clone(), format!("{gateway_url}/v1/chat/completions")).await;
    assert!(again >= 400, "the gateway must not answer from nothing");
    assert!(service.is_degraded());

    gateway_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Which path each request reached, for a mock that answers by protocol.
type Paths = Arc<Mutex<Vec<String>>>;

/// A vendor that speaks Chat Completions and answers anything else with the
/// shape error the real one produced.
///
/// The 422 body is copied from what an OpenAI-compatible Chat Completions
/// endpoint says when it is handed a Responses body, because that message is
/// what the mismatch detector keys on.
async fn mock_chat_only_upstream() -> (String, Paths, tokio::task::JoinHandle<()>) {
    let paths: Paths = Arc::new(Mutex::new(Vec::new()));
    let state = paths.clone();
    let router = Router::new()
        .route(
            "/{*path}",
            post(
                move |uri: axum::http::Uri, body: axum::body::Bytes| async move {
                    state.lock().await.push(uri.path().to_string());
                    if uri.path().contains("/chat/completions") {
                        let _ = body;
                        let mut response = Response::new(Body::from(chat_stream()));
                        response.headers_mut().insert(
                            header::CONTENT_TYPE,
                            header::HeaderValue::from_static("text/event-stream"),
                        );
                        return response;
                    }
                    let mut response = Response::new(Body::from(
                        json!({"error": {"message":
                            "messages[0].content: unknown variant `input_text`, expected one of `text`, `image_url`, `file`"
                        }})
                        .to_string(),
                    ));
                    *response.status_mut() = axum::http::StatusCode::UNPROCESSABLE_ENTITY;
                    response
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), paths, task)
}

/// A body the gateway rebuilt wrongly is its own failure, not a wrong protocol.
///
/// The endpoint declared Responses and the client spoke Responses, so the
/// protocol was never in question. Reporting a shape refusal as a protocol
/// mismatch sent the user to a setting that could not fix anything, and an
/// endpoint whose protocol was only inferred would have had the wrong answer
/// remembered for every later request. The row also has to be in the ledger: a
/// failure nobody can find is a failure nobody can fix.
#[tokio::test]
async fn a_shape_refusal_on_a_settled_protocol_blames_the_gateway() {
    let path = database_path();
    let (upstream_url, _paths, upstream_task) = mock_shape_refusing_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["gpt-5"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "gpt-5"),
            "input": "say hi",
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_str),
        Some("model_gateway_translation_failed"),
        "a body the gateway built is not a wrong protocol: {body}"
    );
    assert!(
        !body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("wrong protocol"),
        "the message must not send the reader to a protocol setting: {body}"
    );

    // The Key was fine and the protocol was right, so nothing is charged and
    // nothing is remembered for the next request.
    let connection = open_database(&path).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert!(
        states.iter().all(|state| state.cooldown_until_ms.is_none()),
        "the Key served the request; only the body was wrong: {states:?}"
    );
    assert_eq!(
        vibex_db::GatewayEndpointProtocolRepository::get(&connection, &upstream, "api").unwrap(),
        None,
        "a settled protocol must not be re-learned from the gateway's own bug"
    );
    // And the failure is in the ledger, with the same code the client read.
    let logs = ModelGatewayRequestLogRepository::recent(&connection, 10).unwrap();
    assert_eq!(logs.len(), 1, "a failed request must reach the ledger");
    assert_eq!(logs[0].outcome, GatewayRequestOutcome::Failed);
    assert_eq!(
        logs[0]
            .route
            .as_ref()
            .and_then(|route| route.error_code.as_deref()),
        Some("model_gateway_translation_failed")
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// An upstream that answers the shape error a whole-body flattener produces.
async fn mock_shape_refusing_upstream() -> (String, Paths, tokio::task::JoinHandle<()>) {
    let paths: Paths = Arc::new(Mutex::new(Vec::new()));
    let state = paths.clone();
    let router = Router::new().route(
        "/{*path}",
        post(move |uri: axum::http::Uri, _body: axum::body::Bytes| async move {
            state.lock().await.push(uri.path().to_string());
            let mut response = Response::new(Body::from(
                json!({"error": {"message":
                    "messages[6]: unknown variant `function_call_output`, expected one of `text`, `image_url`, `file`"
                }})
                .to_string(),
            ));
            *response.status_mut() = axum::http::StatusCode::UNPROCESSABLE_ENTITY;
            response
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), paths, task)
}

/// An upstream that reads `input` the way the vendor does.
///
/// The real endpoint flattens `input` into a Chat-shaped `messages` list before
/// it validates anything, and its content enum knows only `text`, `image_url`,
/// and `file`. A `function_call_output` nested inside a message's `content` is
/// therefore a hard 422 — which is exactly what every Agent hit the moment a
/// tool result entered the conversation.
async fn mock_input_validating_upstream() -> (String, Recorder, tokio::task::JoinHandle<()>) {
    let recorder: Recorder = Arc::new(Mutex::new(Recorded::default()));
    let state = recorder.clone();
    let router = Router::new().route(
        "/{*path}",
        post(move |uri: axum::http::Uri, body: axum::body::Bytes| {
            let state = state.clone();
            async move {
                let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                {
                    let mut recorded = state.lock().await;
                    recorded.path = uri.path().to_string();
                    recorded.body = Some(value.clone());
                }
                let mut messages = 0usize;
                if let Some(items) = value.get("input").and_then(Value::as_array) {
                    for item in items {
                        if item.get("type").and_then(Value::as_str) == Some("function_call_output") {
                            // A sibling input item is fine; the message count is
                            // what the vendor's error names, so keep counting.
                            messages += 1;
                            continue;
                        }
                        if let Some(content) = item.get("content").and_then(Value::as_array) {
                            for part in content {
                                let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
                                if !matches!(kind, "input_text" | "input_image" | "input_file") {
                                    let mut response = Response::new(Body::from(
                                        json!({"error": {"message": format!(
                                            "messages[{messages}]: unknown variant `{kind}`, expected one of `text`, `image_url`, `file`"
                                        )}})
                                        .to_string(),
                                    ));
                                    *response.status_mut() =
                                        axum::http::StatusCode::UNPROCESSABLE_ENTITY;
                                    return response;
                                }
                            }
                        }
                        messages += 1;
                    }
                }
                let mut response = Response::new(Body::from(responses_stream()));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("text/event-stream"),
                );
                response
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), recorder, task)
}

/// A tool round trip survives the translation into Responses.
///
/// This is the Claude path in the reported failure: the client speaks
/// Anthropic, the group's member speaks Responses, so the gateway translates a
/// `tool_result` on the way out. The translated body used to nest it inside
/// `content`, so the vendor rejected the whole turn — after the first tool call
/// and never before, which is what made it look like "it breaks after a while".
#[tokio::test]
async fn a_tool_result_survives_the_translation_into_responses() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_input_validating_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["deepseek-v4.1-flash"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/messages"))
        .header(header::HOST, "127.0.0.1")
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": group_model(&group, "deepseek-v4.1-flash"),
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "list the files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "ls", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Cargo.toml"}
                ]}
            ]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "a tool round trip must not fail: {body}");

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    let input = sent["input"].as_array().unwrap();
    // The tool result is a sibling item, and the call it answers is beside it.
    assert!(
        input
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "toolu_1"),
        "the result must be an input item: {sent}"
    );
    assert!(
        input.iter().any(|item| item["type"] == "function_call"),
        "the call must be beside it: {sent}"
    );
    // And it never appears inside a message's content.
    for item in input {
        let nested = item
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content
                    .iter()
                    .any(|part| part["type"] == "function_call_output")
            });
        assert!(!nested, "the result must not be nested in content: {sent}");
    }

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A tool round trip survives a same-protocol passthrough unchanged.
///
/// This is the Codex path: the client speaks Responses and the member does too,
/// so the body is forwarded as written. It used to be re-encoded, which both
/// re-nested the tool result and dropped the vendor's own fields.
#[tokio::test]
async fn a_tool_result_survives_a_responses_passthrough() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_input_validating_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["gpt-5"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "gpt-5"),
            "store": false,
            "prompt_cache_key": "cache-key-1",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "list the files"}
                ]},
                {"type": "function_call", "call_id": "call_1", "name": "ls", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "Cargo.toml"}
            ],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "a tool round trip must not fail: {body}");

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    assert_eq!(
        sent["prompt_cache_key"], "cache-key-1",
        "a passthrough keeps the vendor's own fields: {sent}"
    );
    assert_eq!(sent["store"], json!(false), "{sent}");
    let input = sent["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "call_1"),
        "{sent}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// An undeclared endpoint is tried once and then remembered.
///
/// This is the whole fix in one test: the first request guesses wrong, the
/// vendor says so in a way that must *not* cool the Key or degrade the group,
/// the gateway retries the plausible alternative, and the answer is kept so the
/// next request starts on the protocol that worked.
#[tokio::test]
async fn an_undeclared_endpoint_is_tried_once_and_the_answer_is_remembered() {
    let path = database_path();
    let (upstream_url, paths, upstream_task) = mock_chat_only_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_untyped_upstream(&config, &upstream_url, &["deepseek-chat"], "sk-test");
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let body = json!({
        "model": group_model(&group, "deepseek-chat"),
        "input": [{"type": "message", "role": "user", "content": "hi"}],
        "stream": true
    });
    let client = reqwest::Client::new();
    let first = client
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = first.status();
    let text = first.text().await.unwrap();
    assert_eq!(
        status, 200,
        "the alternate protocol should have answered: {text}"
    );

    let tried = paths.lock().await.clone();
    assert_eq!(
        tried,
        vec![
            "/v1/responses".to_string(),
            "/v1/messages".to_string(),
            "/v1/chat/completions".to_string()
        ],
        "the guess is walked once, in preference order, until one answers"
    );

    // The wrong guess cost nothing: no Key cooldown, no degraded group.
    let connection = open_database(&path).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert!(
        states.iter().all(|state| state.cooldown_until_ms.is_none()),
        "a wrong protocol must not cool the Key, got {states:?}"
    );
    assert!(!service.is_degraded());
    assert_eq!(
        service.runtime_state().status,
        vibex_core::ModelGatewayStatus::Running
    );

    // The answer was kept, so the second request does not pay for it again.
    assert_eq!(
        vibex_db::GatewayEndpointProtocolRepository::get(&connection, &upstream, "api").unwrap(),
        Some("openai_chat_completions".to_string())
    );

    let second = client
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let _ = second.text().await.unwrap();
    assert_eq!(
        paths.lock().await.clone(),
        vec![
            "/v1/responses".to_string(),
            "/v1/messages".to_string(),
            "/v1/chat/completions".to_string(),
            "/v1/chat/completions".to_string()
        ],
        "a remembered verdict replaces the guess"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A mismatch with nowhere to fall back to is a 502, not a cooled Key.
///
/// When translation to the only alternative is switched off, the request has to
/// fail as a protocol problem the user can see. Classifying it as a Key failure
/// is what let three wrong guesses close the gateway.
#[tokio::test]
async fn a_protocol_mismatch_without_a_fallback_blames_neither_the_key_nor_the_group() {
    let path = database_path();
    let (upstream_url, _paths, upstream_task) = mock_chat_only_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_untyped_upstream(&config, &upstream_url, &["deepseek-chat"], "sk-test");
    let mut policy = GatewayPolicy::default();
    policy.translate.openai_chat_completions = false;
    let group = point_gateway_at(&path, upstream.clone(), policy);
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "input": [{"type": "message", "role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body.get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str),
        Some("model_gateway_protocol_mismatch"),
        "the caller must read what actually went wrong, got {body}"
    );

    let connection = open_database(&path).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &upstream).unwrap();
    assert!(
        states.iter().all(|state| state.cooldown_until_ms.is_none()),
        "the Key worked; only the body was addressed wrong, got {states:?}"
    );
    assert!(
        !service.is_degraded(),
        "one wrong guess must not latch a breaker"
    );
    assert_eq!(service.runtime_state().degraded_routes, 0);

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A degraded group recovers as soon as an upstream answers again, without a
/// restart.
///
/// This is the regression the probe exists for: refusing every request while
/// degraded meant `note_success` could never run, so the state was terminal.
#[tokio::test]
async fn a_degraded_gateway_recovers_when_an_upstream_answers() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    // Trip this group's breaker directly: the failure counting itself is
    // covered above, and driving three dead upstreams here would only repeat it.
    for _ in 0..vibex_model_gateway::ModelGatewayService::DEGRADE_AFTER_FAILURES {
        service.note_failure(group.as_str());
    }
    assert!(service.is_degraded());
    assert_eq!(service.runtime_state().degraded_routes, 1);

    let client = reqwest::Client::new();
    let status = client
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200, "the probe should have reached the upstream");
    assert!(
        !service.is_degraded(),
        "one good answer must clear the degraded state without a restart"
    );
    assert_eq!(
        service.runtime_state().status,
        vibex_core::ModelGatewayStatus::Running
    );
    assert!(service.runtime_state().notice.is_none());

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Seeds a model group over the same upstream model at several Providers.
fn seed_provider_group(
    path: &std::path::Path,
    name: &str,
    providers: &[ModelProviderProfileId],
    model: &str,
    strategy: vibex_core::RouteStrategy,
) -> vibex_core::ModelRouteId {
    let mut connection = open_database(path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: name.to_string(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some(model.to_string()),
        strategy,
        status: vibex_core::RouteStatus::Enabled,
        members: providers
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                vibex_core::ModelRouteMember::new(provider.clone(), model, index as i64)
            })
            .collect(),
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    route.id.clone()
}

/// Seeds a model group: explicit `(Provider, upstream model)` members behind
/// one Agent-facing model id.
fn seed_model_group(
    path: &std::path::Path,
    name: &str,
    agent_model: &str,
    members: &[(ModelProviderProfileId, &str)],
    rules: Vec<vibex_core::ModelRouteRule>,
) -> vibex_core::ModelRouteId {
    let mut connection = open_database(path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: name.to_string(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some(agent_model.to_string()),
        status: vibex_core::RouteStatus::Enabled,
        strategy: vibex_core::RouteStrategy::Ordered,
        rules,
        members: members
            .iter()
            .enumerate()
            .map(|(index, (provider, model))| {
                vibex_core::ModelRouteMember::new(provider.clone(), *model, index as i64)
            })
            .collect(),
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    route.id.clone()
}

/// A provider group offers only the models every Provider serves, and a
/// request for one is answered by a Provider that serves it.
///
/// The group is ordered so the second Provider would answer a plain model id
/// first; the point is that the model — not the Provider — is what the Agent
/// names, and the group's membership decides who may answer.
#[tokio::test]
async fn a_provider_group_routes_a_shared_model_to_a_serving_provider() {
    let path = database_path();
    let (left_url, left_recorder, left_task) = mock_upstream(200, chat_stream(), None).await;
    let (right_url, right_recorder, right_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let left = seed_upstream(
        &config,
        &left_url,
        WireProtocol::ChatCompletions,
        &["shared-model"],
        "sk-left",
    );
    let right = seed_upstream(
        &config,
        &right_url,
        WireProtocol::ChatCompletions,
        &["shared-model"],
        "sk-right",
    );
    enable_gateway_at(&path, GatewayPolicy::default());
    let group_id = seed_provider_group(
        &path,
        "shared-model",
        &[left.clone(), right.clone()],
        "shared-model",
        vibex_core::RouteStrategy::Ordered,
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    // The group's own catalogue must publish the shared model, spelled the way
    // a session selects it.
    let served = service.read_snapshot().unwrap();
    let selected = format!("group/{}/shared-model", group_id.as_str());
    let group = served
        .group_for_model(&selected)
        .expect("the group is in the snapshot");
    assert_eq!(group.1, "shared-model");
    assert_eq!(group.0.models.len(), 1, "one shared model");

    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("group/{}/shared-model", group_id.as_str()),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(
        status, 200,
        "a shared model must be routable through the group"
    );
    assert!(
        !left_recorder.lock().await.path.is_empty(),
        "the first Provider in the group should answer"
    );
    assert!(
        right_recorder.lock().await.path.is_empty(),
        "the ordered group only fails over when it must"
    );

    gateway_task.abort();
    left_task.abort();
    right_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A group of one Provider is served: the gateway still fronts it, which is
/// what gives a single upstream the gateway's protocol translation.
#[tokio::test]
async fn a_provider_group_of_one_is_served_through_the_gateway() {
    let path = database_path();
    let (url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let only = seed_upstream(
        &config,
        &url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-only",
    );
    let group_id = point_gateway_at(&path, only.clone(), GatewayPolicy::default());

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let served = service.read_snapshot().unwrap();
    let group = served
        .groups
        .get(group_id.as_str())
        .expect("a one-Provider group must be in the snapshot");
    assert_eq!(group.models.len(), 1);
    assert_eq!(group.models[0].model, "deepseek-chat");
    assert_eq!(
        group.models[0].providers,
        vec![only.clone()],
        "the lone Provider is the only source"
    );

    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("group/{}/deepseek-chat", group_id.as_str()),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200, "a single-Provider group must answer");
    assert!(
        !recorder.lock().await.path.is_empty(),
        "the request should have reached the Provider through the gateway"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A real request announces itself while it runs, not only when it finishes.
///
/// This is the channel the route graph draws from, so it has to exist for a
/// request the gateway actually served — the unit tests cover the table, this
/// covers the wiring from `prepare` to the ledger.
#[tokio::test]
async fn a_live_request_publishes_begin_and_end() {
    use std::sync::Mutex as StdMutex;

    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let seen: Arc<StdMutex<Vec<vibex_core::GatewayEvent>>> = Arc::new(StdMutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    service.set_event_sink(Arc::new(move |event| {
        sink.lock().unwrap().push(event);
    }));

    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);

    let events = seen.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, vibex_core::GatewayEvent::Began(_))),
        "a request that ran must have announced itself: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, vibex_core::GatewayEvent::Ended { .. })),
        "and must announce that it finished: {events:?}"
    );
    // One request, one id: the events describe the same thing the ledger row
    // does, so a client can follow it rather than guess.
    let first = events[0].request_id().clone();
    assert!(events.iter().all(|event| event.request_id() == &first));
    assert!(
        service.inflight_requests().is_empty(),
        "nothing may stay drawn once the reply is done"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A same-protocol call is forwarded as written, vendor fields and all.
///
/// Re-encoding a passthrough dropped every field the IR does not model —
/// `store`, `previous_response_id`, `prompt_cache_key`, `metadata`,
/// `text.format` — so a client that relied on the vendor's own conversation
/// state lost it, and the prompt cache paid for a body that was no longer the
/// one it had been primed with. Only `model` is the gateway's to rewrite.
#[tokio::test]
async fn a_same_protocol_call_is_forwarded_with_its_vendor_fields() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, responses_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["gpt-5"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "gpt-5"),
            "input": "say hi",
            "store": false,
            "previous_response_id": "resp_previous",
            "prompt_cache_key": "cache-key-1",
            "metadata": {"trace": "abc"},
            "text": {"format": {"type": "json_object"}},
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();

    let recorded = recorder.lock().await;
    let body = recorded.body.as_ref().unwrap();
    // The model is the gateway's to rewrite, and the group prefix is gone.
    assert_eq!(body["model"], "gpt-5");
    // Everything else is the client's own body, byte for byte.
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["previous_response_id"], "resp_previous");
    assert_eq!(body["prompt_cache_key"], "cache-key-1");
    assert_eq!(body["metadata"]["trace"], "abc");
    assert_eq!(body["text"]["format"]["type"], "json_object");

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A Gemini call names its model in the URL, never in the body.
///
/// The Google SDKs put the id in the path and send a body of `contents` alone,
/// so a gateway that only reads the body resolves an empty model and answers
/// `No configured Provider serves the model ""`. The path is the transport, so
/// the model has to be carried into the request before routing happens.
#[tokio::test]
async fn a_gemini_model_is_read_from_the_path() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, gemini_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::GeminiGenerateContent,
        &["gemini-2.5-pro"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    // No `model` in the body, and the slashes inside the group id escaped the
    // way the SDK escapes a path segment.
    let model = group_model(&group, "gemini-2.5-pro");
    let encoded = model.replace('/', "%2F");
    let response = reqwest::Client::new()
        .post(format!(
            "{gateway_url}/v1beta/models/{encoded}:streamGenerateContent?alt=sse"
        ))
        .header(header::HOST, "127.0.0.1")
        .header("x-goog-api-key", "vibex-local-gateway")
        .json(&json!({
            "contents": [{"role": "user", "parts": [{"text": "say hi"}]}],
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "the path model must route the call: {body}");
    // The action suffix is the client's stream flag, and the reply has to be
    // SSE: a `:streamGenerateContent` caller cannot read one JSON object.
    assert!(
        content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "a streaming Gemini call must answer SSE, not a whole object: {content_type:?}"
    );
    assert!(
        body.contains("data: "),
        "the reply must be SSE frames: {body}"
    );

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    assert_eq!(
        sent["model"], "gemini-2.5-pro",
        "the upstream body names the member's model, not the group path: {sent}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A vendor tool type the upstream cannot serve is dropped, not fatal.
///
/// Codex offers Responses' built-in `web_search`, which the Chat-backed upstream
/// rejects with `[code=unsupported_tool]`. Forwarding it killed the whole turn;
/// dropping it keeps every tool call the Agent can actually make.
#[tokio::test]
async fn a_vendor_tool_type_the_upstream_cannot_serve_is_dropped() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_input_validating_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["gpt-5"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "gpt-5"),
            "input": "say hi",
            "tools": [
                {"type": "function", "name": "exec", "parameters": {"type": "object"}},
                {"type": "web_search"},
                {"type": "web_search_preview"}
            ],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status, 200,
        "an unsupported vendor tool must not fail the turn: {body}"
    );

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    let tools = sent["tools"]
        .as_array()
        .expect("the supported tool is still forwarded");
    assert_eq!(tools.len(), 1, "{sent}");
    assert_eq!(tools[0]["type"], "function", "{sent}");
    assert_eq!(tools[0]["name"], "exec", "{sent}");

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// Antigravity's tool result under the `model` role still reaches the upstream
/// as a tool message.
///
/// Its ACP server wraps `functionResponse` in a `model`-role content. Reading
/// that role literally put the result inside an assistant message, so the
/// translated Responses body carried a `function_call` with no
/// `function_call_output` beside it and the upstream refused the whole turn
/// with `An assistant message with 'tool_calls' must be followed by tool
/// messages responding to each 'tool_call_id'`.
#[tokio::test]
async fn a_gemini_tool_result_under_the_model_role_answers_its_call() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, responses_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["deepseek-v4.1-flash"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let model = group_model(&group, "deepseek-v4.1-flash");
    let response = reqwest::Client::new()
        .post(format!(
            "{gateway_url}/v1beta/models/{}:streamGenerateContent?alt=sse",
            model.replace('/', "%2F")
        ))
        .header(header::HOST, "127.0.0.1")
        .header("x-goog-api-key", "vibex-local-gateway")
        .json(&json!({
            "contents": [
                {"role": "user", "parts": [{"text": "read the readme"}]},
                {"role": "model", "parts": [
                    {"functionCall": {"name": "read", "args": {"absolute_path": "/tmp/README.md"}}}
                ]},
                {"role": "model", "parts": [
                    {"functionResponse": {"name": "read", "response": {"content": "hi"}}}
                ]}
            ]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status, 200,
        "the tool result must pair with its call: {body}"
    );

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    let items = sent["input"].as_array().expect("input items");
    let call_id = items
        .iter()
        .find(|item| item["type"] == "function_call")
        .and_then(|item| item["call_id"].as_str())
        .expect("the tool call is translated");
    assert!(
        items
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == call_id),
        "the result has to answer the call it belongs to: {sent}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// The exact body Antigravity sends after a tool runs is accepted.
///
/// Captured from its ACP server: the result is a `model`-role content, and the
/// turn ends with a `model`-role text reply. Reading those roles literally put
/// the tool result inside an assistant turn, so the translated Responses body
/// had a `function_call` with no `function_call_output` and the upstream
/// refused it with `An assistant message with 'tool_calls' must be followed by
/// tool messages responding to each 'tool_call_id'`. The tool had already run,
/// so the turn died after doing its work.
#[tokio::test]
async fn antigravity_style_history_pairs_every_call_with_its_result() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) =
        mock_upstream(200, responses_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["deepseek-v4.1-flash"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let model = group_model(&group, "deepseek-v4.1-flash");
    let response = reqwest::Client::new()
        .post(format!(
            "{gateway_url}/v1beta/models/{}:streamGenerateContent?alt=sse",
            model.replace('/', "%2F")
        ))
        .header(header::HOST, "127.0.0.1")
        .header("x-goog-api-key", "vibex-local-gateway")
        .json(&json!({
            "contents": [
                {"role": "user", "parts": [{"text": "read the readme"}]},
                {"role": "model", "parts": [
                    {"functionCall": {"id": "07f9178c", "name": "read",
                                      "args": {"absolute_path": "/tmp/README.md"}}}
                ]},
                {"role": "model", "parts": [
                    {"functionResponse": {"id": "07f9178c", "name": "read",
                                          "response": {"content": "hi"}}}
                ]},
                {"role": "model", "parts": [{"text": "the readme says hi"}]}
            ]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status, 200,
        "a tool result the client wrapped in a model turn must still pair: {body}"
    );

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    let items = sent["input"].as_array().expect("input items");
    let call_id = items
        .iter()
        .find(|item| item["type"] == "function_call")
        .and_then(|item| item["call_id"].as_str())
        .expect("the tool call is translated");
    let output = items
        .iter()
        .position(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .expect("the result has to answer the call it belongs to");
    let call = items
        .iter()
        .position(|item| item["type"] == "function_call")
        .unwrap();
    assert!(
        output > call,
        "the answer must follow its call, not precede it: {sent}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A reasoning-depth model id resolves to the group's base model.
///
/// Antigravity's picker publishes one model per depth, so it asks the gateway
/// for `group/<id>/<model>-high`. The group holds the base model — a depth is a
/// property of one model rather than a second model — and the upstream has
/// never heard of the suffixed id. Treating the suffix as part of the model
/// answered `404 No configured Provider serves the model
/// 'group/.../deepseek-v4.1-flash-high'` on every turn of that session.
#[tokio::test]
async fn a_reasoning_depth_suffix_resolves_to_the_base_model() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-v4.1-flash"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": format!("{}-high", group_model(&group, "deepseek-v4.1-flash")),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status, 200,
        "a depth suffix must reach the base model, not be refused: {body}"
    );

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    assert_eq!(
        sent["model"], "deepseek-v4.1-flash",
        "the upstream is asked for the model it declares: {sent}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A Gemini tool name that the OpenAI protocols refuse is rewritten, and the
/// reply is translated back to the client's own name.
///
/// Gemini permits `.` and `:` in a function name — its MCP bridge rewrites only
/// `[^a-zA-Z0-9_\-.:]` — so a client forwards
/// `mcp_com.browser-use_browser-use_browser_click`. The Chat-backed upstream
/// rejects the whole request for it:
///
/// ```text
/// Invalid 'tools[36].function.name': string does not match pattern.
/// Expected a string that matches the pattern '^[a-zA-Z0-9_-]+$'
/// ```
///
/// Dropping the tool would take the capability away; forwarding the name loses
/// the turn. The gateway rewrites the name and restores it on the reply, or the
/// client would be handed a tool it never declared.
#[tokio::test]
async fn a_tool_name_the_target_protocol_refuses_is_rewritten_and_restored() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    // The upstream speaks Chat, so the Gemini call is translated.
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-v4.1-flash"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let model = group_model(&group, "deepseek-v4.1-flash");
    let response = reqwest::Client::new()
        .post(format!(
            "{gateway_url}/v1beta/models/{}:streamGenerateContent?alt=sse",
            model.replace('/', "%2F")
        ))
        .header(header::HOST, "127.0.0.1")
        .header("x-goog-api-key", "vibex-local-gateway")
        .json(&json!({
            "contents": [{"role": "user", "parts": [{"text": "click it"}]}],
            "tools": [{
                "functionDeclarations": [{
                    "name": "mcp_com.browser-use_browser-use_browser_click",
                    "description": "click",
                    "parametersJsonSchema": {"type": "OBJECT", "properties": {}}
                }]
            }],
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status, 200,
        "the translated call must not be refused: {body}"
    );

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    let forwarded = sent["tools"][0]["function"]["name"]
        .as_str()
        .expect("the tool is forwarded");
    assert_eq!(
        forwarded, "mcp_com_browser-use_browser-use_browser_click",
        "the upstream body must carry a name it accepts: {sent}"
    );
    assert!(
        forwarded
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')),
        "every forwarded name must satisfy the strict pattern: {forwarded}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// The reply names the tool the client declared, not the rewritten one.
///
/// The rewrite only works if it is reversible: the upstream echoes the name it
/// was given, and a client handed `mcp_com_browser-use_...` instead of its own
/// `mcp_com.browser-use...` would report an unknown tool and drop the call.
#[tokio::test]
async fn a_rewritten_tool_name_comes_back_as_the_clients_own() {
    let path = database_path();
    let stream = [
        r#"data: {"id":"c1","model":"deepseek-v4.1-flash","choices":[{"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"mcp_com_browser-use_browser-use_browser_click","arguments":""}}]}}]}"#,
        r#"data: {"id":"c1","model":"deepseek-v4.1-flash","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"data: {"id":"c1","model":"deepseek-v4.1-flash","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        "data: [DONE]",
    ]
    .join("\n\n")
        + "\n\n";
    let (upstream_url, _recorder, upstream_task) = mock_upstream(200, stream, None).await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-v4.1-flash"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let model = group_model(&group, "deepseek-v4.1-flash");
    let response = reqwest::Client::new()
        .post(format!(
            "{gateway_url}/v1beta/models/{}:streamGenerateContent?alt=sse",
            model.replace('/', "%2F")
        ))
        .header(header::HOST, "127.0.0.1")
        .header("x-goog-api-key", "vibex-local-gateway")
        .json(&json!({
            "contents": [{"role": "user", "parts": [{"text": "click it"}]}],
            "tools": [{
                "functionDeclarations": [{
                    "name": "mcp_com.browser-use_browser-use_browser_click",
                    "description": "click",
                    "parametersJsonSchema": {"type": "OBJECT", "properties": {}}
                }]
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("mcp_com.browser-use_browser-use_browser_click"),
        "the reply must restore the client's own tool name: {body}"
    );
    assert!(
        !body.contains("mcp_com_browser-use_browser-use_browser_click"),
        "the rewritten name must never reach the client: {body}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A request whose only tools were dropped loses the `tools` key entirely.
#[tokio::test]
async fn dropping_every_tool_removes_the_key() {
    let path = database_path();
    let (upstream_url, recorder, upstream_task) = mock_input_validating_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::Responses,
        &["gpt-5"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream.clone(), GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/responses"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "gpt-5"),
            "input": "say hi",
            "tools": [{"type": "web_search"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();

    let recorded = recorder.lock().await;
    let sent = recorded.body.as_ref().unwrap();
    assert!(
        sent.get("tools").is_none(),
        "an empty tool list must be removed, not sent: {sent}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A vendor's 400 body reaches the ledger.
///
/// The body names the field the vendor refused — "Invalid schema for function
/// 'read_file': ..." — and the Agent shows only its own generic wrapper. Without
/// this the ledger had a bare status and the reason existed nowhere the user
/// could look.
#[tokio::test]
async fn a_refused_upstream_body_is_recorded_with_its_message() {
    let path = database_path();
    let (upstream_url, upstream_task) = mock_schema_refusing_upstream().await;
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::GeminiGenerateContent,
        &["gemini-2.5-pro"],
        "sk-test",
    );
    let group = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let model = group_model(&group, "gemini-2.5-pro");
    let response = reqwest::Client::new()
        .post(format!(
            "{gateway_url}/v1beta/models/{model}:generateContent"
        ))
        .header(header::HOST, "127.0.0.1")
        .header("x-goog-api-key", "vibex-local-gateway")
        .json(&json!({
            "contents": [{"role": "user", "parts": [{"text": "read it"}]}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let _ = response.text().await.unwrap();

    let connection = open_database(&path).unwrap();
    let rows = ModelGatewayRequestLogRepository::recent(&connection, 5).unwrap();
    let row = rows.first().expect("the failure must be recorded");
    let route = row.route.as_ref().expect("the row carries a route trace");
    assert_eq!(
        route.error_code.as_deref(),
        Some("model_gateway_upstream_failed")
    );
    let message = route
        .error_message
        .as_deref()
        .unwrap_or_else(|| panic!("the vendor's own words must be kept: {route:#?}"));
    assert!(
        message.contains("STRING") && message.contains("anyOf"),
        "the message must be the vendor's refusal: {message}"
    );

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

async fn mock_schema_refusing_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/{*path}",
        post(move |_body: axum::body::Bytes| async move {
            let mut response = Response::new(Body::from(
                json!({"error": {"message":
                    "Invalid schema for function 'read_file': \"STRING\" is not valid under any of the schemas listed in the 'anyOf' key"
                }})
                .to_string(),
            ));
            *response.status_mut() = axum::http::StatusCode::BAD_REQUEST;
            response
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), task)
}

/// A refusal that never reached an upstream still lands in the ledger.
///
/// Without the row the ledger is empty at the moment the Agent shows an error,
/// which reads as "the request never happened" — the exact confusion that made
/// a path-model bug invisible for a whole session.
#[tokio::test]
async fn a_request_refused_before_routing_is_still_a_ledger_row() {
    let path = database_path();
    let config = ProviderConfigService::new(&path);
    let upstream = seed_upstream(
        &config,
        "http://127.0.0.1:1",
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-test",
    );
    let _ = point_gateway_at(&path, upstream, GatewayPolicy::default());
    let (service, gateway_url, gateway_task) = gateway(&path).await;

    let model = "group/model_route_missing/deepseek-chat";
    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let _ = response.text().await.unwrap();

    let connection = open_database(&path).unwrap();
    let rows = ModelGatewayRequestLogRepository::recent(&connection, 5).unwrap();
    let row = rows.first().expect("a refusal must be recorded");
    assert_eq!(row.outcome, GatewayRequestOutcome::Rejected);
    assert_eq!(row.model_requested, model);
    assert_eq!(row.wire_in, "openai_chat_completions");
    assert_eq!(
        row.route
            .as_ref()
            .and_then(|route| route.error_code.as_deref()),
        Some("model_not_found"),
        "the row must name the gateway's own refusal: {:?}",
        row.route
    );

    gateway_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A model group may name different upstream models per member, and a failure
/// on the first walks to the second even though the requested model never
/// existed at either vendor under that name.
#[tokio::test]
async fn a_model_group_falls_back_across_different_upstream_models() {
    let path = database_path();
    let (left_url, left_recorder, left_task) = mock_upstream(503, String::new(), None).await;
    let (right_url, right_recorder, right_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let left = seed_upstream(
        &config,
        &left_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-left",
    );
    let right = seed_upstream(
        &config,
        &right_url,
        WireProtocol::ChatCompletions,
        &["kimi-k2"],
        "sk-right",
    );
    let group = seed_model_group(
        &path,
        "Mixed",
        "vibex-fast",
        &[(left, "deepseek-chat"), (right, "kimi-k2")],
        Vec::new(),
    );

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "vibex-fast"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "the group must answer via its second member"
    );
    let body = response.text().await.unwrap();
    assert!(
        body.contains("hello"),
        "the second member's stream must arrive"
    );

    assert_eq!(
        left_recorder
            .lock()
            .await
            .body
            .as_ref()
            .and_then(|body| body["model"].as_str()),
        Some("deepseek-chat"),
        "the first member must be asked for its own upstream model"
    );
    assert_eq!(
        right_recorder
            .lock()
            .await
            .body
            .as_ref()
            .and_then(|body| body["model"].as_str()),
        Some("kimi-k2"),
        "the second member must be asked for its own upstream model"
    );

    gateway_task.abort();
    left_task.abort();
    right_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A matching rule reorders the members for that request and leaves the
/// decision explainable in the ledger.
#[tokio::test]
async fn a_model_group_rule_chooses_the_first_member_and_is_traced() {
    let path = database_path();
    let (left_url, left_recorder, left_task) = mock_upstream(200, chat_stream(), None).await;
    let (right_url, right_recorder, right_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let left = seed_upstream(
        &config,
        &left_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-left",
    );
    let right = seed_upstream(
        &config,
        &right_url,
        WireProtocol::ChatCompletions,
        &["kimi-k2"],
        "sk-right",
    );
    let first = vibex_core::ModelRouteMember::new(left, "deepseek-chat", 0);
    let second = vibex_core::ModelRouteMember::new(right, "kimi-k2", 1);
    let rule = vibex_core::ModelRouteRule {
        id: "claude-rule".to_string(),
        use_member_id: Some(second.id.clone()),
        agents: vec!["claude".to_string()],
        ..Default::default()
    };
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: "Ruled".to_string(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some("vibex-ruled".to_string()),
        status: vibex_core::RouteStatus::Enabled,
        rules: vec![rule],
        members: vec![first, second],
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    drop(connection);

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .header("x-vibex-agent", "claude")
        .json(&json!({
            "model": group_model(&route.id, "vibex-ruled"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);
    assert!(
        left_recorder.lock().await.path.is_empty(),
        "the rule must skip the configured first member"
    );
    assert_eq!(
        right_recorder
            .lock()
            .await
            .body
            .as_ref()
            .and_then(|body| body["model"].as_str()),
        Some("kimi-k2")
    );

    let connection = open_database(&path).unwrap();
    let rows = ModelGatewayRequestLogRepository::recent(&connection, 5).unwrap();
    let row = rows.first().expect("the request must be recorded");
    let decision = row
        .route
        .as_ref()
        .and_then(|trace| trace.decision.as_ref())
        .expect("a rule decision must be traced");
    assert_eq!(decision.rule_id.as_deref(), Some("claude-rule"));
    assert_eq!(decision.rule_index, Some(0));
    assert_eq!(decision.reason.as_deref(), Some("rule"));

    gateway_task.abort();
    left_task.abort();
    right_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A stalled member is abandoned for the next one, before any body byte
/// reaches the client.
///
/// This is the case error-based failover cannot see: the connection succeeded,
/// the status is 200, and the vendor then says nothing. The last member is
/// deliberately not tested here because it has nowhere to fail over to and is
/// supposed to wait.
#[tokio::test]
async fn a_first_token_timeout_fails_over_to_the_next_member() {
    let path = database_path();
    let (silent_url, silent_task) = mock_silent_upstream().await;
    let (fast_url, fast_recorder, fast_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let silent = seed_upstream(
        &config,
        &silent_url,
        WireProtocol::ChatCompletions,
        &["slow-model"],
        "sk-slow",
    );
    let fast = seed_upstream(
        &config,
        &fast_url,
        WireProtocol::ChatCompletions,
        &["fast-model"],
        "sk-fast",
    );
    let mut first = vibex_core::ModelRouteMember::new(silent, "slow-model", 0);
    first.first_token_timeout_ms = Some(150);
    let second = vibex_core::ModelRouteMember::new(fast, "fast-model", 1);
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: "Stall".to_string(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some("vibex-stall".to_string()),
        status: vibex_core::RouteStatus::Enabled,
        members: vec![first, second],
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    drop(connection);

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let response = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&route.id, "vibex-stall"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains("hello"), "the second member must answer");
    assert_eq!(
        fast_recorder
            .lock()
            .await
            .body
            .as_ref()
            .and_then(|body| body["model"].as_str()),
        Some("fast-model")
    );

    gateway_task.abort();
    silent_task.abort();
    fast_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A recoverable failure is retried under the route's patience budget.
#[tokio::test]
async fn patience_retries_a_retryable_failure_before_giving_up() {
    let path = database_path();
    let (flaky_url, attempts, flaky_task) = mock_flaky_upstream(1).await;
    let config = ProviderConfigService::new(&path);
    let provider = seed_upstream(
        &config,
        &flaky_url,
        WireProtocol::ChatCompletions,
        &["flaky-model"],
        "sk-flaky",
    );
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: "Patient".to_string(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some("vibex-patient".to_string()),
        status: vibex_core::RouteStatus::Enabled,
        patience_ms: Some(15_000),
        members: vec![vibex_core::ModelRouteMember::new(
            provider,
            "flaky-model",
            0,
        )],
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    drop(connection);

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&route.id, "vibex-patient"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200, "the retry must answer after the first 503");
    assert!(
        attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "patience must actually call the upstream again"
    );

    gateway_task.abort();
    flaky_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A configured classifier is called once, its intent matches a rule, and its
/// effort reaches the ledger decision.
#[tokio::test]
async fn a_classifier_supplies_intent_and_automatic_effort() {
    let path = database_path();
    let classifier_reply = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "{\"intent\":\"code\",\"effort\":\"high\"}"
            }
        }]
    });
    let (classifier_url, classifier_recorder, classifier_task) =
        mock_upstream(200, String::new(), Some(classifier_reply)).await;
    let (main_url, main_recorder, main_task) = mock_upstream(200, chat_stream(), None).await;
    let config = ProviderConfigService::new(&path);
    let classifier = seed_upstream(
        &config,
        &classifier_url,
        WireProtocol::ChatCompletions,
        &["classifier-model"],
        "sk-classifier",
    );
    let main = seed_upstream(
        &config,
        &main_url,
        WireProtocol::ChatCompletions,
        &["main-model"],
        "sk-main",
    );
    let member = vibex_core::ModelRouteMember::new(main, "main-model", 0);
    let rule = vibex_core::ModelRouteRule {
        id: "code-rule".to_string(),
        use_member_id: Some(member.id.clone()),
        intent: Some("code".to_string()),
        ..Default::default()
    };
    let mut connection = open_database(&path).unwrap();
    apply_migrations(&mut connection).unwrap();
    let route = vibex_core::ModelRoute {
        display_name: "Classified".to_string(),
        kind: vibex_core::RouteKind::ModelGroup,
        agent_model_id: Some("vibex-classified".to_string()),
        status: vibex_core::RouteStatus::Enabled,
        classifier: Some(format!("{}/classifier-model", classifier.as_str())),
        effort: Some("auto".to_string()),
        rules: vec![rule],
        members: vec![member],
        ..Default::default()
    };
    vibex_db::ModelRouteRepository::insert(&connection, &route).unwrap();
    drop(connection);

    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let status = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&route.id, "vibex-classified"),
            "messages": [{"role": "user", "content": "fix this failing test"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);
    assert!(
        !classifier_recorder.lock().await.path.is_empty(),
        "the classifier must be asked"
    );
    assert!(
        !main_recorder.lock().await.path.is_empty(),
        "the main member must answer"
    );

    let connection = open_database(&path).unwrap();
    let rows = ModelGatewayRequestLogRepository::recent(&connection, 5).unwrap();
    let decision = rows
        .first()
        .and_then(|row| row.route.as_ref())
        .and_then(|trace| trace.decision.as_ref())
        .expect("the classifier decision must be traced");
    assert_eq!(
        decision.classifier.as_deref(),
        Some(format!("{}/classifier-model", classifier.as_str()).as_str())
    );
    assert_eq!(decision.intent.as_deref(), Some("code"));
    assert_eq!(decision.effort.as_deref(), Some("high"));
    assert_eq!(decision.rule_id.as_deref(), Some("code-rule"));

    gateway_task.abort();
    classifier_task.abort();
    main_task.abort();
    service.shutdown().await;
    cleanup(path);
}

/// A configured cooldown override replaces the built-in duration.
#[tokio::test]
async fn a_cooldown_override_shortens_the_key_rest() {
    let path = database_path();
    let (upstream_url, _recorder, upstream_task) = mock_upstream(503, String::new(), None).await;
    let config = ProviderConfigService::new(&path);
    let provider = seed_upstream(
        &config,
        &upstream_url,
        WireProtocol::ChatCompletions,
        &["deepseek-chat"],
        "sk-rate",
    );
    let group = point_gateway_at(
        &path,
        provider.clone(),
        GatewayPolicy {
            cooldown_overrides_ms: std::collections::BTreeMap::from([(
                "upstream".to_string(),
                1_500,
            )]),
            ..GatewayPolicy::default()
        },
    );
    let (service, gateway_url, gateway_task) = gateway(&path).await;
    let _ = reqwest::Client::new()
        .post(format!("{gateway_url}/v1/chat/completions"))
        .header(header::HOST, "127.0.0.1")
        .json(&json!({
            "model": group_model(&group, "deepseek-chat"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    let connection = open_database(&path).unwrap();
    let states = ModelProviderAccountStateRepository::list(&connection, &provider).unwrap();
    let state = states.first().expect("the failed Key must have a state");
    let now = vibex_core::unix_timestamp_ms();
    let remaining = state.cooldown_until_ms.unwrap_or_default() - now;
    assert!(
        (0..=3_000).contains(&remaining),
        "the override must replace the ten-second default, got {remaining} ms"
    );
    assert_eq!(state.cooldown_reason, Some(GatewayFailureKind::Upstream));

    gateway_task.abort();
    upstream_task.abort();
    service.shutdown().await;
    cleanup(path);
}

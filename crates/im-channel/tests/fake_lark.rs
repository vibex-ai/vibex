//! End-to-end test of the Lark channel against a fake Lark open platform.
//!
//! The fake server speaks the parts of the contract the adapter uses: the tenant
//! access token exchange, the long-connection endpoint handshake, event frames
//! over the websocket, and the message send API. Approvals are exercised through
//! the text fallback the Lark adapter uses instead of card callbacks.
//!
//! No real Lark tenant or network access is involved.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::{get, post, put},
};
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast};
use vibex_core::{
    AgentId, AgentNotificationIntent, AgentSession, AgentSessionRuntimeSelectionState,
    AgentSessionSafety, AgentSessionState, CreateAgentSessionRequest, FetchTimelineRequest,
    ImChannelConfig, ImChannelDraft, ImLarkConfig, ImLarkTransport, PermissionActionDetail,
    PermissionRequest, PermissionRequestStatus, PermissionResolution, PermissionResponseKind,
    PermissionResponseOption, ProjectId, ProviderProfileId, ResolveElicitationRequest,
    ResolvePermissionRequest, SendAgentMessageRequest, SessionRuntimeSelection,
    SessionRuntimeSelectionStatus, TimelineItem, TimelineItemId, TimelineLiveEvent, TimelinePage,
    TimelinePayload, TimelinePayloadDepth, TimelineRedactionState, TimelineSource, VibexResult,
    VibexSessionId, WorkspaceId, WorkspaceMode, unix_timestamp_ms,
};
use vibex_im_channel::{ImAgentPort, ImChannelService, ImChannelServiceConfig};

struct FakeLark {
    ws_url: Mutex<String>,
    events: Mutex<VecDeque<Value>>,
    sent: Mutex<Vec<Value>>,
    edited: Mutex<Vec<Value>>,
    notify: Notify,
}

impl FakeLark {
    fn new() -> Self {
        Self {
            ws_url: Mutex::new(String::new()),
            events: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            edited: Mutex::new(Vec::new()),
            notify: Notify::new(),
        }
    }

    fn push(&self, event: Value) {
        self.events.lock().expect("events").push_back(event);
        self.notify.notify_waiters();
    }

    /// Every outbound body flattened to the text a reader would see.
    fn sent_texts(&self) -> Vec<String> {
        self.sent
            .lock()
            .expect("sent")
            .iter()
            .filter_map(|body| {
                let content = body.get("content").and_then(Value::as_str)?;
                let content: Value = serde_json::from_str(content).ok()?;
                if let Some(text) = content.get("text").and_then(Value::as_str) {
                    return Some(text.to_string());
                }
                // An interactive card carries its prose in a div element.
                let elements = content.get("elements")?.as_array()?;
                let mut rendered = String::new();
                for element in elements {
                    if let Some(text) = element.pointer("/text/content").and_then(Value::as_str) {
                        rendered.push_str(text);
                    }
                    if let Some(actions) = element.get("actions").and_then(Value::as_array) {
                        for action in actions {
                            if let Some(label) =
                                action.pointer("/text/content").and_then(Value::as_str)
                            {
                                rendered.push('[');
                                rendered.push_str(label);
                                rendered.push(']');
                            }
                        }
                    }
                }
                Some(rendered)
            })
            .collect()
    }

    /// The value a card button sends back when pressed.
    fn first_card_action(&self) -> Option<String> {
        self.sent.lock().expect("sent").iter().find_map(|body| {
            let content = body.get("content").and_then(Value::as_str)?;
            let content: Value = serde_json::from_str(content).ok()?;
            content
                .get("elements")?
                .as_array()?
                .iter()
                .find_map(|element| {
                    element
                        .get("actions")?
                        .as_array()?
                        .first()?
                        .pointer("/value/vibex_action")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
    }
}

async fn fake_lark(
    State(state): State<Arc<FakeLark>>,
    Path(path): Path<String>,
    Query(_query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    match path.as_str() {
        "open-apis/auth/v3/tenant_access_token/internal" => {
            assert_eq!(body.get("app_id").and_then(Value::as_str), Some("cli_test"));
            assert_eq!(
                body.get("app_secret").and_then(Value::as_str),
                Some("secret")
            );
            Json(json!({ "code": 0, "tenant_access_token": "t-fake", "expire": 7200 }))
        }
        "callback/ws/endpoint" => Json(json!({
            "code": 0,
            "data": { "URL": state.ws_url.lock().expect("ws url").clone() }
        })),
        "open-apis/im/v1/messages" => {
            state.sent.lock().expect("sent").push(body);
            Json(json!({ "code": 0, "data": { "message_id": "om_sent" } }))
        }
        other if other.starts_with("open-apis/im/v1/messages/") => {
            state.edited.lock().expect("edited").push(body);
            Json(json!({ "code": 0, "data": {} }))
        }
        other => Json(json!({ "code": 404, "msg": format!("unhandled path {other}") })),
    }
}

/// Lark serves chat metadata and message resources on GET.
async fn fake_lark_get(
    State(_state): State<Arc<FakeLark>>,
    Path(path): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if path.contains("/resources/") {
        return vec![0xff, 0xd8, 0xff, 0xe0, 0x00].into_response();
    }
    Json(json!({ "code": 0, "data": { "name": "Fake Lark chat" } })).into_response()
}

async fn ws_handler(
    State(state): State<Arc<FakeLark>>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| async move {
        let _ = serve_ws(socket, state).await;
    })
}

async fn serve_ws(mut socket: WebSocket, state: Arc<FakeLark>) -> Result<(), ()> {
    loop {
        let events: Vec<Value> = state.events.lock().expect("events").drain(..).collect();
        if events.is_empty() {
            let _ = tokio::time::timeout(Duration::from_millis(50), state.notify.notified()).await;
            continue;
        }
        for event in events {
            let payload = serde_json::to_vec(&event).map_err(|_| ())?;
            let frame = encode_frame(&payload);
            if socket.send(Message::Binary(frame.into())).await.is_err() {
                return Ok(());
            }
        }
    }
}

/// Encodes the long-connection envelope: method=1, payload_type="event", payload.
fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    // protobuf field 4, wire type 0 (varint)
    put_varint(4 << 3, &mut bytes);
    put_varint(1, &mut bytes);
    put_varint((7 << 3) | 2, &mut bytes);
    put_varint(5, &mut bytes);
    bytes.extend_from_slice(b"event");
    put_varint((8 << 3) | 2, &mut bytes);
    put_varint(payload.len() as u64, &mut bytes);
    bytes.extend_from_slice(payload);
    bytes
}

fn put_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

struct MockAgent {
    timeline: broadcast::Sender<TimelineLiveEvent>,
    sessions: broadcast::Sender<AgentSession>,
    notifications: broadcast::Sender<AgentNotificationIntent>,
    session: AgentSession,
    selection: SessionRuntimeSelection,
    pending: Mutex<Option<PermissionRequest>>,
    prompts: Mutex<Vec<String>>,
    attachments: Mutex<Vec<Vec<vibex_core::MessageAttachment>>>,
    resolutions: Mutex<Vec<PermissionResolution>>,
}

impl MockAgent {
    fn new() -> Self {
        let (timeline, _) = broadcast::channel(32);
        let (sessions, _) = broadcast::channel(32);
        let (notifications, _) = broadcast::channel(32);
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Lark session".to_string(),
            project_id: ProjectId::new(),
            workspace_id: WorkspaceId::new(),
            workspace_root: "/tmp/fake-lark-workspace".to_string(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("claude").expect("agent id"),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: Default::default(),
            transport: Default::default(),
            terminal: None,
        };
        let selection = SessionRuntimeSelection::provider(
            session.agent_id.clone(),
            ProviderProfileId::new(),
            "fake-model",
        );
        Self {
            timeline,
            sessions,
            notifications,
            session,
            selection,
            pending: Mutex::new(None),
            prompts: Mutex::new(Vec::new()),
            attachments: Mutex::new(Vec::new()),
            resolutions: Mutex::new(Vec::new()),
        }
    }

    fn item(&self, sequence: i64, payload: TimelinePayload) -> TimelineItem {
        TimelineItem {
            id: TimelineItemId::new(),
            session_id: self.session.id.clone(),
            sequence,
            timestamp_ms: unix_timestamp_ms(),
            source: TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: TimelinePayloadDepth::Detail,
            payload,
        }
    }
}

#[async_trait]
impl ImAgentPort for MockAgent {
    fn subscribe_timeline(&self) -> broadcast::Receiver<TimelineLiveEvent> {
        self.timeline.subscribe()
    }

    fn subscribe_session_updates(&self) -> broadcast::Receiver<AgentSession> {
        self.sessions.subscribe()
    }

    fn subscribe_notifications(&self) -> broadcast::Receiver<AgentNotificationIntent> {
        self.notifications.subscribe()
    }

    async fn get_session(&self, _session_id: &VibexSessionId) -> VibexResult<AgentSession> {
        Ok(self.session.clone())
    }

    async fn list_sessions(&self, _include_archived: bool) -> VibexResult<Vec<AgentSession>> {
        Ok(vec![self.session.clone()])
    }

    async fn fetch_timeline(&self, _request: FetchTimelineRequest) -> VibexResult<TimelinePage> {
        let items = self
            .pending
            .lock()
            .expect("pending")
            .clone()
            .map(|request| vec![self.item(7, TimelinePayload::PermissionRequest(request))])
            .unwrap_or_default();
        Ok(TimelinePage {
            session_id: self.session.id.clone(),
            items,
            start_sequence: Some(7),
            end_sequence: Some(7),
            has_older: false,
            has_newer: false,
            starts_at_turn_start: true,
            leading_turn_start: None,
            omitted: None,
        })
    }

    async fn runtime_selection(
        &self,
        _session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        Ok(AgentSessionRuntimeSelectionState {
            desired: self.selection.clone(),
            effective: self.selection.clone(),
            status: SessionRuntimeSelectionStatus::Ready,
            session_revision: 1,
            selection_revision: 1,
            current_binding_id: None,
            activation_generation: 1,
            pending_switch_id: None,
            actionable_error: None,
        })
    }

    async fn create_session(
        &self,
        _request: CreateAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        Ok(self.session.clone())
    }

    async fn send_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        self.prompts.lock().expect("prompts").push(request.text);
        self.attachments
            .lock()
            .expect("attachments")
            .push(request.attachments);
        Ok(Vec::new())
    }

    async fn interrupt(&self, _session_id: &VibexSessionId) -> VibexResult<()> {
        Ok(())
    }

    async fn resolve_permission(
        &self,
        request: ResolvePermissionRequest,
    ) -> VibexResult<TimelineItem> {
        self.resolutions
            .lock()
            .expect("resolutions")
            .push(request.resolution.clone());
        Ok(self.item(8, TimelinePayload::PermissionResolution(request.resolution)))
    }

    async fn resolve_elicitation(
        &self,
        _request: ResolveElicitationRequest,
    ) -> VibexResult<TimelineItem> {
        Err(vibex_core::VibexError::process(
            "fake_resolve_elicitation",
            "not used by this test",
        ))
    }
}

fn message_event(event_id: &str, message_id: &str, chat_type: &str, text: &str) -> Value {
    json!({
        "header": { "event_type": "im.message.receive_v1", "event_id": event_id },
        "event": {
            "sender": {
                "sender_type": "user",
                "sender_id": { "open_id": "ou_user" }
            },
            "message": {
                "message_id": message_id,
                "chat_id": "oc_chat",
                "chat_type": chat_type,
                "message_type": "text",
                "content": json!({ "text": text }).to_string(),
                "mentions": []
            }
        }
    })
}

async fn wait_for<F, Fut>(mut condition: F, label: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {label}");
}

fn seed_session(conn: &rusqlite::Connection, agent: &MockAgent) {
    let now = unix_timestamp_ms();
    conn.execute(
        "INSERT INTO projects (project_id, name, root_path, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        rusqlite::params![
            agent.session.project_id.as_str(),
            "Lark project",
            agent.session.workspace_root,
            now
        ],
    )
    .expect("project");
    conn.execute(
        "INSERT INTO workspaces (workspace_id, project_id, root_path, mode, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        rusqlite::params![
            agent.session.workspace_id.as_str(),
            agent.session.project_id.as_str(),
            agent.session.workspace_root,
            "current_checkout",
            now
        ],
    )
    .expect("workspace");
    conn.execute(
        "INSERT INTO agent_sessions (
            session_id, title, project_id, workspace_id, workspace_root, workspace_mode,
            state, permission_mode, ask_on_risk, bypass_all_permissions,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        rusqlite::params![
            agent.session.id.as_str(),
            agent.session.title,
            agent.session.project_id.as_str(),
            agent.session.workspace_id.as_str(),
            agent.session.workspace_root,
            "current_checkout",
            "idle",
            "workspace_write",
            1,
            0,
            now
        ],
    )
    .expect("session");
}

fn seed_pairing(conn: &rusqlite::Connection, channel_id: &str, agent: &MockAgent) {
    let now = unix_timestamp_ms();
    conn.execute(
        "INSERT INTO im_principals (
            principal_id, channel_id, external_user_id, display_name, role, status,
            paired_at_ms, last_seen_at_ms, revoked_at_ms, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, 'ou_user', 'Lark tester', 'operator', 'active', ?3, ?3, NULL, ?3, ?3)",
        rusqlite::params![vibex_core::ImPrincipalId::new().as_str(), channel_id, now],
    )
    .expect("principal");
    conn.execute(
        "INSERT INTO im_bindings (
            binding_id, channel_id, external_chat_id, thread_key, thread_kind, session_id,
            created_by_principal_id, status, last_delivered_sequence, title,
            title_sync_enabled, provider_payload_json, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, 'oc_chat', '', NULL, ?3, NULL, 'active', 0, ?4, 0, NULL, ?5, ?5)",
        rusqlite::params![
            vibex_core::ImBindingId::new().as_str(),
            channel_id,
            agent.session.id.as_str(),
            agent.session.title,
            now
        ],
    )
    .expect("binding");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lark_long_connection_round_trip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let home = dir.path().to_path_buf();
    let db_path = home.join("vibex.db");
    vibex_config_switch::secrets::configure_provider_secret_store(
        vibex_config_switch::secrets::ProviderSecretStoreConfig::host_file(&home),
    );
    let mut conn = vibex_db::open_database(&db_path).expect("db");
    vibex_db::apply_migrations(&mut conn).expect("migrate");

    let agent = Arc::new(MockAgent::new());
    seed_session(&conn, &agent);
    drop(conn);

    let fake = Arc::new(FakeLark::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    *fake.ws_url.lock().expect("ws url") = format!("ws://{address}/ws");
    let router = Router::new()
        .route("/ws", get(ws_handler))
        .route("/{*path}", get(fake_lark_get))
        .route("/{*path}", post(fake_lark))
        .route("/{*path}", put(fake_lark))
        .with_state(Arc::clone(&fake));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake server");
    });

    let agent_port: Arc<dyn ImAgentPort> = agent.clone();
    let service = Arc::new(
        ImChannelService::new(ImChannelServiceConfig {
            db_path: db_path.clone(),
            home_dir: home,
            agent: agent_port,
            telegram_api_base: None,
            lark_api_base: Some(format!("http://{address}")),
            locale: Some("en".to_string()),
        })
        .expect("service"),
    );
    service.start().await.expect("start");
    let channel = service
        .create_channel(ImChannelDraft {
            display_name: "Fake Lark".to_string(),
            config: ImChannelConfig::Lark(ImLarkConfig {
                app_id: "cli_test".to_string(),
                chat_id: Some("oc_chat".to_string()),
                transport: ImLarkTransport::LongConnection,
            }),
            default_session: None,
            event_filter: Vec::new(),
            locale: Some("en".to_string()),
        })
        .await
        .expect("create channel");
    assert_eq!(channel.kind, vibex_core::ImChannelKind::Lark);
    service
        .set_app_secret(&channel.channel_id, "secret")
        .await
        .expect("secret");
    service
        .set_channel_enabled(&channel.channel_id, true)
        .await
        .expect("enable");
    tokio::time::sleep(Duration::from_millis(400)).await;

    // An unpaired direct message is refused, which proves the event arrived
    // over the websocket and the reply went back out through the send API.
    fake.push(message_event("evt-1", "om_1", "p2p", "hello"));
    wait_for(
        || async {
            fake.sent_texts()
                .iter()
                .any(|text| text.contains("not paired"))
        },
        "unpaired reply",
    )
    .await;

    let conn = rusqlite::Connection::open(&db_path).expect("db");
    seed_pairing(&conn, channel.channel_id.as_str(), &agent);
    drop(conn);

    fake.push(message_event("evt-2", "om_2", "p2p", "hello vibex"));
    wait_for(
        || async { !agent.prompts.lock().expect("prompts").is_empty() },
        "prompt",
    )
    .await;
    assert_eq!(
        agent
            .prompts
            .lock()
            .expect("prompts")
            .first()
            .map(String::as_str),
        Some("hello vibex")
    );

    // An image message is one prompt carrying one downloaded file.
    fake.push(json!({
        "header": { "event_type": "im.message.receive_v1", "event_id": "evt-image" },
        "event": {
            "sender": { "sender_type": "user", "sender_id": { "open_id": "ou_user" } },
            "message": {
                "message_id": "om_image",
                "chat_id": "oc_chat",
                "chat_type": "p2p",
                "message_type": "image",
                "content": json!({ "image_key": "img_1", "file_key": "file_1" }).to_string(),
                "mentions": []
            }
        }
    }));
    wait_for(
        || async { agent.attachments.lock().expect("attachments").len() >= 2 },
        "image prompt",
    )
    .await;
    let sent = agent
        .attachments
        .lock()
        .expect("attachments")
        .last()
        .cloned()
        .expect("image prompt");
    assert_eq!(sent.len(), 1, "one image is one attachment");
    assert!(
        sent[0]
            .uri
            .as_deref()
            .is_some_and(|uri| uri.starts_with("data:") && uri.contains(";base64,")),
        "image bytes travel as a data URL"
    );

    // An approval goes out as an interactive card with one button per option.
    let request = PermissionRequest {
        id: vibex_core::RequestId::new(),
        session_id: agent.session.id.clone(),
        project_id: Some(agent.session.project_id.clone()),
        workspace_id: Some(agent.session.workspace_id.clone()),
        provider_request_id: Some("lark-request".to_string()),
        risk_category: vibex_core::PermissionRiskCategory::Command,
        title: "Run fake command".to_string(),
        details: vec![PermissionActionDetail {
            label: "Command".to_string(),
            value: "cargo test".to_string(),
        }],
        allowed_responses: vec![
            PermissionResponseKind::Approve,
            PermissionResponseKind::Deny,
        ],
        response_options: vec![
            PermissionResponseOption {
                option_id: "allow".to_string(),
                label: "Allow".to_string(),
                response: PermissionResponseKind::Approve,
            },
            PermissionResponseOption {
                option_id: "deny".to_string(),
                label: "Deny".to_string(),
                response: PermissionResponseKind::Deny,
            },
        ],
        status: PermissionRequestStatus::Pending,
        requested_at_ms: unix_timestamp_ms(),
        expires_at_ms: None,
    };
    *agent.pending.lock().expect("pending") = Some(request.clone());
    agent
        .timeline
        .send(TimelineLiveEvent {
            session_id: agent.session.id.clone(),
            sequence: 7,
            item: agent.item(7, TimelinePayload::PermissionRequest(request)),
        })
        .expect("timeline");
    wait_for(
        || async {
            fake.sent_texts()
                .iter()
                .any(|text| text.contains("Run fake command"))
        },
        "approval card",
    )
    .await;
    let approved_by_button = fake.first_card_action().expect("card action");
    assert!(
        fake.sent
            .lock()
            .expect("sent")
            .iter()
            .any(|body| body.get("msg_type").and_then(Value::as_str) == Some("interactive")),
        "approval was sent as a card"
    );

    // A card press arrives as its own event carrying the button value.
    fake.push(json!({
        "header": { "event_type": "card.action.trigger", "event_id": "evt-3" },
        "event": {
            "operator": { "operator_id": { "open_id": "ou_user" } },
            "action": { "value": { "vibex_action": approved_by_button } },
            "context": { "open_chat_id": "oc_chat" }
        }
    }));
    wait_for(
        || async { !agent.resolutions.lock().expect("resolutions").is_empty() },
        "approval resolution",
    )
    .await;
    let resolution = agent
        .resolutions
        .lock()
        .expect("resolutions")
        .first()
        .cloned()
        .expect("resolution");
    assert_eq!(resolution.response, PermissionResponseKind::Approve);
    assert_eq!(resolution.provider_resolution_id.as_deref(), Some("allow"));

    wait_for(
        || async {
            fake.edited.lock().expect("edited").iter().any(|body| {
                body.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| content.contains("approved"))
            })
        },
        "approval card edit",
    )
    .await;

    service.stop().await;
    server.abort();
}

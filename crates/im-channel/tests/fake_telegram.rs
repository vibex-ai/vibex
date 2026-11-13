//! End-to-end test of the Telegram channel against a fake Bot API.
//!
//! The fake server exercises the same HTTP contract as Telegram: long polling,
//! `sendMessage` buttons, callback acknowledgements, and `editMessageText` on a
//! resolved approval. No real bot token or network access is involved.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::post,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast};
use vibex_core::{
    AgentId, AgentNotificationIntent, AgentSession, AgentSessionRuntimeSelectionState,
    AgentSessionSafety, AgentSessionState, CreateAgentSessionRequest, FetchTimelineRequest,
    ImBindingSummary, ImChannelConfig, ImChannelDraft, ImPairingRequest, ImPrincipalRole,
    ImPrincipalStatus, PermissionActionDetail, PermissionRequest, PermissionRequestStatus,
    PermissionResolution, PermissionResponseKind, PermissionResponseOption, ProjectId,
    ProviderProfileId, ResolveElicitationRequest, ResolvePermissionRequest,
    SendAgentMessageRequest, SessionRuntimeSelection, SessionRuntimeSelectionStatus, TimelineItem,
    TimelineItemId, TimelineLiveEvent, TimelinePage, TimelinePayload, TimelinePayloadDepth,
    TimelineRedactionState, TimelineSource, VibexResult, VibexSessionId, WorkspaceId,
    WorkspaceMode, unix_timestamp_ms,
};
use vibex_im_channel::{ImAgentPort, ImChannelService, ImChannelServiceConfig};

struct FakeTelegram {
    updates: Mutex<VecDeque<Value>>,
    sent: Mutex<Vec<Value>>,
    edited: Mutex<Vec<Value>>,
    callbacks: Mutex<Vec<Value>>,
    topics: Mutex<Vec<Value>>,
    topic_edits: Mutex<Vec<Value>>,
    notify: Notify,
    next_message_id: AtomicI64,
}

impl FakeTelegram {
    fn new() -> Self {
        Self {
            updates: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            edited: Mutex::new(Vec::new()),
            callbacks: Mutex::new(Vec::new()),
            topics: Mutex::new(Vec::new()),
            topic_edits: Mutex::new(Vec::new()),
            notify: Notify::new(),
            next_message_id: AtomicI64::new(1),
        }
    }

    fn push(&self, update: Value) {
        self.updates.lock().expect("updates").push_back(update);
        self.notify.notify_waiters();
    }

    fn sent_texts(&self) -> Vec<String> {
        self.sent
            .lock()
            .expect("sent")
            .iter()
            .filter_map(|body| body.get("text").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    fn callback_data(&self) -> Option<String> {
        self.sent.lock().expect("sent").iter().find_map(|body| {
            body.pointer("/reply_markup/inline_keyboard/0/0/callback_data")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    }
}

async fn fake_telegram(
    State(state): State<Arc<FakeTelegram>>,
    Path((_token, method)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let result = match method.as_str() {
        "getMe" => json!({ "username": "fake_vibex_bot", "first_name": "Fake" }),
        "getChat" => json!({ "id": 42, "type": "private", "title": "Fake DM" }),
        "getUpdates" => {
            if state.updates.lock().expect("updates").is_empty() {
                let _ =
                    tokio::time::timeout(Duration::from_millis(250), state.notify.notified()).await;
            }
            let updates: Vec<Value> = state.updates.lock().expect("updates").drain(..).collect();
            json!(updates)
        }
        "sendMessage" => {
            state.sent.lock().expect("sent").push(body);
            let id = state.next_message_id.fetch_add(1, Ordering::Relaxed);
            json!({ "message_id": id })
        }
        "editMessageText" => {
            state.edited.lock().expect("edited").push(body);
            Value::Bool(true)
        }
        "answerCallbackQuery" => {
            state.callbacks.lock().expect("callbacks").push(body);
            Value::Bool(true)
        }
        "createForumTopic" => {
            state.topics.lock().expect("topics").push(body);
            json!({ "message_thread_id": 777 })
        }
        "editForumTopic" => {
            state.topic_edits.lock().expect("topic_edits").push(body);
            Value::Bool(true)
        }
        other => {
            return Json(json!({
                "ok": false,
                "error_code": 404,
                "description": format!("unhandled method {other}"),
            }));
        }
    };
    Json(json!({ "ok": true, "result": result }))
}

struct MockAgent {
    timeline: broadcast::Sender<TimelineLiveEvent>,
    sessions: broadcast::Sender<AgentSession>,
    notifications: broadcast::Sender<AgentNotificationIntent>,
    session: AgentSession,
    selection: SessionRuntimeSelection,
    pending: Mutex<Option<PermissionRequest>>,
    prompts: Mutex<Vec<String>>,
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
            title: "Fake session".to_string(),
            project_id: ProjectId::new(),
            workspace_id: WorkspaceId::new(),
            workspace_root: "/tmp/fake-im-workspace".to_string(),
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
        Err(vibex_core::VibexError::process(
            "fake_create_session",
            "not used by this test",
        ))
    }

    async fn send_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        self.prompts.lock().expect("prompts").push(request.text);
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

fn message_update(update_id: i64, text: &str) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "date": 1,
            "text": text,
            "chat": { "id": 42, "type": "private" },
            "from": { "id": 42, "is_bot": false, "first_name": "Tester" }
        }
    })
}

async fn wait_for<F, Fut>(mut condition: F, label: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..120 {
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
            "Fake project",
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn telegram_pairing_commands_and_approval_round_trip() {
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

    let fake = Arc::new(FakeTelegram::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    let router = Router::new()
        .route("/bot{token}/{method}", post(fake_telegram))
        .with_state(Arc::clone(&fake));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake server");
    });

    let agent_port: Arc<dyn ImAgentPort> = agent.clone();
    let service = Arc::new(
        ImChannelService::new(ImChannelServiceConfig {
            db_path,
            home_dir: home,
            agent: agent_port,
            telegram_api_base: Some(format!("http://{address}")),
            lark_api_base: None,
            locale: Some("en".to_string()),
        })
        .expect("service"),
    );
    service.start().await.expect("start");
    let channel = service
        .create_channel(ImChannelDraft {
            display_name: "Fake bot".to_string(),
            config: ImChannelConfig::Telegram(vibex_core::ImTelegramConfig {
                chat_id: "42".to_string(),
                topic_mode: false,
                require_mention_in_groups: false,
            }),
            default_session: None,
            event_filter: Vec::new(),
            locale: Some("en".to_string()),
        })
        .await
        .expect("create channel");
    service
        .set_bot_token(&channel.channel_id, "123:fake-token")
        .await
        .expect("token");
    service
        .set_channel_enabled(&channel.channel_id, true)
        .await
        .expect("enable");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let pairing = service
        .create_pairing_code(ImPairingRequest {
            channel_id: channel.channel_id.clone(),
            role: ImPrincipalRole::Operator,
            ttl_ms: Some(60_000),
        })
        .await
        .expect("pairing");
    fake.push(message_update(1, "/start"));
    fake.push(message_update(2, &format!("/pair {}", pairing.code)));
    wait_for(
        || async {
            service
                .list_principals(&channel.channel_id)
                .await
                .map(|principals| {
                    principals
                        .iter()
                        .any(|principal| principal.status == ImPrincipalStatus::Active)
                })
                .unwrap_or(false)
        },
        "pairing",
    )
    .await;

    fake.push(message_update(3, "/sessions"));
    wait_for(
        || async {
            fake.sent_texts()
                .iter()
                .any(|text| text.contains("Recent sessions"))
        },
        "session list",
    )
    .await;

    fake.push(message_update(4, "/use 1"));
    wait_for(
        || async {
            fake.sent_texts()
                .iter()
                .any(|text| text.contains("Bound to"))
        },
        "binding",
    )
    .await;

    fake.push(message_update(5, "hello fake"));
    wait_for(
        || async { !agent.prompts.lock().expect("prompts").is_empty() },
        "prompt",
    )
    .await;

    let request = PermissionRequest {
        id: vibex_core::RequestId::new(),
        session_id: agent.session.id.clone(),
        project_id: Some(agent.session.project_id.clone()),
        workspace_id: Some(agent.session.workspace_id.clone()),
        provider_request_id: Some("fake-request".to_string()),
        risk_category: vibex_core::PermissionRiskCategory::Command,
        title: "Run fake tests".to_string(),
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
        expires_at_ms: Some(unix_timestamp_ms() + 60_000),
    };
    *agent.pending.lock().expect("pending") = Some(request.clone());
    agent
        .timeline
        .send(TimelineLiveEvent {
            session_id: agent.session.id.clone(),
            sequence: 7,
            item: agent.item(7, TimelinePayload::PermissionRequest(request)),
        })
        .expect("timeline event");
    wait_for(|| async { fake.callback_data().is_some() }, "approval card").await;
    let action_id = fake.callback_data().expect("callback data");
    fake.push(json!({
        "update_id": 6,
        "callback_query": {
            "id": "callback-1",
            "from": { "id": 42, "is_bot": false, "first_name": "Tester" },
            "data": action_id,
            "message": {
                "message_id": 1,
                "chat": { "id": 42, "type": "private" }
            }
        }
    }));
    wait_for(
        || async { !agent.resolutions.lock().expect("resolutions").is_empty() },
        "approval resolution",
    )
    .await;
    assert!(
        fake.callbacks
            .lock()
            .expect("callbacks")
            .iter()
            .any(|body| body.get("callback_query_id").and_then(Value::as_str) == Some("callback-1")),
        "callback was acknowledged"
    );
    assert!(
        fake.edited.lock().expect("edited").iter().any(|body| body
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("approved"))),
        "approval card was edited"
    );

    let bindings: Vec<ImBindingSummary> = service
        .list_bindings(&channel.channel_id)
        .await
        .expect("bindings");
    assert_eq!(bindings.len(), 1);
    assert_eq!(agent.prompts.lock().expect("prompts").len(), 1);
    assert_eq!(agent.resolutions.lock().expect("resolutions").len(), 1);

    service.stop().await;
    server.abort();
}

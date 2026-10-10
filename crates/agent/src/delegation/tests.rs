use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf, duplex, split};
use tokio::sync::Notify;

const TEST_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_AUTHORITY: &str = "test-authority";

#[derive(Default)]
struct TestHost {
    revision: AtomicU64,
    active: AtomicUsize,
    activity: Notify,
    release: Notify,
    calls: Mutex<Vec<(VibexUseTool, Value)>>,
    deliveries: Mutex<Vec<(VibexUseTool, Value)>>,
}

struct ActiveCall<'a>(&'a TestHost);

impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.activity.notify_one();
    }
}

impl TestHost {
    async fn wait_for_active(&self, count: usize) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let activity = self.activity.notified();
                if self.active.load(Ordering::SeqCst) == count {
                    break;
                }
                activity.await;
            }
        })
        .await
        .expect("active requests should converge");
    }

    async fn wait_for_delivery(&self) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let activity = self.activity.notified();
                if !self.deliveries.lock().unwrap().is_empty() {
                    break;
                }
                activity.await;
            }
        })
        .await
        .expect("stdout delivery should reach the host");
    }
}

impl VibexUseToolHost for TestHost {
    fn activation_revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    fn authorize_actor(&self, actor: &VibexUseActor) -> VibexResult<()> {
        if actor.authority != TEST_AUTHORITY
            || actor.activation_revision != self.activation_revision()
        {
            return Err(VibexError::capability(
                "activation_revoked",
                "the delivery was revoked",
            ));
        }
        Ok(())
    }

    fn call(
        &self,
        actor: VibexUseActor,
        tool: VibexUseTool,
        arguments: Value,
    ) -> vibex_core::VibexUseToolFuture<'_> {
        Box::pin(async move {
            self.authorize_actor(&actor)?;
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveCall(self);
            self.calls.lock().unwrap().push((tool, arguments.clone()));
            self.activity.notify_one();
            if arguments.get("block").and_then(Value::as_bool) == Some(true) {
                self.release.notified().await;
            }
            if tool == VibexUseTool::Delegate || tool == VibexUseTool::GetTasks {
                return Err(VibexError::capability(
                    "test_policy_denied",
                    "the host refused this operation",
                ));
            }
            if matches!(tool, VibexUseTool::Wait | VibexUseTool::GetEvents) {
                let padding = arguments
                    .get("padding")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                return Ok(json!({
                    "events": [{ "eventId": "event-one", "cursor": 1, "taskRef": "vibex://task/task-one" }],
                    "nextCursor": 1,
                    "padding": "x".repeat(padding.min(MAX_BROKER_LINE_BYTES)),
                }));
            }
            Ok(json!({ "tool": tool.name() }))
        })
    }

    fn response_delivered(
        &self,
        actor: &VibexUseActor,
        tool: VibexUseTool,
        response: &Value,
    ) -> VibexResult<()> {
        self.authorize_actor(actor)?;
        self.deliveries
            .lock()
            .unwrap()
            .push((tool, response.clone()));
        self.activity.notify_one();
        Ok(())
    }

    fn tool_definitions(&self, _: &VibexUseActor) -> Vec<vibex_core::VibexUseToolDefinition> {
        [
            VibexUseTool::Delegate,
            VibexUseTool::GetTasks,
            VibexUseTool::Wait,
            VibexUseTool::GetEvents,
            VibexUseTool::AckEvents,
        ]
        .into_iter()
        .map(|tool| vibex_core::VibexUseToolDefinition {
            name: tool.name().to_string(),
            description: "Test tool".to_string(),
            input_schema: json!({ "type": "object" }),
        })
        .collect()
    }
}

struct BrokerFixture {
    _directory: tempfile::TempDir,
    manager: Arc<AgentManager>,
    host: Arc<TestHost>,
    config: Arc<stdio::SidecarConfig>,
    global_token: String,
    task: JoinHandle<()>,
}

impl BrokerFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let manager = Arc::new(AgentManager::new(directory.path().join("runtime.sqlite")).unwrap());
        let host = Arc::new(TestHost::default());
        host.revision.store(1, Ordering::SeqCst);
        let installed: Arc<dyn VibexUseToolHost> = host.clone();
        manager.install_vibex_use_host(&installed).unwrap();
        let (launch, task) = start_delegation_broker(
            manager.clone(),
            PathBuf::from("vibex"),
            TEST_AUTHORITY.to_string(),
            1,
        )
        .await
        .unwrap();
        let parent = VibexSessionId::new();
        let config = Arc::new(stdio::SidecarConfig {
            endpoint: launch.broker_endpoint.parse().unwrap(),
            token: session_activation_capability_token(
                &launch.capability_token,
                &parent,
                TEST_AUTHORITY,
                1,
            ),
            parent_session_id: parent.to_string(),
            authority: TEST_AUTHORITY.to_string(),
            activation_revision: 1,
        });
        Self {
            _directory: directory,
            manager,
            host,
            config,
            global_token: launch.capability_token,
            task,
        }
    }

    fn request(&self, method: &str, params: Value) -> Value {
        json!({
            "token": self.config.token,
            "parentSessionId": self.config.parent_session_id,
            "authority": self.config.authority,
            "activationRevision": self.config.activation_revision,
            "method": method,
            "params": params,
        })
    }

    async fn connect(&self, request: Value) -> BufReader<TcpStream> {
        let mut stream = TcpStream::connect(self.config.endpoint).await.unwrap();
        write_frame(&mut stream, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        BufReader::new(stream)
    }
}

impl Drop for BrokerFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct StdioFixture {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    task: JoinHandle<std::io::Result<()>>,
}

impl StdioFixture {
    fn new(config: Arc<stdio::SidecarConfig>) -> Self {
        let (client, server) = duplex(MAX_BROKER_LINE_BYTES * 2);
        let (reader, writer) = split(server);
        let task = tokio::spawn(stdio::serve_stdio(BufReader::new(reader), writer, config));
        let (reader, input) = split(client);
        Self {
            input,
            output: BufReader::new(reader),
            task,
        }
    }

    async fn send(&mut self, request: Value) {
        write_frame(&mut self.input, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
    }

    async fn reply(&mut self) -> Value {
        reply(&mut self.output).await
    }
}

impl Drop for StdioFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn reply(reader: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> Value {
    let bytes = tokio::time::timeout(
        TEST_TIMEOUT,
        read_bounded_line(reader, MAX_BROKER_LINE_BYTES),
    )
    .await
    .expect("a response should arrive without blocking other requests")
    .unwrap()
    .expect("a complete response");
    serde_json::from_slice(&bytes).unwrap()
}

fn tool_call(id: Value, tool: VibexUseTool, arguments: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": tool.name(), "arguments": arguments } })
}

fn ping(id: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "ping" })
}

#[test]
fn activation_tokens_bind_every_identity_dimension() {
    let parent = VibexSessionId::new();
    let token = session_activation_capability_token("secret", &parent, TEST_AUTHORITY, 1);
    assert_eq!(
        token,
        session_activation_capability_token("secret", &parent, TEST_AUTHORITY, 1)
    );
    for changed in [
        session_activation_capability_token("other", &parent, TEST_AUTHORITY, 1),
        session_activation_capability_token("secret", &VibexSessionId::new(), TEST_AUTHORITY, 1),
        session_activation_capability_token("secret", &parent, "other", 1),
        session_activation_capability_token("secret", &parent, TEST_AUTHORITY, 2),
        session_capability_token("secret", &parent),
    ] {
        assert_ne!(token, changed);
    }
}

#[tokio::test]
async fn broker_requires_bound_authority_and_revision_and_rejects_impersonation() {
    let fixture = BrokerFixture::new().await;
    for field in ["authority", "activationRevision"] {
        let mut request = fixture.request("vibex_discover", json!({}));
        request.as_object_mut().unwrap().remove(field);
        let response = reply(&mut fixture.connect(request).await).await;
        assert_eq!(
            response["error"]["code"],
            "agent_delegation_request_invalid"
        );
    }
    for (field, value) in [
        ("parentSessionId", json!(VibexSessionId::new())),
        ("authority", json!("another-authority")),
        ("activationRevision", json!(2)),
        ("token", json!(fixture.global_token)),
        (
            "token",
            json!(session_capability_token(
                &fixture.global_token,
                &VibexSessionId::parse(&fixture.config.parent_session_id).unwrap()
            )),
        ),
    ] {
        let mut request = fixture.request("vibex_discover", json!({}));
        request[field] = value;
        let response = reply(&mut fixture.connect(request).await).await;
        assert_eq!(response["error"]["code"], "agent_delegation_unauthorized");
    }
    assert!(fixture.host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn revoked_activation_terminates_a_wait_and_requires_a_new_credential() {
    let fixture = BrokerFixture::new().await;
    let mut wait = fixture
        .connect(fixture.request("vibex_wait", json!({ "block": true })))
        .await;
    fixture.host.wait_for_active(1).await;
    fixture.host.revision.store(2, Ordering::SeqCst);
    let rejected = reply(&mut wait).await;
    assert_eq!(rejected["error"]["code"], "activation_revoked");
    fixture.host.wait_for_active(0).await;
    let mut forged = fixture.request("vibex_discover", json!({}));
    forged["activationRevision"] = json!(2);
    let rejected = reply(&mut fixture.connect(forged.clone()).await).await;
    assert_eq!(rejected["error"]["code"], "agent_delegation_unauthorized");
    forged["token"] = json!(session_activation_capability_token(
        &fixture.global_token,
        &VibexSessionId::parse(&fixture.config.parent_session_id).unwrap(),
        TEST_AUTHORITY,
        2,
    ));
    assert_eq!(reply(&mut fixture.connect(forged).await).await["ok"], true);
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wait_does_not_block_discovery_and_cancellation_allows_request_id_reuse() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    stdio
        .send(tool_call(
            json!(1),
            VibexUseTool::Wait,
            json!({ "block": true }),
        ))
        .await;
    fixture.host.wait_for_active(1).await;
    stdio
        .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .await;
    let catalogue = stdio.reply().await;
    assert_eq!(catalogue["id"], 2);
    assert!(
        catalogue["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "vibex_wait")
    );
    stdio.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } })).await;
    stdio.send(ping(json!(1))).await;
    assert_eq!(
        stdio.reply().await,
        json!({ "jsonrpc": "2.0", "id": 1, "result": {} })
    );
    fixture.host.wait_for_active(0).await;
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_json_and_duplicate_request_ids_leave_the_channel_usable() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    stdio.input.write_all(b"{invalid}\n").await.unwrap();
    assert_eq!(stdio.reply().await["error"]["code"], -32700);
    stdio
        .send(json!({ "jsonrpc": "2.0", "id": 3, "method": 5 }))
        .await;
    assert_eq!(stdio.reply().await["error"]["code"], -32600);
    stdio
        .send(tool_call(
            json!(7),
            VibexUseTool::Wait,
            json!({ "block": true }),
        ))
        .await;
    fixture.host.wait_for_active(1).await;
    stdio.send(ping(json!(7))).await;
    assert_eq!(stdio.reply().await["error"]["code"], -32000);
    stdio.send(ping(json!("7"))).await;
    assert_eq!(stdio.reply().await["id"], "7");
    stdio.send(ping(json!(true))).await;
    assert_eq!(stdio.reply().await["error"]["code"], -32600);
    stdio.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 7 } })).await;
    fixture.host.wait_for_active(0).await;
}

#[tokio::test]
async fn request_budget_is_bounded_and_notifications_can_free_capacity() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    for id in 0..16 {
        stdio
            .send(tool_call(
                json!(id),
                VibexUseTool::Wait,
                json!({ "block": true }),
            ))
            .await;
    }
    fixture.host.wait_for_active(16).await;
    stdio.send(ping(json!(99))).await;
    assert_eq!(stdio.reply().await["result"], json!({}));
    stdio
        .send(tool_call(
            json!(100),
            VibexUseTool::Wait,
            json!({ "block": true }),
        ))
        .await;
    assert_eq!(stdio.reply().await["error"]["code"], -32000);
    assert_eq!(fixture.host.calls.lock().unwrap().len(), 16);
    for id in 0..16 {
        stdio.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id } })).await;
    }
    fixture.host.wait_for_active(0).await;
    stdio.send(ping(json!(101))).await;
    assert_eq!(stdio.reply().await["result"], json!({}));
}

#[tokio::test]
async fn broker_timeout_releases_the_pending_call_without_result_delivery() {
    let fixture = BrokerFixture::new().await;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mut connection = BufReader::new(
        TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap(),
    );
    let (stream, _) = listener.accept().await.unwrap();
    let server = tokio::spawn(serve_broker_connection_with_timeout(
        stream,
        fixture.manager.clone(),
        fixture.global_token.clone(),
        Duration::from_millis(30),
    ));
    write_frame(
        connection.get_mut(),
        &serde_json::to_vec(&fixture.request("vibex_wait", json!({ "block": true }))).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        reply(&mut connection).await["error"]["code"],
        "agent_delegation_broker_timeout"
    );
    server.await.unwrap().unwrap();
    fixture.host.wait_for_active(0).await;
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn intermediate_delivery_and_a_wrong_receipt_do_not_mark_events_delivered() {
    let fixture = BrokerFixture::new().await;
    let mut connection = fixture
        .connect(fixture.request("vibex_get_events", json!({})))
        .await;
    let result = reply(&mut connection).await;
    assert!(result["deliveryReceipt"].is_string());
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
    write_frame(
        connection.get_mut(),
        br#"{"deliveryReceipt":"another-receipt","eventIds":["event-one"]}"#,
    )
    .await
    .unwrap();
    assert!(
        read_bounded_line(&mut connection, 4096)
            .await
            .unwrap()
            .is_none()
    );
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
    let mut connection = fixture
        .connect(fixture.request("vibex_get_events", json!({})))
        .await;
    let result = reply(&mut connection).await;
    write_frame(
        connection.get_mut(),
        &serde_json::to_vec(
            &json!({ "deliveryReceipt": result["deliveryReceipt"], "eventIds": ["forged-event"] }),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(reply(&mut connection).await["ok"], true);
    let delivered = fixture.host.deliveries.lock().unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0],
        (VibexUseTool::GetEvents, result["value"].clone())
    );
}

#[tokio::test]
async fn oversized_stdout_results_report_an_error_without_a_delivery_receipt() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    stdio
        .send(tool_call(
            json!(1),
            VibexUseTool::Wait,
            json!({ "padding": MAX_MCP_MESSAGE_BYTES }),
        ))
        .await;
    let result = stdio.reply().await;
    assert_eq!(result["error"]["code"], -32000);
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("smaller result page")
    );
    stdio.send(ping(json!(2))).await;
    assert_eq!(stdio.reply().await["id"], 2);
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn legacy_aliases_are_capability_gated_and_share_host_policy() {
    let fixture = BrokerFixture::new().await;
    let tools = reply(
        &mut fixture
            .connect(fixture.request("list_tools", json!({})))
            .await,
    )
    .await;
    let names: Vec<_> = tools["value"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"delegate_to_agent"));
    assert!(names.contains(&"get_delegation_status"));
    assert!(!names.contains(&"cancel_delegation"));
    let delegate = fixture.connect(fixture.request("delegate_to_agent", json!({
        "task": "Inspect the workspace", "agentId": "claude", "model": "model-one", "reasoningEffort": "high", "modeId": "code", "idempotencyKey": "same-request",
    }))).await;
    assert_eq!(
        reply(&mut { delegate }).await["error"]["code"],
        "test_policy_denied"
    );
    let status = fixture
        .connect(fixture.request(
            "get_delegation_status",
            json!({ "delegationId": AgentDelegationId::new() }),
        ))
        .await;
    assert_eq!(
        reply(&mut { status }).await["error"]["code"],
        "test_policy_denied"
    );
    let calls = fixture.host.calls.lock().unwrap();
    assert_eq!(calls[0].0, VibexUseTool::Delegate);
    assert_eq!(calls[0].1["task"]["completionPolicy"], "single_turn_legacy");
    assert_eq!(calls[0].1["idempotencyKey"], "same-request");
    assert_eq!(calls[0].1["target"]["reasoningEffort"], "high");
    assert_eq!(calls[1].0, VibexUseTool::GetTasks);
}

#[test]
fn legacy_results_keep_session_identity_private_and_error_metadata_is_bounded() {
    let mut delegation = AgentDelegation::single_turn_legacy(
        VibexSessionId::new(),
        "test",
        "Child task",
        "Inspect the project",
        None,
        AgentDelegationStatus::Running,
        1,
    );
    delegation.child_session_id = Some(VibexSessionId::new());
    let result = encode_delegation_tool_result(delegation).unwrap();
    assert!(result.get("id").is_some());
    assert!(result.get("parentSessionId").is_none());
    assert!(result.get("childSessionId").is_none());
    let error = VibexError::process("test_failure", "request failed")
        .with_diagnostic("error", "private native output")
        .with_diagnostic("token", "a-secret")
        .with_diagnostic("catalogRevision", "9");
    let response = broker_error_value(&error);
    assert_eq!(
        response["error"]["diagnostics"],
        json!([{ "key": "catalogRevision", "value": "9" }])
    );
    assert!(!response.to_string().contains("a-secret"));
}

#[test]
fn invalid_legacy_selector_types_do_not_silently_inherit_another_runtime() {
    for key in [
        "agentId",
        "providerProfileId",
        "model",
        "reasoningEffort",
        "modeId",
    ] {
        let mut arguments = json!({ "task": "Inspect the workspace" });
        arguments[key] = json!(42);
        assert_eq!(
            legacy_delegate_arguments(&arguments).unwrap_err().code,
            "agent_delegation_params_invalid"
        );
    }
}

#[tokio::test]
async fn a_runtime_without_the_shared_host_fails_closed() {
    let fixture = BrokerFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = AgentManager::new(directory.path().join("unavailable.sqlite")).unwrap();
    let request: BrokerRequest =
        serde_json::from_value(fixture.request("delegate_to_agent", json!({ "task": "Inspect" })))
            .unwrap();
    let error = authorize_broker_request(&manager, &fixture.global_token, &request)
        .err()
        .unwrap();
    assert_eq!(error.code, "vibex_use_unavailable");
}

#[tokio::test]
async fn eof_drains_an_already_accepted_initialize_request() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    stdio.send(json!({ "jsonrpc": "2.0", "id": 7, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } })).await;
    stdio.input.shutdown().await.unwrap();
    let response = stdio.reply().await;
    assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        AGENT_DELEGATION_MCP_SERVER_ID
    );
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
}

#[derive(Default)]
struct OutputState {
    bytes: Mutex<Vec<u8>>,
    flush_allowed: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<std::task::Waker>>,
    changed: Notify,
    fail_flush: bool,
}

impl OutputState {
    async fn frame(&self) -> Value {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let changed = self.changed.notified();
                if self.bytes.lock().unwrap().last() == Some(&b'\n') {
                    return serde_json::from_slice(&self.bytes.lock().unwrap()).unwrap();
                }
                changed.await;
            }
        })
        .await
        .expect("the output should contain a complete frame")
    }

    fn release_flush(&self) {
        self.flush_allowed.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

struct GatedWriter(Arc<OutputState>);

impl tokio::io::AsyncWrite for GatedWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.bytes.lock().unwrap().extend_from_slice(bytes);
        self.0.changed.notify_one();
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.0.fail_flush {
            return std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        if self.0.flush_allowed.load(Ordering::SeqCst) {
            std::task::Poll::Ready(Ok(()))
        } else {
            *self.0.waker.lock().unwrap() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn host_records_the_exact_response_only_after_stdout_flush() {
    let fixture = BrokerFixture::new().await;
    let output = Arc::new(OutputState::default());
    let (mut input, reader) = duplex(4096);
    let server = tokio::spawn(stdio::serve_stdio(
        BufReader::new(reader),
        GatedWriter(output.clone()),
        fixture.config.clone(),
    ));
    write_frame(
        &mut input,
        &serde_json::to_vec(&tool_call(json!(1), VibexUseTool::GetEvents, json!({}))).unwrap(),
    )
    .await
    .unwrap();
    let frame = output.frame().await;
    assert_eq!(frame["id"], 1);
    assert!(
        fixture.host.deliveries.lock().unwrap().is_empty(),
        "writing bytes without flush must not record delivery"
    );
    output.release_flush();
    fixture.host.wait_for_delivery().await;
    assert_eq!(
        fixture.host.deliveries.lock().unwrap().as_slice(),
        &[(
            VibexUseTool::GetEvents,
            frame["result"]["structuredContent"].clone()
        )]
    );
    input.shutdown().await.unwrap();
    tokio::time::timeout(TEST_TIMEOUT, server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn failed_stdout_flush_leaves_events_eligible_for_redelivery() {
    let fixture = BrokerFixture::new().await;
    let output = Arc::new(OutputState {
        fail_flush: true,
        ..Default::default()
    });
    let (mut input, reader) = duplex(4096);
    let server = tokio::spawn(stdio::serve_stdio(
        BufReader::new(reader),
        GatedWriter(output),
        fixture.config.clone(),
    ));
    write_frame(
        &mut input,
        &serde_json::to_vec(&tool_call(json!(1), VibexUseTool::GetEvents, json!({}))).unwrap(),
    )
    .await
    .unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, server)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delivery_handoff_blocks_only_explicit_ack_and_keeps_ping_responsive() {
    let fixture = BrokerFixture::new().await;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mut config = (*fixture.config).clone();
    config.endpoint = listener.local_addr().unwrap();
    let received_receipt = Arc::new(Notify::new());
    let release_receipt = Arc::new(Notify::new());
    let ack_calls = Arc::new(AtomicUsize::new(0));
    let received = received_receipt.clone();
    let release = release_receipt.clone();
    let calls = ack_calls.clone();
    let broker = tokio::spawn(async move {
        let mut requests = JoinSet::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let received = received.clone();
            let release = release.clone();
            let calls = calls.clone();
            requests.spawn(async move {
                let mut connection = BufReader::new(stream);
                let request = reply(&mut connection).await;
                if request["method"] == VibexUseTool::GetEvents.name() {
                    write_frame(connection.get_mut(), &serde_json::to_vec(&json!({
                        "ok": true, "value": { "events": [{ "eventId": "event-one" }] }, "deliveryReceipt": "receipt-one",
                    })).unwrap()).await.unwrap();
                    let receipt = reply(&mut connection).await;
                    assert_eq!(receipt["deliveryReceipt"], "receipt-one");
                    received.notify_one();
                    release.notified().await;
                    write_frame(connection.get_mut(), br#"{"ok":true}"#).await.unwrap();
                } else {
                    assert_eq!(request["method"], VibexUseTool::AckEvents.name());
                    calls.fetch_add(1, Ordering::SeqCst);
                    write_frame(connection.get_mut(), br#"{"ok":true,"value":{"acked":1}}"#).await.unwrap();
                }
            });
        }
        while let Some(result) = requests.join_next().await {
            result.unwrap();
        }
    });
    let mut stdio = StdioFixture::new(Arc::new(config));
    stdio
        .send(tool_call(json!(1), VibexUseTool::GetEvents, json!({})))
        .await;
    assert_eq!(stdio.reply().await["id"], 1);
    tokio::time::timeout(TEST_TIMEOUT, received_receipt.notified())
        .await
        .unwrap();
    stdio
        .send(tool_call(
            json!(2),
            VibexUseTool::AckEvents,
            json!({ "eventIds": ["event-one"] }),
        ))
        .await;
    stdio.send(ping(json!(3))).await;
    assert_eq!(stdio.reply().await["id"], 3);
    assert_eq!(
        ack_calls.load(Ordering::SeqCst),
        0,
        "explicit ACK must wait until the delivery receipt is stored"
    );
    release_receipt.notify_one();
    assert_eq!(stdio.reply().await["id"], 2);
    assert_eq!(ack_calls.load(Ordering::SeqCst), 1);
    tokio::time::timeout(TEST_TIMEOUT, broker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn completing_a_call_preserves_the_next_partially_read_frame() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    stdio
        .send(tool_call(
            json!(1),
            VibexUseTool::Wait,
            json!({ "block": true }),
        ))
        .await;
    fixture.host.wait_for_active(1).await;
    let message = serde_json::to_vec(&ping(json!(2))).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", message.len());
    stdio.input.write_all(header.as_bytes()).await.unwrap();
    let split_at = message.len() / 2;
    stdio.input.write_all(&message[..split_at]).await.unwrap();
    fixture.host.release.notify_one();
    assert_eq!(stdio.reply().await["id"], 1);
    stdio.input.write_all(&message[split_at..]).await.unwrap();
    assert_eq!(stdio.reply().await["id"], 2);
}

#[tokio::test]
async fn oversized_stdio_input_closes_its_waiters_without_recording_delivery() {
    let fixture = BrokerFixture::new().await;
    let mut stdio = StdioFixture::new(fixture.config.clone());
    stdio
        .send(tool_call(
            json!(1),
            VibexUseTool::Wait,
            json!({ "block": true }),
        ))
        .await;
    fixture.host.wait_for_active(1).await;
    stdio
        .input
        .write_all(&vec![b'x'; MAX_MCP_MESSAGE_BYTES + 1])
        .await
        .unwrap();
    let result = tokio::time::timeout(TEST_TIMEOUT, &mut stdio.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    fixture.host.wait_for_active(0).await;
    assert!(fixture.host.deliveries.lock().unwrap().is_empty());
}

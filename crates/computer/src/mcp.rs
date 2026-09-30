//! The Model Context Protocol surface for computer-use tools.
//!
//! One handler serves both delivery paths — the loopback HTTP endpoint and the
//! stdio sidecar — so behaviour cannot drift between them.
//!
//! The approval flow is the reason this module exists rather than letting an
//! Agent call the engine directly. When a tool call hits a class that needs
//! consent, the handler raises a card through the runtime, and the retry
//! carries a **one-off** approval that is spent by that retry. Nothing about
//! "the user once approved a click on Save" is remembered unless the class is
//! one the risk model allows to be remembered.
//!
//! Descriptions carry the rules; see [`crate::tools`].

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use vibex_core::{
    ComputerApprovalGranularity, ComputerSessionId, ComputerToolTier, VibexSessionId, WorkspaceId,
};

use crate::error::ComputerError;
use crate::service::{
    ApprovedAction, ComputerApprovalRequest, ComputerService, ComputerToolContext,
    ComputerToolOutcome,
};
use crate::{error::codes, tools};

/// The stable id of the built-in computer MCP server.
pub use vibex_core::COMPUTER_MCP_SERVER_ID;

/// Default MCP protocol version echoed when a client does not name one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Everything the runtime knows about the agent session behind a token.
#[derive(Debug, Clone)]
pub struct ComputerMcpSession {
    pub session_id: ComputerSessionId,
    pub agent_session_id: Option<VibexSessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub tier: ComputerToolTier,
    /// Human-readable agent name, used in approval copy.
    pub agent_label: String,
}

impl ComputerMcpSession {
    pub fn tool_context(&self) -> ComputerToolContext {
        ComputerToolContext {
            session_id: self.session_id.clone(),
            agent_session_id: self.agent_session_id.clone(),
            workspace_id: self.workspace_id.clone(),
            tier: self.tier,
            agent_label: self.agent_label.clone(),
            approved: Vec::new(),
        }
    }
}

/// How a human answered a computer-use approval prompt.
///
/// The variants are deliberately the same shape as the browser's: a `Once`
/// approval travels with the retry, a remembered one is written to the session
/// grant store, and a denial does nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerPermissionDecision {
    /// Approve this call only.
    Approve,
    /// Approve and remember, for the classes the risk model permits.
    AlwaysAllow,
    /// Deny; the action does not run.
    Deny,
}

/// The runtime side of the MCP endpoint: token resolution and human consent.
#[async_trait]
pub trait ComputerMcpHost: Send + Sync + 'static {
    /// Resolves a bearer token to a session context.
    async fn resolve_token(&self, token: &str) -> Option<ComputerMcpSession>;

    /// Raises an approval card and waits for the human.
    ///
    /// Implementations must apply their own timeout: pending permissions have
    /// no global expiry in this runtime, so an unanswered card would hold the
    /// Agent's tool call for its whole budget.
    async fn request_permission(
        &self,
        session: &ComputerMcpSession,
        request: &ComputerApprovalRequest,
    ) -> ComputerPermissionDecision;

    /// Reports an event worth surfacing in the UI.
    async fn report(&self, _session: &ComputerMcpSession, _event: &str, _payload: Value) {}
}

/// The transport-neutral MCP handler.
pub struct ComputerMcpHandler {
    service: ComputerService,
    host: Option<Arc<dyn ComputerMcpHost>>,
}

impl std::fmt::Debug for ComputerMcpHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComputerMcpHandler")
            .field("has_host", &self.host.is_some())
            .finish()
    }
}

impl ComputerMcpHandler {
    pub fn new(service: ComputerService) -> Self {
        Self {
            service,
            host: None,
        }
    }

    pub fn with_host(mut self, host: Arc<dyn ComputerMcpHost>) -> Self {
        self.host = Some(host);
        self
    }

    pub fn service(&self) -> &ComputerService {
        &self.service
    }

    /// Handles one JSON-RPC message. Returns `None` for notifications.
    pub async fn handle_message(
        &self,
        session: &ComputerMcpSession,
        message: Value,
    ) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if id.is_none() {
            if method == "notifications/cancelled" {
                // The Agent's stop must reach the desktop: an action already
                // running is not revocable, but nothing further is dispatched.
                self.service
                    .pause(&session.session_id, "the Agent cancelled its own request")
                    .await
                    .ok();
            }
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        let response = match method {
            "initialize" => self.initialize(&params),
            "ping" => json!({}),
            "tools/list" => tools::tools_list_payload(session.tier),
            "tools/call" => self.call_tool(session, &params).await,
            other => {
                return Some(error_response(
                    id,
                    -32601,
                    &format!("`{other}` is not supported by the computer MCP server"),
                ));
            }
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": response }))
    }

    fn initialize(&self, params: &Value) -> Value {
        let protocol_version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_PROTOCOL_VERSION)
            .to_string();
        json!({
            "protocolVersion": protocol_version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": COMPUTER_MCP_SERVER_ID, "version": env!("CARGO_PKG_VERSION") },
            "instructions": tools::initialize_instructions(),
        })
    }

    async fn call_tool(&self, session: &ComputerMcpSession, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if name.is_empty() {
            return json!({
                "content": [{ "type": "text", "text": "`name` is required" }],
                "isError": true,
            });
        }
        let context = session.tool_context();
        let outcome = match self
            .service
            .call_tool_checked(&context, name, &arguments)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let outcome = ComputerToolOutcome::error(&error);
                return outcome_to_mcp(&outcome, &error);
            }
        };
        // An approval-required result carries the card instead of an error.
        let Some(request) = outcome.approval.clone() else {
            return outcome_to_mcp(&outcome, &ComputerError::validation("ok", "ok"));
        };
        let Some(host) = self.host.as_ref() else {
            let error = ComputerError::permission(
                codes::HARD_DENIED,
                "the action needs the user's approval, but no approval channel is connected",
            );
            return outcome_to_mcp(&ComputerToolOutcome::error(&error), &error);
        };
        let decision = host.request_permission(session, &request).await;
        match decision {
            ComputerPermissionDecision::Deny => {
                let error = ComputerError::permission(
                    codes::DENIED,
                    "The user denied this action. Do not retry it; ask what they would prefer.",
                );
                outcome_to_mcp(&ComputerToolOutcome::error(&error), &error)
            }
            ComputerPermissionDecision::Approve | ComputerPermissionDecision::AlwaysAllow => {
                let mut approved = context.clone();
                approved.approved.push(ApprovedAction {
                    risk: request.risk,
                    target: request.target.canonical_identity(),
                    foreground: matches!(
                        request.risk,
                        vibex_core::ComputerRiskClass::ForegroundEscalation
                    ) || arguments
                        .get("delivery_mode")
                        .and_then(Value::as_str)
                        .is_some_and(|mode| mode.eq_ignore_ascii_case("foreground")),
                });
                let retried = self
                    .service
                    .call_tool_checked(&approved, name, &arguments)
                    .await;
                match retried {
                    Ok(outcome) => {
                        // "Always allow" is only durable for the classes the
                        // risk model says may be remembered; for the rest the
                        // approval died with the retry above.
                        if decision == ComputerPermissionDecision::AlwaysAllow
                            && request.granularity == ComputerApprovalGranularity::Session
                        {
                            self.service
                                .remember_grant(&session.session_id, request.risk, &request.target)
                                .await;
                        }
                        outcome_to_mcp(&outcome, &ComputerError::validation("ok", "ok"))
                    }
                    Err(error) => {
                        let outcome = ComputerToolOutcome::error(&error);
                        outcome_to_mcp(&outcome, &error)
                    }
                }
            }
        }
    }
}

/// Renders an outcome as MCP content.
///
/// The text always leads with the outcome's own text; the error code is
/// appended when there is one, because a model needs to distinguish "the user
/// denied it" from "the element moved" to decide what to do next.
fn outcome_to_mcp(outcome: &ComputerToolOutcome, error: &ComputerError) -> Value {
    let mut text = outcome.text.clone();
    if outcome.is_error || !error.code.is_empty() {
        if let Some(hint) = &error.recovery_hint
            && outcome.is_error
        {
            text.push_str(&format!("\nHint: {hint}"));
        }
        if outcome.is_error {
            text.push_str(&format!("\nCode: {}", error.code));
        }
    }
    let mut content = vec![json!({ "type": "text", "text": text })];
    for image in &outcome.images {
        content.push(json!({
            "type": "image",
            "data": image.base64,
            "mimeType": image.mime_type,
        }));
    }
    json!({ "content": content, "isError": outcome.is_error })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// Prefix of a computer-use MCP bearer token.
pub const COMPUTER_TOKEN_PREFIX: &str = vibex_core::COMPUTER_MCP_TOKEN_PREFIX;

/// Issues a session-scoped bearer token.
pub fn issue_session_token(global_secret: &str, session_id: &str) -> String {
    vibex_core::computer_mcp_session_token(global_secret, session_id)
}

/// Verifies a bearer token and returns the session id it authenticates.
pub fn verify_session_token(global_secret: &str, token: &str) -> Option<String> {
    vibex_core::verify_computer_mcp_session_token(global_secret, token)
}

/// Reads one MCP stdio message, accepting both framings.
pub fn read_stdio_message<R: std::io::BufRead>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
    let mut first = String::new();
    loop {
        first.clear();
        let read = reader.read_line(&mut first)?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = first.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            let length: usize = value.trim().parse().unwrap_or(0);
            if length == 0 || length > MAX_MESSAGE_BYTES {
                return Ok(None);
            }
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header)? == 0 {
                    return Ok(None);
                }
                if header.trim().is_empty() {
                    break;
                }
            }
            let mut buffer = vec![0u8; length];
            std::io::Read::read_exact(reader, &mut buffer)?;
            return Ok(Some(buffer));
        }
        return Ok(Some(trimmed.as_bytes().to_vec()));
    }
}

/// Writes one MCP stdio message as newline-delimited JSON.
pub fn write_stdio_message<W: std::io::Write>(
    writer: &mut W,
    value: &Value,
) -> std::io::Result<()> {
    let encoded = serde_json::to_vec(value)?;
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FixtureEngine;
    use crate::service::{ComputerService, ComputerServiceConfig};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ApprovalHost {
        decision: ComputerPermissionDecision,
        asked: Mutex<Vec<String>>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ComputerMcpHost for ApprovalHost {
        async fn resolve_token(&self, _token: &str) -> Option<ComputerMcpSession> {
            None
        }

        async fn request_permission(
            &self,
            _session: &ComputerMcpSession,
            request: &ComputerApprovalRequest,
        ) -> ComputerPermissionDecision {
            self.asked
                .lock()
                .unwrap()
                .push(request.risk.as_str().to_string());
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.decision
        }
    }

    async fn handler_with_host(decision: ComputerPermissionDecision) -> Arc<ComputerMcpHandler> {
        let service = ComputerService::new(ComputerServiceConfig::new("/tmp/vibex-computer-mcp"));
        service.install_engine(Arc::new(FixtureEngine::default()));
        let host = Arc::new(ApprovalHost {
            decision,
            asked: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        });
        Arc::new(ComputerMcpHandler::new(service).with_host(host))
    }

    fn session() -> ComputerMcpSession {
        ComputerMcpSession {
            session_id: vibex_core::ComputerSessionId::new(),
            agent_session_id: None,
            workspace_id: None,
            tier: ComputerToolTier::Visual,
            agent_label: "Claude".to_string(),
        }
    }

    async fn handler() -> Arc<ComputerMcpHandler> {
        let service = ComputerService::new(ComputerServiceConfig::new("/tmp/vibex-computer-mcp"));
        service.install_engine(Arc::new(FixtureEngine::default()));
        Arc::new(ComputerMcpHandler::new(service))
    }

    #[tokio::test]
    async fn initialize_names_the_builtin_server() {
        let handler = handler().await;
        let response = handler
            .handle_message(
                &session(),
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            )
            .await
            .unwrap();
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            COMPUTER_MCP_SERVER_ID
        );
        assert!(
            response["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("unverified")
        );
    }

    #[tokio::test]
    async fn tools_list_reflects_the_tier() {
        let handler = handler().await;
        let mut structured = session();
        structured.tier = ComputerToolTier::Structured;
        let response = handler
            .handle_message(
                &structured,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            )
            .await
            .unwrap();
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn a_destructive_click_raises_a_card_and_the_retry_runs_once() {
        let handler = handler_with_host(ComputerPermissionDecision::Approve).await;
        let session = session();
        // Observe first: the action needs a fresh reference.
        let observed = handler
            .handle_message(
                &session,
                json!({
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": { "name": "computer_get_app_state", "arguments": { "app": "com.example.mail" } }
                }),
            )
            .await
            .unwrap();
        let text = observed["result"]["content"][0]["text"].as_str().unwrap();
        let reference = text
            .lines()
            .find(|line| line.contains("Send"))
            .and_then(|line| line.split('[').nth(1))
            .and_then(|rest| rest.split(']').next())
            .expect("the fixture has a Send button")
            .to_string();
        let clicked = handler
            .handle_message(
                &session,
                json!({
                    "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": {
                        "name": "computer_click",
                        "arguments": { "app": "com.example.mail", "element": reference }
                    }
                }),
            )
            .await
            .unwrap();
        assert_eq!(clicked["result"]["isError"], false);
        assert!(
            clicked["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Verification:")
        );
    }

    #[tokio::test]
    async fn a_denied_action_never_reaches_the_desktop() {
        let service = ComputerService::new(ComputerServiceConfig::new("/tmp/vibex-computer-deny"));
        let engine = Arc::new(FixtureEngine::default());
        service.install_engine(engine.clone());
        let host = Arc::new(ApprovalHost {
            decision: ComputerPermissionDecision::Deny,
            asked: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        });
        let handler = Arc::new(ComputerMcpHandler::new(service).with_host(host));
        let session = session();
        // Observe, so the destructive control has a fresh reference.
        let observed = handler
            .handle_message(
                &session,
                json!({
                    "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                    "params": { "name": "computer_get_app_state", "arguments": { "app": "com.example.mail" } }
                }),
            )
            .await
            .unwrap();
        let text = observed["result"]["content"][0]["text"].as_str().unwrap();
        let reference = text
            .lines()
            .find(|line| line.contains("Send"))
            .and_then(|line| line.split('[').nth(1))
            .and_then(|rest| rest.split(']').next())
            .expect("the fixture has a Send button")
            .to_string();
        let before = engine.action_count.load(Ordering::SeqCst);

        let denied = handler
            .handle_message(
                &session,
                json!({
                    "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                    "params": {
                        "name": "computer_click",
                        "arguments": { "app": "com.example.mail", "element": reference }
                    }
                }),
            )
            .await
            .unwrap();
        let text = denied["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("denied"), "{text}");
        assert_eq!(
            engine.action_count.load(Ordering::SeqCst),
            before,
            "a denied action must not reach the engine"
        );
    }

    #[tokio::test]
    async fn a_credential_target_never_raises_a_card() {
        let handler = handler_with_host(ComputerPermissionDecision::Approve).await;
        let session = session();
        let denied = handler
            .handle_message(
                &session,
                json!({
                    "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                    "params": {
                        "name": "computer_type_text",
                        "arguments": { "app": "com.example.notes", "element": "c1-2", "text": "hunter2" }
                    }
                }),
            )
            .await
            .unwrap();
        let text = denied["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("secure") || text.contains("credential") || text.contains("stale"),
            "{text}"
        );
    }

    #[test]
    fn tokens_are_session_scoped_and_round_trip() {
        let token = issue_session_token("cap", "session_a_b");
        assert!(token.starts_with("ctok_"));
        assert_eq!(
            verify_session_token("cap", &token).as_deref(),
            Some("session_a_b")
        );
        assert_eq!(verify_session_token("other", &token), None);
    }

    #[test]
    fn stdio_framing_supports_both_shapes() {
        let mut input = std::io::Cursor::new(b"{\"id\":1}\n".to_vec());
        assert!(read_stdio_message(&mut input).unwrap().is_some());
        let payload = b"{\"id\":2}";
        let mut input = std::io::Cursor::new(
            format!("Content-Length: {}\r\n\r\n", payload.len())
                .into_bytes()
                .into_iter()
                .chain(payload.iter().copied())
                .collect::<Vec<u8>>(),
        );
        let message = read_stdio_message(&mut input).unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&message).unwrap()["id"], 2);
        let mut output = Vec::new();
        write_stdio_message(&mut output, &json!({ "id": 3 })).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "{\"id\":3}\n");
    }
}

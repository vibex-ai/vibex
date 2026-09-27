//! The Model Context Protocol surface for browser tools.
//!
//! The runtime exposes browser tools directly over a loopback HTTP endpoint
//! (`POST /mcp`, Streamable HTTP) and, for agents that cannot use HTTP MCP,
//! through a stdio sidecar that forwards the very same JSON-RPC to that
//! endpoint. One handler serves both, so behaviour cannot drift between the two
//! delivery paths.
//!
//! Two properties matter for correctness:
//!
//! * **Per-session capability tokens.** A browser session is created for one
//!   agent session, and the bearer token only resolves to that session, so one
//!   agent can never act on another's tabs.
//! * **Descriptions carry the rules.** Tool usage, the ref lifecycle and the
//!   untrusted-content notice live in the tool descriptions themselves, because
//!   they travel with the tool. `ContextBridge` is a runtime-switch handover
//!   and is deliberately not used for this.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use vibex_core::{BrowserSessionId, BrowserToolTier, VibexSessionId, WorkspaceId};

use crate::error::BrowserError;
use crate::service::{BrowserService, BrowserToolContext, BrowserToolOutcome};
use crate::tools;

/// Stable id of the built-in browser MCP server.
///
/// Defined in `vibex-core` so the ACP layer can recognize the runtime's own
/// servers without depending on this crate.
pub use vibex_core::BROWSER_MCP_SERVER_ID;

/// Default MCP protocol version echoed when a client does not name one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Everything the runtime knows about the agent session behind a token.
#[derive(Debug, Clone)]
pub struct BrowserMcpSession {
    pub session_id: BrowserSessionId,
    pub agent_session_id: Option<VibexSessionId>,
    pub workspace_id: Option<WorkspaceId>,
    /// Roots the agent may read from. Uploads and local previews are confined
    /// to these.
    pub authorized_roots: Vec<PathBuf>,
    pub tier: BrowserToolTier,
    /// Human-readable agent name, used in approval copy.
    pub agent_label: String,
}

impl BrowserMcpSession {
    fn tool_context(&self) -> BrowserToolContext {
        BrowserToolContext {
            session_id: self.session_id.clone(),
            agent_session_id: self.agent_session_id.clone(),
            workspace_id: self.workspace_id.clone(),
            authorized_roots: self.authorized_roots.clone(),
            tier: self.tier,
            approved_origins: Vec::new(),
        }
    }
}

/// How a human answered a browser approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPermissionDecision {
    /// Approve this call only. The approval is attached to the retry and is not
    /// remembered.
    Approve,
    /// Approve and remember the origin for the rest of the session.
    AlwaysAllow,
    Deny,
}

/// The runtime side of the MCP endpoint: token resolution and approval.
///
/// The browser crate owns the protocol; the runtime owns identity and human
/// consent, so those are supplied through this trait rather than guessed here.
#[async_trait]
pub trait BrowserMcpHost: Send + Sync + 'static {
    /// Resolves a bearer token to a session context.
    async fn resolve_token(&self, token: &str) -> Option<BrowserMcpSession>;

    /// Raises an approval card and waits for the human.
    ///
    /// `error` carries `origin` and `domain` diagnostics describing what the
    /// Agent is trying to reach. Implementations must apply their own timeout:
    /// the runtime has no global pending-permission expiry, so an unanswered
    /// card would otherwise hang for the whole ACP prompt budget.
    async fn request_permission(
        &self,
        session: &BrowserMcpSession,
        error: &BrowserError,
    ) -> BrowserPermissionDecision;

    /// Makes an "always allow" decision survive a runtime restart.
    ///
    /// The browser layer grants the origin in memory itself; this is the hook
    /// that puts it somewhere durable. The default is a no-op so a host without
    /// storage still works.
    async fn remember_origin(&self, _session: &BrowserMcpSession, _origin: &str) {}

    /// Reports a browser event worth surfacing in the UI.
    async fn report(&self, _session: &BrowserMcpSession, _event: &str, _payload: Value) {}
}

/// The transport-neutral MCP handler.
pub struct BrowserMcpHandler {
    service: BrowserService,
    host: Option<Arc<dyn BrowserMcpHost>>,
}

impl std::fmt::Debug for BrowserMcpHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserMcpHandler")
            .field("has_host", &self.host.is_some())
            .finish()
    }
}

impl BrowserMcpHandler {
    pub fn new(service: BrowserService) -> Self {
        Self {
            service,
            host: None,
        }
    }

    pub fn with_host(mut self, host: Arc<dyn BrowserMcpHost>) -> Self {
        self.host = Some(host);
        self
    }

    pub fn service(&self) -> &BrowserService {
        &self.service
    }

    /// Handles one JSON-RPC message.
    ///
    /// Returns `None` for notifications, which have no response.
    pub async fn handle_message(
        &self,
        session: &BrowserMcpSession,
        message: Value,
    ) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        // A notification never gets a reply.
        if id.is_none() {
            if method == "notifications/cancelled" {
                self.cancel(session, &params).await;
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
                    &format!("`{other}` is not supported by the browser MCP server"),
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
            "serverInfo": { "name": BROWSER_MCP_SERVER_ID, "version": env!("CARGO_PKG_VERSION") },
            // Whether a client forwards `instructions` into the model context
            // varies, so this supplements the tool descriptions rather than
            // replacing them.
            "instructions": tools::initialize_instructions(),
        })
    }

    /// Propagates `notifications/cancelled` into the browser.
    ///
    /// The sidecar would otherwise drop the agent's stop request on the floor
    /// and the human's "stop" button would never reach the runtime.
    async fn cancel(&self, session: &BrowserMcpSession, params: &Value) {
        let _ = params;
        if let Ok(snapshot) = self.service.session_snapshot(&session.session_id).await
            && let Some(tab_id) = snapshot.session.agent_tab_id
        {
            self.service.abort_agent_operations(&tab_id).await;
        }
    }

    async fn call_tool(&self, session: &BrowserMcpSession, params: &Value) -> Value {
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
        let ctx = session.tool_context();
        let outcome = match self.service.call_tool_checked(&ctx, name, &arguments).await {
            Ok(outcome) => outcome,
            Err(error) => {
                // A navigation or file access into somewhere new is a human
                // decision. The runtime owns the approval channel, so the
                // handler asks it and retries once with the approval attached
                // to that retry only.
                match self.approval_for(session, &error).await {
                    Some(BrowserPermissionDecision::Approve) => {
                        let approved = ctx.with_approved_origin(approval_key(&error));
                        match self
                            .service
                            .call_tool_checked(&approved, name, &arguments)
                            .await
                        {
                            Ok(outcome) => outcome,
                            Err(error) => BrowserToolOutcome::error(&error),
                        }
                    }
                    Some(BrowserPermissionDecision::AlwaysAllow) => {
                        let origin = approval_key(&error);
                        if let Some(host) = self.host.as_ref() {
                            host.remember_origin(session, &origin).await;
                        }
                        self.service.grant_origin(&origin).await;
                        match self.service.call_tool_checked(&ctx, name, &arguments).await {
                            Ok(outcome) => outcome,
                            Err(error) => BrowserToolOutcome::error(&error),
                        }
                    }
                    Some(BrowserPermissionDecision::Deny) => {
                        BrowserToolOutcome::error(&BrowserError::permission(
                            "browser_navigation_denied",
                            "The user denied access to that origin.",
                        ))
                    }
                    None => BrowserToolOutcome::error(&error),
                }
            }
        };
        outcome_to_mcp(&outcome)
    }

    /// Asks the host whether an approval-required failure may proceed.
    ///
    /// The decision is made on the typed error alone: an earlier version keyed
    /// it off the presence of a `tab_id` argument and a substring of the
    /// message, which silently skipped the card for the common
    /// `browser_navigate` call that omits `tab_id`.
    async fn approval_for(
        &self,
        session: &BrowserMcpSession,
        error: &BrowserError,
    ) -> Option<BrowserPermissionDecision> {
        let host = self.host.as_ref()?;
        if !crate::policy::is_navigation_approval_code(&error.code) {
            return None;
        }
        Some(host.request_permission(session, error).await)
    }
}

/// The origin an approval decision applies to.
///
/// Falls back to the empty string only for an error that reached an approval
/// path without diagnostics, which cannot happen for the codes that get here.
fn approval_key(error: &BrowserError) -> String {
    error
        .diagnostics
        .iter()
        .find(|(key, _)| key == "origin")
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

/// Renders a tool outcome as MCP content.
fn outcome_to_mcp(outcome: &BrowserToolOutcome) -> Value {
    let mut content = vec![json!({ "type": "text", "text": outcome.text })];
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

/// Prefix of a browser MCP bearer token.
pub const BROWSER_TOKEN_PREFIX: &str = vibex_core::BROWSER_MCP_TOKEN_PREFIX;

/// Issues a session-scoped bearer token.
///
/// The token is `btok_<session id>_<mac>` with
/// `mac = SHA256(global secret ‖ 0x00 ‖ session id)`.
///
/// The derivation belongs to the shared contract crate, because the runtime
/// mints this token when it describes the browser MCP server to an Agent and
/// this crate verifies it when the request arrives. Both call
/// [`vibex_core::browser_mcp_session_token`]; deriving it twice is what once
/// left every request answerable only with a 401.
pub fn issue_session_token(global_secret: &str, session_id: &str) -> String {
    vibex_core::browser_mcp_session_token(global_secret, session_id)
}

/// Verifies a bearer token and returns the session id it authenticates.
///
/// The comparison is constant time over the MAC.
pub fn verify_session_token(global_secret: &str, token: &str) -> Option<String> {
    vibex_core::verify_browser_mcp_session_token(global_secret, token)
}

/// Builds the JSON-RPC payload for one stdio message.
pub fn parse_json_rpc_message(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

/// Reads one MCP stdio message, accepting both `Content-Length` framing and
/// newline-delimited JSON.
///
/// Different agent CLIs use different framings, and getting this wrong is
/// invisible until an agent silently never calls a tool.
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
            // Consume the remaining headers up to the blank line.
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

    fn session() -> BrowserMcpSession {
        BrowserMcpSession {
            session_id: BrowserSessionId::new(),
            agent_session_id: None,
            workspace_id: None,
            authorized_roots: vec![PathBuf::from("/work")],
            tier: BrowserToolTier::Fine,
            agent_label: "Claude".to_string(),
        }
    }

    fn handler() -> BrowserMcpHandler {
        BrowserMcpHandler::new(BrowserService::new(
            crate::service::BrowserServiceConfig::new("/tmp/vibex-browser-mcp-test"),
        ))
    }

    #[tokio::test]
    async fn initialize_echoes_the_protocol_version_and_offers_instructions() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": { "protocolVersion": "2025-03-26" },
                }),
            )
            .await
            .unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            BROWSER_MCP_SERVER_ID
        );
        assert!(
            response["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("browser_observe")
        );
        assert_eq!(
            response["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[tokio::test]
    async fn initialize_defaults_the_protocol_version() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
            )
            .await
            .unwrap();
        assert_eq!(
            response["result"]["protocolVersion"],
            DEFAULT_PROTOCOL_VERSION
        );
    }

    #[tokio::test]
    async fn tools_list_respects_the_session_tier() {
        let handler = handler();
        let mut coarse = session();
        coarse.tier = BrowserToolTier::Coarse;
        let response = handler
            .handle_message(
                &coarse,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            )
            .await
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);

        let mut visual = session();
        visual.tier = BrowserToolTier::Visual;
        let response = handler
            .handle_message(
                &visual,
                json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
            )
            .await
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert!(tools.len() > 2);
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "browser_screenshot")
        );
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            )
            .await;
        assert!(response.is_none());
    }

    #[tokio::test]
    async fn cancelled_notifications_are_accepted() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": 4, "reason": "user stopped" },
                }),
            )
            .await;
        assert!(response.is_none());
    }

    #[tokio::test]
    async fn unknown_methods_return_method_not_found() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/list" }),
            )
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn a_tool_call_without_a_name_is_an_error_result() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({
                    "jsonrpc": "2.0",
                    "id": 6,
                    "method": "tools/call",
                    "params": { "arguments": {} },
                }),
            )
            .await
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
    }

    #[tokio::test]
    async fn ping_is_supported() {
        let handler = handler();
        let response = handler
            .handle_message(
                &session(),
                json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" }),
            )
            .await
            .unwrap();
        assert!(response["result"].is_object());
    }

    #[test]
    fn tokens_are_session_scoped_and_round_trip() {
        let global = "cap_global";
        let session_id = "session_00000000000000000000000000000001";
        let token = issue_session_token(global, session_id);
        assert_ne!(token, global);
        assert!(token.starts_with("btok_session_"));
        assert_eq!(
            verify_session_token(global, &token).as_deref(),
            Some(session_id)
        );
    }

    #[test]
    fn a_token_for_one_session_does_not_verify_for_another() {
        let global = "cap_global";
        let first = issue_session_token(global, "session_aaaa");
        let second = issue_session_token(global, "session_bbbb");
        assert_ne!(first, second);
        // Swapping the session id inside a valid token must fail the MAC.
        let forged = format!("btok_session_bbbb_{}", first.rsplit_once('_').unwrap().1);
        assert!(verify_session_token(global, &forged).is_none());
    }

    #[test]
    fn a_different_global_secret_is_rejected() {
        let token = issue_session_token("cap_one", "session_aaaa");
        assert!(verify_session_token("cap_two", &token).is_none());
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(verify_session_token("cap", "").is_none());
        assert!(verify_session_token("cap", "cap_global").is_none());
        assert!(verify_session_token("cap", "btok_").is_none());
        assert!(verify_session_token("cap", "btok__").is_none());
        assert!(verify_session_token("cap", "btok_session_a").is_none());
    }

    #[test]
    fn stdio_reader_accepts_newline_delimited_json() {
        let mut input = std::io::Cursor::new(b"{\"id\":1}\n".to_vec());
        let message = read_stdio_message(&mut input).unwrap().unwrap();
        assert_eq!(parse_json_rpc_message(&message).unwrap()["id"], 1);
    }

    #[test]
    fn stdio_reader_accepts_content_length_framing() {
        let payload = b"{\"id\":2}";
        let mut input = std::io::Cursor::new(
            format!("Content-Length: {}\r\n\r\n", payload.len())
                .into_bytes()
                .into_iter()
                .chain(payload.iter().copied())
                .collect::<Vec<u8>>(),
        );
        let message = read_stdio_message(&mut input).unwrap().unwrap();
        assert_eq!(parse_json_rpc_message(&message).unwrap()["id"], 2);
    }

    #[test]
    fn stdio_reader_returns_none_at_eof() {
        let mut input = std::io::Cursor::new(Vec::new());
        assert!(read_stdio_message(&mut input).unwrap().is_none());
    }

    #[test]
    fn stdio_writer_emits_newline_delimited_json() {
        let mut buffer = Vec::new();
        write_stdio_message(&mut buffer, &json!({ "id": 3 })).unwrap();
        assert_eq!(String::from_utf8(buffer).unwrap(), "{\"id\":3}\n");
    }

    /// A host that answers every approval with the same decision and records
    /// what it was asked about.
    struct ApprovalHost {
        decision: BrowserPermissionDecision,
        asked: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BrowserMcpHost for ApprovalHost {
        async fn resolve_token(&self, _token: &str) -> Option<BrowserMcpSession> {
            None
        }

        async fn request_permission(
            &self,
            _session: &BrowserMcpSession,
            error: &BrowserError,
        ) -> BrowserPermissionDecision {
            self.asked.lock().unwrap().push(error.code.clone());
            self.decision
        }
    }

    fn navigation_approval_error() -> BrowserError {
        crate::policy::NavigationDecision::RequiresApproval {
            origin: "https://other.test".to_string(),
            domain: "other.test".to_string(),
        }
        .approval_error()
        .expect("a cross-origin navigation needs approval")
    }

    fn private_network_approval_error() -> BrowserError {
        crate::policy::NavigationDecision::RequiresApprovalForPrivateNetwork {
            origin: "http://127.0.0.1:2375".to_string(),
            domain: "127.0.0.1".to_string(),
        }
        .approval_error()
        .expect("a private-network navigation needs approval")
    }

    fn handler_with_host(decision: BrowserPermissionDecision) -> BrowserMcpHandler {
        BrowserMcpHandler::new(BrowserService::new(
            crate::service::BrowserServiceConfig::new("/tmp/vibex-browser-approval-test"),
        ))
        .with_host(Arc::new(ApprovalHost {
            decision,
            asked: std::sync::Mutex::new(Vec::new()),
        }))
    }

    #[tokio::test]
    async fn a_navigation_approval_is_requested_even_without_a_tab_id() {
        // The regression: the gate used to require a `tab_id` argument or the
        // substring "approval" in the message, so the most natural
        // `browser_navigate` call — which omits the optional `tab_id` — never
        // reached the human.
        let handler = handler_with_host(BrowserPermissionDecision::Deny);
        let decision = handler
            .approval_for(&session(), &navigation_approval_error())
            .await;
        assert_eq!(decision, Some(BrowserPermissionDecision::Deny));
    }

    #[tokio::test]
    async fn a_private_network_approval_is_requested_too() {
        let handler = handler_with_host(BrowserPermissionDecision::AlwaysAllow);
        let decision = handler
            .approval_for(&session(), &private_network_approval_error())
            .await;
        assert_eq!(decision, Some(BrowserPermissionDecision::AlwaysAllow));
    }

    #[tokio::test]
    async fn a_failure_that_is_not_an_approval_never_raises_a_card() {
        let handler = handler_with_host(BrowserPermissionDecision::Approve);
        let error = BrowserError::validation("browser_element_not_found", "no such element");
        assert_eq!(handler.approval_for(&session(), &error).await, None);
        let missing_path = BrowserError::permission(
            "browser_path_not_authorized",
            "the requested path is outside the authorized roots",
        );
        assert_eq!(handler.approval_for(&session(), &missing_path).await, None);
    }

    #[test]
    fn an_approval_key_is_the_origin_from_the_error_diagnostics() {
        assert_eq!(
            approval_key(&navigation_approval_error()),
            "https://other.test"
        );
        assert_eq!(
            approval_key(&private_network_approval_error()),
            "http://127.0.0.1:2375"
        );
    }

    #[test]
    fn outcomes_render_images_as_mcp_content() {
        let outcome = BrowserToolOutcome {
            text: "shot".to_string(),
            is_error: false,
            images: vec![crate::service::BrowserImageContent {
                mime_type: "image/png".to_string(),
                base64: "AAAA".to_string(),
            }],
            records: Vec::new(),
            dialog: None,
            file_chooser_tab: None,
        };
        let rendered = outcome_to_mcp(&outcome);
        assert_eq!(rendered["content"][0]["type"], "text");
        assert_eq!(rendered["content"][1]["type"], "image");
        assert_eq!(rendered["content"][1]["mimeType"], "image/png");
        assert_eq!(rendered["isError"], false);
    }
}

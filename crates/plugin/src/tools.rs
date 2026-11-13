//! The loopback MCP endpoint that hands plugin tools to Agents.
//!
//! A plugin registers tools through `vibex.agent.tools`; this endpoint is how
//! the Agent learns they exist and how a call reaches the script. It speaks the
//! same Streamable-HTTP MCP dialect the browser tool does, on the same
//! loopback-only terms, so an Agent that can talk to one can talk to the other.
//!
//! # Why the call is a queue and not a direct call
//!
//! `gpui-shell` has no function value, so the host cannot invoke a script
//! function. The call is queued for the plugin's panel, which answers on its
//! next render with `vibex.agent.tools.respond(id, text)`. This endpoint
//! therefore waits: the MCP request stays open until the reply arrives or the
//! timeout expires, and an unanswered call is reported to the Agent as an error
//! rather than as an empty success.
//!
//! # Security
//!
//! * bound to `127.0.0.1`, never a wildcard address;
//! * `Host` must be a loopback authority, which defeats DNS rebinding;
//! * every request carries a bearer token generated at startup;
//! * a body size limit, because any local process can reach the port.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

/// The largest MCP request body accepted.
pub const MAX_MCP_BODY_BYTES: usize = 4 * 1024 * 1024;
/// How long a call waits for the plugin's panel to answer.
///
/// A script answers when it renders, so this has to be long enough for a frame
/// and short enough that an unloaded plugin does not hold the Agent's turn.
pub const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// The protocol revision this endpoint speaks.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// One tool a plugin registered, as the Agent sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginToolDescriptor {
    /// The plugin that owns the tool.
    pub plugin_id: String,
    /// The name the plugin registered.
    pub name: String,
    /// The name the Agent sees: `plugin_<owner>_<name>`.
    ///
    /// Two plugins may both register `search`, and an MCP tool name has to be
    /// unique within one server, so the owner is part of it.
    pub qualified_name: String,
    pub description: String,
}

impl PluginToolDescriptor {
    /// The qualified name for one plugin's tool.
    pub fn qualified(plugin_id: &str, name: &str) -> String {
        let sanitize = |value: &str| {
            value
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '_' {
                        character
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        };
        format!("plugin_{}_{}", sanitize(plugin_id), sanitize(name))
    }
}

/// What the endpoint needs from the host to answer a call.
///
/// Implemented by the desktop host, which owns the channels and the UI thread.
/// The endpoint itself has no access to either, which is what keeps this crate
/// testable without an application.
pub trait PluginToolHost: Send + Sync + 'static {
    /// The tools every enabled plugin currently contributes.
    fn tools(&self) -> Vec<PluginToolDescriptor>;

    /// Queues a call and waits for the plugin's answer.
    ///
    /// Returns the reply text, or an error string the Agent is shown.
    fn call_tool(
        &self,
        plugin_id: &str,
        name: &str,
        arguments: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;
}

#[derive(Clone)]
struct EndpointState {
    host: Arc<dyn PluginToolHost>,
    token: Arc<String>,
}

/// A running plugin-tool MCP endpoint.
pub struct PluginToolEndpoint {
    address: SocketAddr,
    token: Arc<String>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for PluginToolEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginToolEndpoint")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl PluginToolEndpoint {
    /// Binds the endpoint on a random loopback port and starts serving.
    pub async fn start(host: Arc<dyn PluginToolHost>) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let token = Arc::new(new_capability_token());
        let state = EndpointState {
            host,
            token: Arc::clone(&token),
        };
        let router = Router::new().route("/mcp", post(handle)).with_state(state);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            address,
            token,
            task,
        })
    }

    /// The endpoint's URL, for an Agent's MCP configuration.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.address)
    }

    /// The bearer token a client must present.
    pub fn capability_token(&self) -> &str {
        &self.token
    }

    /// Stops serving.
    pub fn stop(&self) {
        self.task.abort();
    }
}

/// A token long enough that it cannot be guessed on the loopback interface.
fn new_capability_token() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    // A time-seeded fallback would be a guessable token, so a failure here is
    // fatal rather than degraded.
    if getrandom::fill(&mut bytes).is_err() {
        panic!("the plugin tool endpoint could not read randomness");
    }
    let mut token = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

async fn handle(State(state): State<EndpointState>, headers: HeaderMap, body: Bytes) -> Response {
    if !host_is_loopback(&headers) {
        return (StatusCode::FORBIDDEN, "the Host header is not loopback").into_response();
    }
    if !bearer_matches(&headers, &state.token) {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    if body.len() > MAX_MCP_BODY_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "the request body is too large",
        )
            .into_response();
    }
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32700, "message": "the body is not JSON" }
        }))
        .into_response();
    };

    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    // A notification never gets a reply.
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };

    let response = match method {
        "initialize" => json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "vibex-plugins", "version": env!("CARGO_PKG_VERSION") }
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools_list(&state.host) }),
        "tools/call" => call_tool(&state.host, &params).await,
        other => {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("`{other}` is not supported by the plugin tool server")
                }
            }))
            .into_response();
        }
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": response })).into_response()
}

/// The `tools/list` payload.
fn tools_list(host: &Arc<dyn PluginToolHost>) -> Vec<Value> {
    host.tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.qualified_name,
                "description": tool.description,
                "inputSchema": { "type": "object", "additionalProperties": true }
            })
        })
        .collect()
}

/// One `tools/call`, waiting for the plugin's answer.
async fn call_tool(host: &Arc<dyn PluginToolHost>, params: &Value) -> Value {
    let qualified = params.get("name").and_then(Value::as_str).unwrap_or("");
    if qualified.is_empty() {
        return error_result("`name` is required");
    }
    let Some(tool) = host
        .tools()
        .into_iter()
        .find(|tool| tool.qualified_name == qualified)
    else {
        return error_result(&format!("`{qualified}` is not a tool any plugin offers"));
    };
    let arguments = params
        .get("arguments")
        .map(|value| value.to_string())
        .unwrap_or_else(|| "{}".to_string());

    match tokio::time::timeout(
        TOOL_CALL_TIMEOUT,
        host.call_tool(&tool.plugin_id, &tool.name, &arguments),
    )
    .await
    {
        Ok(Ok(text)) => json!({ "content": [{ "type": "text", "text": text }] }),
        // The plugin answered with a failure, or the call could not be queued.
        // Either is the Agent's to see, not a silent empty result.
        Ok(Err(error)) => error_result(&error),
        Err(_) => error_result(&format!(
            "the plugin `{}` did not answer `{}` in time",
            tool.plugin_id, tool.name
        )),
    }
}

fn error_result(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
}

/// Whether the request's `Host` names a loopback authority.
fn host_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let host = host.rsplit_once(':').map_or(host, |(host, _)| host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Whether the request carries the endpoint's bearer token.
fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(candidate) = value.strip_prefix("Bearer ") else {
        return false;
    };
    // A constant-time compare: the token is a secret, and a length or prefix
    // leak would let a local process narrow it down.
    constant_time_eq(candidate.trim().as_bytes(), token.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_name_names_the_plugin_that_owns_it() {
        assert_eq!(
            PluginToolDescriptor::qualified("acme.widgets", "search"),
            "plugin_acme_widgets_search"
        );
        // Two plugins may both offer `search` without colliding.
        assert_ne!(
            PluginToolDescriptor::qualified("acme.widgets", "search"),
            PluginToolDescriptor::qualified("other.widgets", "search")
        );
    }

    #[test]
    fn a_bearer_token_is_compared_in_constant_time_and_exactly() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert!(bearer_matches(&headers, "abc123"));
        assert!(!bearer_matches(&headers, "abc124"));
        assert!(!bearer_matches(&headers, "abc12"));
        assert!(!bearer_matches(&headers, "abc1234"));
    }

    #[test]
    fn a_missing_or_malformed_authorization_is_refused() {
        let headers = HeaderMap::new();
        assert!(!bearer_matches(&headers, "abc123"));

        let mut basic = HeaderMap::new();
        basic.insert(
            axum::http::header::AUTHORIZATION,
            "Basic abc123".parse().unwrap(),
        );
        assert!(!bearer_matches(&basic, "abc123"));
    }

    #[test]
    fn only_a_loopback_host_header_is_accepted() {
        for (host, allowed) in [
            ("127.0.0.1:8080", true),
            ("localhost:9000", true),
            ("[::1]:1234", true),
            ("evil.example:80", false),
            ("127.0.0.1.evil.example", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(axum::http::header::HOST, host.parse().unwrap());
            assert_eq!(host_is_loopback(&headers), allowed, "{host}");
        }
        assert!(!host_is_loopback(&HeaderMap::new()));
    }

    /// A host that answers from a fixed list, so the protocol can be driven
    /// without a desktop.
    struct StubHost {
        tools: Vec<PluginToolDescriptor>,
        reply: std::sync::Mutex<Option<String>>,
        seen: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl PluginToolHost for StubHost {
        fn tools(&self) -> Vec<PluginToolDescriptor> {
            self.tools.clone()
        }

        fn call_tool(
            &self,
            plugin_id: &str,
            name: &str,
            arguments: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
        {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((
                    plugin_id.to_string(),
                    name.to_string(),
                    arguments.to_string(),
                ));
            }
            let reply = self
                .reply
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
                .unwrap_or_else(|| "no reply configured".to_string());
            Box::pin(async move { Ok(reply) })
        }
    }

    async fn call(client: &reqwest::Client, url: &str, token: &str, body: Value) -> Value {
        client
            .post(url)
            .header(
                "Host",
                url.split("//").nth(1).unwrap().split('/').next().unwrap(),
            )
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .expect("the request is answered")
            .json()
            .await
            .expect("the body is JSON")
    }

    #[tokio::test]
    async fn the_endpoint_lists_and_calls_a_plugin_tool() {
        let host = Arc::new(StubHost {
            tools: vec![PluginToolDescriptor {
                plugin_id: "acme.widgets".to_string(),
                name: "search".to_string(),
                qualified_name: PluginToolDescriptor::qualified("acme.widgets", "search"),
                description: "Search the widgets".to_string(),
            }],
            reply: std::sync::Mutex::new(Some("three widgets found".to_string())),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let endpoint = PluginToolEndpoint::start(host.clone()).await.unwrap();
        let client = reqwest::Client::new();

        let listed = call(
            &client,
            &endpoint.url(),
            endpoint.capability_token(),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
        let tools = listed["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "plugin_acme_widgets_search");
        assert_eq!(tools[0]["description"], "Search the widgets");

        let called = call(
            &client,
            &endpoint.url(),
            endpoint.capability_token(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "plugin_acme_widgets_search",
                    "arguments": { "query": "widget" }
                }
            }),
        )
        .await;
        assert_eq!(
            called["result"]["content"][0]["text"],
            "three widgets found"
        );
        assert!(called["result"].get("isError").is_none());

        // The plugin was asked for the tool it actually registered, not the
        // qualified name the Agent sees.
        let seen = host.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "acme.widgets");
        assert_eq!(seen[0].1, "search");
        assert!(seen[0].2.contains("widget"));

        endpoint.stop();
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_error_the_agent_can_read() {
        let host = Arc::new(StubHost {
            tools: Vec::new(),
            reply: std::sync::Mutex::new(None),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let endpoint = PluginToolEndpoint::start(host).await.unwrap();
        let client = reqwest::Client::new();
        let response = call(
            &client,
            &endpoint.url(),
            endpoint.capability_token(),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "plugin_nobody_nothing", "arguments": {} }
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true);
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not a tool any plugin offers")
        );
        endpoint.stop();
    }

    #[tokio::test]
    async fn a_request_without_the_bearer_token_is_refused() {
        let host = Arc::new(StubHost {
            tools: Vec::new(),
            reply: std::sync::Mutex::new(None),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let endpoint = PluginToolEndpoint::start(host).await.unwrap();
        let address = endpoint
            .url()
            .split("//")
            .nth(1)
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_string();
        let status = reqwest::Client::new()
            .post(&endpoint.url())
            .header("Host", address)
            .json(&json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
        endpoint.stop();
    }

    #[test]
    fn the_capability_token_is_long_and_not_repeated() {
        let first = new_capability_token();
        let second = new_capability_token();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
    }
}

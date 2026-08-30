//! Local MCP bridge for provider-neutral child Agent delegation.
//!
//! The bridge is deliberately split from provider adapters. ACP launches a
//! short-lived stdio process, while this module's loopback broker is the only
//! component allowed to call the authoritative [`AgentManager`].

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream as StdTcpStream;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use vibex_core::{
    AgentDelegationId, AgentId, CancelAgentDelegationRequest, CreateAgentDelegationRequest,
    ProviderProfileId, VibexError, VibexResult, VibexSessionId,
};

use crate::manager::{AgentDelegationToolConfig, AgentManager};

const MAX_MCP_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_BROKER_LINE_BYTES: usize = 512 * 1024;
pub const AGENT_DELEGATION_MCP_SERVER_ID: &str = "vibex-agent-delegation";

/// Starts the loopback broker and returns the session-independent launch
/// configuration consumed by `runtime_resources_for_session`.
pub async fn start_delegation_broker(
    manager: Arc<AgentManager>,
    command: PathBuf,
) -> VibexResult<(AgentDelegationToolConfig, JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|error| {
        VibexError::process(
            "agent_delegation_broker_bind_failed",
            "failed to bind the local Agent delegation broker",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let address = listener.local_addr().map_err(|error| {
        VibexError::process(
            "agent_delegation_broker_address_failed",
            "failed to determine the local Agent delegation broker address",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let global_token = format!("cap_{}", AgentDelegationId::new().as_str());
    let endpoint = format!("127.0.0.1:{}", address.port());
    let broker_manager = manager.clone();
    let broker_token = global_token.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        target: "vibex_agent",
                        error = %error,
                        "Agent delegation broker accept failed"
                    );
                    continue;
                }
            };
            let manager = broker_manager.clone();
            let token = broker_token.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_broker_connection(stream, manager, token).await {
                    tracing::debug!(
                        target: "vibex_agent",
                        error_code = %error.code,
                        "Agent delegation broker connection closed"
                    );
                }
            });
        }
    });
    Ok((
        AgentDelegationToolConfig {
            command,
            broker_endpoint: endpoint,
            capability_token: global_token,
        },
        task,
    ))
}

/// Derives a capability token scoped to one parent session. The broker never
/// accepts the global token on a request, which prevents one MCP process from
/// claiming another session's delegation authority.
pub fn session_capability_token(global_token: &str, session_id: &VibexSessionId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(global_token.as_bytes());
    hasher.update([0]);
    hasher.update(session_id.as_str().as_bytes());
    format!("session_{}", hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

async fn serve_broker_connection(
    stream: TcpStream,
    manager: Arc<AgentManager>,
    global_token: String,
) -> VibexResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = AsyncBufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await.map_err(|error| {
        VibexError::process(
            "agent_delegation_broker_read_failed",
            "failed to read Agent delegation broker request",
        )
        .with_diagnostic("error", error.to_string())
    })? {
        if line.len() > MAX_BROKER_LINE_BYTES {
            let response =
                broker_error("agent_delegation_request_too_large", "request is too large");
            writer.write_all(response.to_string().as_bytes()).await.ok();
            writer.write_all(b"\n").await.ok();
            continue;
        }
        let request: BrokerRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(_) => {
                let response = broker_error(
                    "agent_delegation_request_invalid",
                    "request is invalid JSON",
                );
                writer.write_all(response.to_string().as_bytes()).await.ok();
                writer.write_all(b"\n").await.ok();
                continue;
            }
        };
        let response = handle_broker_request(&manager, &global_token, request).await;
        writer
            .write_all(response.to_string().as_bytes())
            .await
            .map_err(|error| {
                VibexError::process(
                    "agent_delegation_broker_write_failed",
                    "failed to write Agent delegation broker response",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        writer.write_all(b"\n").await.map_err(|error| {
            VibexError::process(
                "agent_delegation_broker_write_failed",
                "failed to finish Agent delegation broker response",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrokerRequest {
    token: String,
    parent_session_id: String,
    method: String,
    #[serde(default)]
    params: Value,
}

async fn handle_broker_request(
    manager: &Arc<AgentManager>,
    global_token: &str,
    request: BrokerRequest,
) -> Value {
    let parent_session_id = match VibexSessionId::parse(request.parent_session_id.clone()) {
        Ok(id) => id,
        Err(_) => {
            return broker_error(
                "agent_delegation_parent_invalid",
                "parent session is invalid",
            );
        }
    };
    let expected = session_capability_token(global_token, &parent_session_id);
    if request.token != expected {
        return broker_error(
            "agent_delegation_unauthorized",
            "delegation capability is invalid",
        );
    }
    let result = match request.method.as_str() {
        "delegate_to_agent" => delegate(manager, parent_session_id, request.params).await,
        "get_delegation_status" => get_status(manager, parent_session_id, request.params),
        "cancel_delegation" => cancel(manager, parent_session_id, request.params).await,
        _ => Err(VibexError::validation(
            "agent_delegation_method_not_found",
            "delegation method was not found",
        )),
    };
    match result {
        Ok(value) => json!({ "ok": true, "value": value }),
        Err(error) => broker_error(&error.code, &error.message),
    }
}

async fn delegate(
    manager: &Arc<AgentManager>,
    parent_session_id: VibexSessionId,
    params: Value,
) -> VibexResult<Value> {
    let object = params.as_object().ok_or_else(|| {
        VibexError::validation(
            "agent_delegation_params_invalid",
            "delegation parameters must be an object",
        )
    })?;
    let task = object
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let title = optional_string(object, "title");
    let agent_id = parse_optional_id(object, "agentId", AgentId::parse)?;
    let provider_profile_id =
        parse_optional_id(object, "providerProfileId", ProviderProfileId::parse)?;
    let request = CreateAgentDelegationRequest {
        parent_session_id,
        idempotency_key: optional_string(object, "idempotencyKey").unwrap_or_default(),
        task,
        title,
        agent_id,
        provider_profile_id,
        model: optional_string(object, "model"),
        reasoning_effort: optional_string(object, "reasoningEffort"),
        mode_id: optional_string(object, "modeId"),
    };
    serde_json::to_value(manager.create_agent_delegation(request).await?).map_err(|error| {
        VibexError::process(
            "agent_delegation_encode_failed",
            "failed to encode Agent delegation result",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn get_status(
    manager: &Arc<AgentManager>,
    parent_session_id: VibexSessionId,
    params: Value,
) -> VibexResult<Value> {
    let delegation_id = parse_required_id(&params, "delegationId", AgentDelegationId::parse)?;
    serde_json::to_value(manager.get_agent_delegation(&parent_session_id, &delegation_id)?).map_err(
        |error| {
            VibexError::process(
                "agent_delegation_encode_failed",
                "failed to encode Agent delegation result",
            )
            .with_diagnostic("error", error.to_string())
        },
    )
}

async fn cancel(
    manager: &Arc<AgentManager>,
    parent_session_id: VibexSessionId,
    params: Value,
) -> VibexResult<Value> {
    let delegation_id = parse_required_id(&params, "delegationId", AgentDelegationId::parse)?;
    let value = manager
        .cancel_agent_delegation(CancelAgentDelegationRequest {
            parent_session_id,
            delegation_id,
        })
        .await?;
    serde_json::to_value(value).map_err(|error| {
        VibexError::process(
            "agent_delegation_encode_failed",
            "failed to encode Agent delegation result",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn broker_error(code: &str, message: &str) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message } })
}

fn optional_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_optional_id<T>(
    object: &serde_json::Map<String, Value>,
    key: &str,
    parser: impl Fn(String) -> VibexResult<T>,
) -> VibexResult<Option<T>> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(|value| parser(value.to_string()))
        .transpose()
}

fn parse_required_id<T>(
    params: &Value,
    key: &str,
    parser: impl Fn(String) -> VibexResult<T>,
) -> VibexResult<T> {
    params
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VibexError::validation("agent_delegation_id_missing", "delegation id is required")
        })
        .and_then(|value| parser(value.to_string()))
}

/// Runs the executable's stdio MCP mode. It supports both newline-delimited
/// JSON and Content-Length framed JSON so it can be used by ACP dialects that
/// choose either standard framing convention.
pub fn run_delegation_mcp_stdio() -> Result<(), String> {
    let endpoint = std::env::var("VIBEX_AGENT_DELEGATION_ENDPOINT")
        .map_err(|_| "delegation broker endpoint is missing".to_string())?;
    let token = std::env::var("VIBEX_AGENT_DELEGATION_TOKEN")
        .map_err(|_| "delegation capability token is missing".to_string())?;
    let parent_session_id = std::env::var("VIBEX_AGENT_DELEGATION_PARENT_SESSION")
        .map_err(|_| "delegation parent session is missing".to_string())?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    while let Some(message) = read_stdio_message(&mut reader).map_err(|error| error.to_string())? {
        let response = handle_mcp_message(&endpoint, &token, &parent_session_id, message);
        if let Some(response) = response {
            write_stdio_message(&mut writer, &response).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn handle_mcp_message(endpoint: &str, token: &str, parent: &str, message: Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "notifications/initialized" | "notifications/cancelled" => None,
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": message
                    .get("params")
                    .and_then(|params| params.get("protocolVersion"))
                    .cloned()
                    .unwrap_or_else(|| json!("2024-11-05")),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "vibex-agent-delegation", "version": env!("CARGO_PKG_VERSION") }
            }
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": delegation_tool_definitions() }
        })),
        "tools/call" => {
            let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let broker_result = call_broker(endpoint, token, parent, name, arguments);
            let (is_error, text) = match broker_result {
                Ok(value) => (false, value.to_string()),
                Err(error) => (true, error),
            };
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "isError": is_error,
                    "content": [{ "type": "text", "text": text }]
                }
            }))
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "method not found" }
        })),
    }
}

fn delegation_tool_definitions() -> Value {
    json!([
        {
            "name": "delegate_to_agent",
            "description": "Run a bounded task in an independent child Agent session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "The task to run." },
                    "title": { "type": "string" },
                    "agentId": { "type": "string" },
                    "providerProfileId": { "type": "string" },
                    "model": { "type": "string" },
                    "reasoningEffort": { "type": "string" },
                    "modeId": { "type": "string" },
                    "idempotencyKey": { "type": "string" }
                },
                "required": ["task"]
            }
        },
        {
            "name": "get_delegation_status",
            "description": "Read the current status and bounded result of a child Agent task.",
            "inputSchema": {
                "type": "object",
                "properties": { "delegationId": { "type": "string" } },
                "required": ["delegationId"]
            }
        },
        {
            "name": "cancel_delegation",
            "description": "Cancel a child Agent task owned by this parent session.",
            "inputSchema": {
                "type": "object",
                "properties": { "delegationId": { "type": "string" } },
                "required": ["delegationId"]
            }
        }
    ])
}

fn call_broker(
    endpoint: &str,
    token: &str,
    parent: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut stream = StdTcpStream::connect(endpoint).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    let request = json!({
        "token": token,
        "parentSessionId": parent,
        "method": method,
        "params": params,
    });
    stream
        .write_all(request.to_string().as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("value").cloned().unwrap_or(Value::Null))
    } else {
        Err(response
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("delegation broker request failed")
            .to_string())
    }
}

fn read_stdio_message(reader: &mut BufReader<impl Read>) -> io::Result<Option<Value>> {
    let mut first_line = String::new();
    loop {
        first_line.clear();
        let read = reader.read_line(&mut first_line)?;
        if read == 0 {
            return Ok(None);
        }
        if first_line.trim().is_empty() {
            continue;
        }
        break;
    }
    let payload = if first_line
        .to_ascii_lowercase()
        .starts_with("content-length:")
    {
        let length = first_line
            .split_once(':')
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length"))?;
        if length > MAX_MCP_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message too large",
            ));
        }
        let mut header_line = String::new();
        reader.read_line(&mut header_line)?;
        while !header_line.trim().is_empty() {
            header_line.clear();
            reader.read_line(&mut header_line)?;
        }
        let mut bytes = vec![0_u8; length];
        reader.read_exact(&mut bytes)?;
        bytes
    } else {
        first_line.into_bytes()
    };
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_stdio_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() > MAX_MCP_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP response too large",
        ));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tokens_are_scoped_and_stable() {
        let session = VibexSessionId::new();
        let one = session_capability_token("global", &session);
        assert_eq!(one, session_capability_token("global", &session));
        assert_ne!(
            one,
            session_capability_token("global", &VibexSessionId::new())
        );
        assert_ne!(one, session_capability_token("other", &session));
    }

    #[test]
    fn tool_list_exposes_generic_contract() {
        let tools = delegation_tool_definitions();
        assert_eq!(tools.as_array().map(Vec::len), Some(3));
        assert_eq!(tools[0]["name"], "delegate_to_agent");
    }

    #[test]
    fn mcp_initialize_and_tool_list_use_the_standard_contract() {
        let initialize = handle_mcp_message(
            "127.0.0.1:1",
            "capability",
            "session_parent",
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "initialize",
                "params": { "protocolVersion": "2025-03-26" }
            }),
        )
        .unwrap();
        assert_eq!(initialize["id"], 7);
        assert_eq!(initialize["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(
            initialize["result"]["capabilities"]["tools"]["listChanged"],
            false
        );

        let list = handle_mcp_message(
            "127.0.0.1:1",
            "capability",
            "session_parent",
            json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list" }),
        )
        .unwrap();
        assert_eq!(list["id"], 8);
        assert_eq!(list["result"]["tools"].as_array().map(Vec::len), Some(3));
    }
}

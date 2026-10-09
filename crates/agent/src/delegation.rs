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
    AgentDelegation, AgentDelegationId, AgentDelegationStatus, AgentId,
    CancelAgentDelegationRequest, CreateAgentDelegationRequest, ErrorCategory, ProviderProfileId,
    VibexError, VibexResult, VibexSessionId, VibexUseActor, VibexUseTool,
};

use crate::manager::{AgentDelegationToolConfig, AgentManager};

const MAX_MCP_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_BROKER_LINE_BYTES: usize = 512 * 1024;
pub const AGENT_DELEGATION_MCP_SERVER_ID: &str = "vibex-agent-delegation";
/// Environment variable carrying the runtime authority that issued this
/// sidecar's capability. A reference is only ever resolved under the authority
/// that minted it.
pub const AGENT_DELEGATION_AUTHORITY_ENV: &str = "VIBEX_AGENT_DELEGATION_AUTHORITY";
/// Environment variable carrying the delivery activation revision the sidecar
/// was launched under. A call is re-validated against the live revision.
pub const AGENT_DELEGATION_ACTIVATION_ENV: &str = "VIBEX_AGENT_DELEGATION_ACTIVATION";

/// Starts the loopback broker and returns the session-independent launch
/// configuration consumed by `runtime_resources_for_session`.
pub async fn start_delegation_broker(
    manager: Arc<AgentManager>,
    command: PathBuf,
    authority: String,
    activation_revision: u64,
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
            authority,
            activation_revision,
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

/// Product-safe result returned to the Agent through MCP. The desktop and
/// remote UI still receive the full `AgentDelegation` DTO, but an Agent only
/// needs the delegation id to poll. In particular, internal session ids must
/// not be handed to unrelated session-context tools with a different id
/// format.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentDelegationToolResult {
    id: AgentDelegationId,
    idempotency_key: String,
    title: String,
    task_summary: String,
    requested_agent_id: Option<AgentId>,
    effective_agent_id: Option<AgentId>,
    status: AgentDelegationStatus,
    result_summary: Option<String>,
    error_code: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
}

impl From<AgentDelegation> for AgentDelegationToolResult {
    fn from(delegation: AgentDelegation) -> Self {
        Self {
            id: delegation.id,
            idempotency_key: delegation.idempotency_key,
            title: delegation.title,
            task_summary: delegation.task_summary,
            requested_agent_id: delegation.requested_agent_id,
            effective_agent_id: delegation.effective_agent_id,
            status: delegation.status,
            result_summary: delegation.result_summary,
            error_code: delegation.error_code,
            created_at_ms: delegation.created_at_ms,
            updated_at_ms: delegation.updated_at_ms,
            started_at_ms: delegation.started_at_ms,
            completed_at_ms: delegation.completed_at_ms,
        }
    }
}

fn encode_delegation_tool_result(delegation: AgentDelegation) -> VibexResult<Value> {
    serde_json::to_value(AgentDelegationToolResult::from(delegation)).map_err(|error| {
        VibexError::process(
            "agent_delegation_encode_failed",
            "failed to encode Agent delegation result",
        )
        .with_diagnostic("error", error.to_string())
    })
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
        "list_tools" => list_tools(manager, parent_session_id),
        method if VibexUseTool::parse(method).is_some() => {
            vibex_use(manager, parent_session_id, method, request.params).await
        }
        _ => Err(VibexError::validation(
            "agent_delegation_method_not_found",
            "delegation method was not found",
        )),
    };
    match result {
        Ok(value) => json!({ "ok": true, "value": value }),
        Err(error) => broker_error_value(&error),
    }
}

/// Merges the historical tools with whatever Vibex-use can really deliver to
/// this caller.
///
/// A tool that cannot succeed is omitted instead of advertised and then
/// refused. The legacy three are always present because their behaviour does
/// not depend on a presentation client.
fn list_tools(
    manager: &Arc<AgentManager>,
    parent_session_id: VibexSessionId,
) -> VibexResult<Value> {
    let mut tools = delegation_tool_definitions()
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(host) = manager.vibex_use_host() {
        let actor = VibexUseActor::new(
            manager.vibex_use_authority(),
            parent_session_id,
            manager.vibex_use_activation_revision(),
        );
        for definition in host.tool_definitions(&actor) {
            tools.push(json!({
                "name": definition.name,
                "description": definition.description,
                "inputSchema": definition.input_schema,
            }));
        }
    }
    Ok(json!({ "tools": tools }))
}

/// Forwards one Vibex-use tool call to the installed domain service.
///
/// The sidecar owns framing only. It derives the actor from the capability it
/// was launched with, so a tool argument can never claim another parent
/// session, and the service behind the host re-authorizes every reference.
async fn vibex_use(
    manager: &Arc<AgentManager>,
    parent_session_id: VibexSessionId,
    method: &str,
    params: Value,
) -> VibexResult<Value> {
    let Some(tool) = VibexUseTool::parse(method) else {
        return Err(VibexError::validation(
            "agent_delegation_method_not_found",
            "delegation method was not found",
        ));
    };
    let Some(host) = manager.vibex_use_host() else {
        return Err(VibexError::capability(
            "vibex_use_unavailable",
            "Vibex-use is not available in this runtime",
        ));
    };
    let actor = VibexUseActor::new(
        manager.vibex_use_authority(),
        parent_session_id,
        manager.vibex_use_activation_revision(),
    );
    // The arguments may be absent for a schema-less call; every tool parses its
    // own fields and rejects what it cannot use.
    let arguments = if params.is_object() {
        params
    } else {
        Value::Object(serde_json::Map::new())
    };
    host.call(actor, tool, arguments).await
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
        // The historical bridge keeps its single-turn completion semantics and
        // owns the session it creates.
        completion_policy: vibex_core::DelegationCompletionPolicy::SingleTurnLegacy,
        ownership_kind: vibex_core::DelegationOwnershipKind::OwnedChild,
        context_refs: Vec::new(),
        acceptance_criteria: Vec::new(),
        follows_task_id: None,
        existing_session_id: None,
    };
    encode_delegation_tool_result(manager.create_agent_delegation(request).await?)
}

fn get_status(
    manager: &Arc<AgentManager>,
    parent_session_id: VibexSessionId,
    params: Value,
) -> VibexResult<Value> {
    let delegation_id = parse_required_id(&params, "delegationId", AgentDelegationId::parse)?;
    encode_delegation_tool_result(manager.get_agent_delegation(&parent_session_id, &delegation_id)?)
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
    encode_delegation_tool_result(value)
}

fn broker_error(code: &str, message: &str) -> Value {
    broker_error_value(&VibexError::new(
        ErrorCategory::Process,
        code.to_string(),
        message.to_string(),
    ))
}

/// The structured error an Agent receives.
///
/// It carries what the caller can act on — whether a retry is safe, what to do
/// next, and which values were involved — instead of a bare sentence the model
/// has to interpret. It never carries credentials, prompts or raw tool output.
fn broker_error_value(error: &VibexError) -> Value {
    let retryable = matches!(
        error.category,
        ErrorCategory::Storage | ErrorCategory::Provider | ErrorCategory::Process
    );
    let mut value = json!({
        "ok": false,
        "error": {
            "code": error.code,
            "message": error.message,
            "category": error.category,
            "retryable": retryable,
        }
    });
    let error_object = value
        .get_mut("error")
        .and_then(Value::as_object_mut)
        .expect("the error object was just built");
    if let Some(hint) = error
        .recovery_hint
        .as_deref()
        .filter(|hint| !hint.is_empty())
    {
        error_object.insert("recoveryHint".to_string(), json!(hint));
    }
    if !error.diagnostics.is_empty() {
        error_object.insert(
            "diagnostics".to_string(),
            Value::Array(
                error
                    .diagnostics
                    .iter()
                    .map(|diagnostic| json!({ "key": diagnostic.key, "value": diagnostic.value }))
                    .collect(),
            ),
        );
    }
    value
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
            "result": {
                "tools": broker_tool_definitions(endpoint, token, parent)
            }
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
            // The structured payload and the text are the same value rendered
            // two ways, so a host that only reads text and a host that reads
            // structuredContent cannot disagree about what happened.
            let (is_error, structured) = match broker_result {
                Ok(value) => (false, value),
                Err(error) => (true, error),
            };
            let text = structured.to_string();
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "isError": is_error,
                    "content": [{ "type": "text", "text": text }],
                    "structuredContent": structured
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

/// Asks the broker for the catalogue of this activation.
///
/// The sidecar deliberately caches nothing: a target disabled, or a
/// presentation client that went away, after `initialize` must change what the
/// next `tools/list` reports instead of leaving a stale promise in place.
fn broker_tool_definitions(endpoint: &str, token: &str, parent: &str) -> Value {
    // A transient broker failure falls back to the historical three so a
    // working session is never left with no tools at all; the next activation
    // (or the next `tools/list`) picks the full catalogue back up.
    match call_broker(endpoint, token, parent, "list_tools", json!({})) {
        Ok(value) => value
            .get("tools")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        Err(_) => delegation_tool_definitions(),
    }
}

/// The historical three-tool contract, kept byte-compatible so an existing
/// Agent session keeps working after the Vibex-use upgrade.
fn delegation_tool_definitions() -> Value {
    json!([
        {
            "name": "delegate_to_agent",
            "description": "Run a bounded task in an independent child Agent session. The response object's id is the delegationId to use for polling, and internal session ids are intentionally omitted. Omit model to inherit the parent session model; a supplied model must be configured for the selected Agent Profile.",
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
            "description": "Read the current status and bounded result of a child Agent task using delegationId. The response intentionally omits internal session ids; resultSummary is bounded.",
            "inputSchema": {
                "type": "object",
                "properties": { "delegationId": { "type": "string" } },
                "required": ["delegationId"]
            }
        },
        {
            "name": "cancel_delegation",
            "description": "Cancel a child Agent task owned by this parent session using delegationId. The response intentionally omits internal session ids.",
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
) -> Result<Value, Value> {
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
        // The broker already answered with a structured error; forwarding it
        // whole is what lets the model see retryability and the recovery action
        // instead of re-parsing a sentence.
        Err(response.get("error").cloned().unwrap_or_else(|| {
            json!({
                "code": "agent_delegation_request_failed",
                "message": "delegation broker request failed",
                "retryable": true,
            })
        }))
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
    use vibex_core::VibexUseToolHost;

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
        assert!(
            tools[0]["description"]
                .as_str()
                .is_some_and(|description| description.contains("object's id is the delegationId"))
        );
    }

    #[test]
    fn agent_results_do_not_expose_internal_session_ids() {
        let mut delegation = AgentDelegation::single_turn_legacy(
            VibexSessionId::new(),
            "test",
            "Child task",
            "Inspect the project",
            Some(AgentId::parse("zcode").unwrap()),
            AgentDelegationStatus::Running,
            1,
        );
        delegation.child_session_id = Some(VibexSessionId::new());
        let result = serde_json::to_value(AgentDelegationToolResult::from(delegation)).unwrap();

        assert!(result.get("delegationId").is_none());
        assert!(result.get("parentSessionId").is_none());
        assert!(result.get("childSessionId").is_none());
        assert!(result.get("id").is_some());
        assert_eq!(result["status"], "running");
    }

    /// A host that only knows one tool, used to prove the catalogue merges.
    struct OneToolHost;

    impl VibexUseToolHost for OneToolHost {
        fn call(
            &self,
            _actor: VibexUseActor,
            _tool: VibexUseTool,
            _arguments: Value,
        ) -> vibex_core::VibexUseToolFuture<'_> {
            Box::pin(async { Ok(json!({ "ok": true })) })
        }

        fn tool_definitions(
            &self,
            _actor: &VibexUseActor,
        ) -> Vec<vibex_core::VibexUseToolDefinition> {
            vec![vibex_core::VibexUseToolDefinition {
                name: "vibex_delegate".to_string(),
                description: "Delegate work".to_string(),
                input_schema: json!({ "type": "object" }),
            }]
        }
    }

    #[test]
    fn the_broker_catalogue_merges_the_legacy_tools_with_the_installed_host() {
        let db_path = std::env::temp_dir().join(format!(
            "vibex-agent-delegation-{}",
            AgentDelegationId::new()
        ));
        let manager = Arc::new(AgentManager::new(&db_path).unwrap());
        let parent = VibexSessionId::new();

        // With no host installed the catalogue is exactly the historical three,
        // so an older runtime keeps offering exactly what it can do.
        let legacy = list_tools(&manager, parent.clone()).unwrap();
        assert_eq!(legacy["tools"].as_array().map(Vec::len), Some(3));

        let host: Arc<dyn VibexUseToolHost> = Arc::new(OneToolHost);
        manager.install_vibex_use_host(&host).unwrap();
        let merged = list_tools(&manager, parent).unwrap();
        let names: Vec<&str> = merged["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"vibex_delegate"));
        assert!(names.contains(&"delegate_to_agent"));
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn an_unknown_vibex_use_host_call_is_refused_not_guessed() {
        let db_path = std::env::temp_dir().join(format!(
            "vibex-agent-delegation-{}",
            AgentDelegationId::new()
        ));
        let manager = Arc::new(AgentManager::new(&db_path).unwrap());
        // No host installed: the call reports the capability gap instead of
        // silently falling back to the legacy three-tool behaviour.
        let error = vibex_use(&manager, VibexSessionId::new(), "vibex_delegate", json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "vibex_use_unavailable");
        let _ = std::fs::remove_file(&db_path);
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

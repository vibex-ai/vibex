//! Local MCP bridge for provider-neutral child Agent delegation.
//!
//! The bridge is deliberately split from provider adapters. ACP launches a
//! short-lived stdio process, while this module's loopback broker is the only
//! component allowed to call the authoritative [`AgentManager`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::BufReader as AsyncBufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use vibex_core::{
    AgentDelegation, AgentDelegationId, AgentDelegationStatus, AgentId, ErrorCategory,
    ProviderProfileId, VibexError, VibexResult, VibexSessionId, VibexUseActor, VibexUseRef,
    VibexUseTool, VibexUseToolHost,
};

use crate::manager::{AgentDelegationToolConfig, AgentManager};

mod framing;
mod stdio;

pub use stdio::run_delegation_mcp_stdio;

use framing::{encode_json, read_bounded_line, write_frame};

const MAX_MCP_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_BROKER_LINE_BYTES: usize = 512 * 1024;
const MAX_BROKER_CONNECTIONS: usize = 128;
const BROKER_IO_TIMEOUT: Duration = Duration::from_secs(5);
const BROKER_CALL_TIMEOUT: Duration = Duration::from_secs(30);
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
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                _ = connections.join_next(), if !connections.is_empty() => {}
                accepted = listener.accept(), if connections.len() < MAX_BROKER_CONNECTIONS => {
                    let (stream, _) = match accepted {
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
                    connections.spawn(async move {
                        if let Err(error) = serve_broker_connection(stream, manager, token).await {
                            tracing::debug!(
                                target: "vibex_agent",
                                error_code = %error.code,
                                "Agent delegation broker connection closed"
                            );
                        }
                    });
                }
            }
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

/// Binds a credential to its issued authority, parent, and activation. A
/// sidecar cannot upgrade an old credential by changing its revision field.
pub fn session_activation_capability_token(
    global_token: &str,
    session_id: &VibexSessionId,
    authority: &str,
    activation_revision: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(global_token.as_bytes());
    hasher.update(b"\0vibex-agent-delegation-activation-v1\0");
    for value in [session_id.as_str(), authority] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(activation_revision.to_be_bytes());
    format!("activation_{}", hex_lower(&hasher.finalize()))
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
    serve_broker_connection_with_timeout(stream, manager, global_token, BROKER_CALL_TIMEOUT).await
}

async fn serve_broker_connection_with_timeout(
    stream: TcpStream,
    manager: Arc<AgentManager>,
    global_token: String,
    call_timeout: Duration,
) -> VibexResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = AsyncBufReader::new(reader);
    let Some(line) = tokio::time::timeout(
        BROKER_IO_TIMEOUT,
        read_bounded_line(&mut reader, MAX_BROKER_LINE_BYTES),
    )
    .await
    .map_err(|_| broker_timeout())?
    .map_err(|_| broker_io_error("agent_delegation_broker_read_failed"))?
    else {
        return Ok(());
    };
    let request = match serde_json::from_slice::<BrokerRequest>(&line) {
        Ok(request) => request,
        Err(_) => {
            return write_broker_response(
                &mut writer,
                &broker_error("agent_delegation_request_invalid", "request is invalid"),
            )
            .await;
        }
    };
    let (host, actor) = match authorize_broker_request(&manager, &global_token, &request) {
        Ok(authorized) => authorized,
        Err(error) => return write_broker_response(&mut writer, &broker_error_value(&error)).await,
    };
    let result = {
        let call = handle_broker_request(&manager, &host, &actor, &request.method, request.params);
        // Each call has its own connection. Closing it cancels this request's
        // waiter, without interpreting an MCP cancellation as task cancellation.
        let disconnected = read_bounded_line(&mut reader, 1024);
        let deadline = tokio::time::sleep(call_timeout);
        tokio::pin!(call, disconnected, deadline);
        let mut activation_check = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                biased;
                _ = &mut disconnected => return Ok(()),
                _ = activation_check.tick() => {
                    if let Err(error) = host.authorize_actor(&actor) {
                        break Err(error);
                    }
                }
                value = &mut call => break value,
                _ = &mut deadline => break Err(broker_timeout()),
            }
        }
    };
    let result = result.and_then(|value| host.authorize_actor(&actor).map(|()| value));
    let (mut response, delivery_tool) = match result {
        Ok((value, tool)) => {
            let has_events = value
                .get("events")
                .and_then(Value::as_array)
                .is_some_and(|events| !events.is_empty());
            (
                json!({ "ok": true, "value": value }),
                tool.filter(|_| has_events),
            )
        }
        Err(error) => (broker_error_value(&error), None),
    };
    let receipt = delivery_tool.map(|_| format!("delivery_{}", AgentDelegationId::new()));
    if let Some(receipt) = receipt.as_ref() {
        response["deliveryReceipt"] = json!(receipt);
    }
    if encode_json(&response, MAX_BROKER_LINE_BYTES - 1).is_err() {
        return write_broker_response(
            &mut writer,
            &broker_error(
                "vibex_use_response_too_large",
                "request a smaller result page",
            ),
        )
        .await;
    }
    write_broker_response(&mut writer, &response).await?;
    let (Some(receipt), Some(tool)) = (receipt, delivery_tool) else {
        return Ok(());
    };
    let confirmation =
        tokio::time::timeout(BROKER_IO_TIMEOUT, read_bounded_line(&mut reader, 1024)).await;
    let Ok(Ok(Some(confirmation))) = confirmation else {
        // The result remains unacknowledged and may be delivered again.
        return Ok(());
    };
    let confirmation: Value = serde_json::from_slice(&confirmation)
        .map_err(|_| broker_io_error("agent_delegation_receipt_invalid"))?;
    if confirmation.get("deliveryReceipt").and_then(Value::as_str) != Some(receipt.as_str()) {
        return Err(broker_io_error("agent_delegation_receipt_invalid"));
    }
    // This receipt is only sent after the sidecar flushes stdout. It carries
    // no event ids: the host receives the exact result kept on this connection.
    let delivered = host
        .authorize_actor(&actor)
        .and_then(|()| host.response_delivered(&actor, tool, &response["value"]));
    let response = match delivered {
        Ok(()) => json!({ "ok": true }),
        Err(error) => broker_error_value(&error),
    };
    write_broker_response(&mut writer, &response).await
}

async fn write_broker_response(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    response: &Value,
) -> VibexResult<()> {
    let bytes = encode_json(response, MAX_BROKER_LINE_BYTES - 1)
        .map_err(|_| broker_io_error("agent_delegation_response_too_large"))?;
    tokio::time::timeout(BROKER_IO_TIMEOUT, write_frame(writer, &bytes))
        .await
        .map_err(|_| broker_timeout())?
        .map_err(|_| broker_io_error("agent_delegation_broker_write_failed"))
}

fn broker_timeout() -> VibexError {
    VibexError::process(
        "agent_delegation_broker_timeout",
        "the delegation broker request timed out",
    )
}

fn broker_io_error(code: &'static str) -> VibexError {
    VibexError::process(
        code,
        "the delegation broker connection could not complete the request",
    )
}

// Credentials and prompt parameters deliberately do not implement Debug.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrokerRequest {
    token: String,
    parent_session_id: String,
    authority: String,
    activation_revision: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

fn authorize_broker_request(
    manager: &AgentManager,
    global_token: &str,
    request: &BrokerRequest,
) -> VibexResult<(Arc<dyn VibexUseToolHost>, VibexUseActor)> {
    let parent_session_id =
        VibexSessionId::parse(request.parent_session_id.clone()).map_err(|_| {
            VibexError::validation(
                "agent_delegation_parent_invalid",
                "parent session is invalid",
            )
        })?;
    let expected = session_activation_capability_token(
        global_token,
        &parent_session_id,
        &request.authority,
        request.activation_revision,
    );
    let difference = expected
        .bytes()
        .zip(request.token.bytes())
        .fold(0_u8, |difference, (expected, supplied)| {
            difference | (expected ^ supplied)
        });
    if request.token.len() != expected.len() || difference != 0 {
        return Err(VibexError::new(
            ErrorCategory::Permission,
            "agent_delegation_unauthorized",
            "delegation capability is invalid",
        ));
    }
    let host = manager.vibex_use_host().ok_or_else(|| {
        VibexError::capability(
            "vibex_use_unavailable",
            "Vibex-use is not available in this runtime",
        )
    })?;
    let actor = VibexUseActor::new(
        request.authority.clone(),
        parent_session_id,
        request.activation_revision,
    );
    host.authorize_actor(&actor)?;
    Ok((host, actor))
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
    host: &Arc<dyn VibexUseToolHost>,
    actor: &VibexUseActor,
    method: &str,
    params: Value,
) -> VibexResult<(Value, Option<VibexUseTool>)> {
    host.authorize_actor(actor)?;
    match method {
        "list_tools" => Ok((list_tools(host.as_ref(), actor), None)),
        "delegate_to_agent" => {
            let arguments = legacy_delegate_arguments(&params)?;
            let accepted = host
                .call(actor.clone(), VibexUseTool::Delegate, arguments)
                .await?;
            let reference = accepted
                .get("taskRef")
                .or_else(|| {
                    accepted
                        .get("tasks")
                        .and_then(Value::as_array)
                        .and_then(|tasks| tasks.first())
                        .and_then(|task| task.get("taskRef"))
                })
                .and_then(Value::as_str)
                .and_then(VibexUseRef::parse)
                .and_then(|reference| reference.task_id())
                .ok_or_else(|| {
                    VibexError::conflict(
                        "agent_delegation_operation_pending",
                        "delegation is still being accepted; retry with the same idempotency key",
                    )
                })?;
            let value = encode_delegation_tool_result(
                manager.get_agent_delegation(&actor.session_id, &reference)?,
            )?;
            Ok((value, None))
        }
        "get_delegation_status" | "cancel_delegation" => {
            let delegation_id =
                parse_required_id(&params, "delegationId", AgentDelegationId::parse)?;
            let task_ref = VibexUseRef::task(&delegation_id).as_uri();
            if method == "get_delegation_status" {
                host.call(
                    actor.clone(),
                    VibexUseTool::GetTasks,
                    json!({
                        "taskRefs": [task_ref], "includeFinished": true,
                    }),
                )
                .await?;
            } else {
                // Preserve the legacy direct-parent restriction before a
                // mutation, then apply the shared scope and cancellation policy.
                manager.get_agent_delegation(&actor.session_id, &delegation_id)?;
                host.call(
                    actor.clone(),
                    VibexUseTool::CancelTask,
                    json!({
                        "taskRef": task_ref,
                        "idempotencyKey": format!("legacy-cancel-{}", delegation_id.as_str()),
                    }),
                )
                .await?;
            }
            let value = encode_delegation_tool_result(
                manager.get_agent_delegation(&actor.session_id, &delegation_id)?,
            )?;
            Ok((value, None))
        }
        method => {
            let tool = VibexUseTool::parse(method).ok_or_else(|| {
                VibexError::validation(
                    "agent_delegation_method_not_found",
                    "delegation method was not found",
                )
            })?;
            if !params.is_object() {
                return Err(VibexError::validation(
                    "agent_delegation_params_invalid",
                    "tool arguments must be an object",
                ));
            }
            let value = host.call(actor.clone(), tool, params).await?;
            Ok((value, Some(tool)))
        }
    }
}

/// Legacy aliases are offered only when the corresponding domain capability
/// is offered. An unavailable host never creates a policy bypass.
fn list_tools(host: &dyn VibexUseToolHost, actor: &VibexUseActor) -> Value {
    let definitions = host.tool_definitions(actor);
    let legacy = delegation_tool_definitions();
    let mut tools = Vec::new();
    for (definition, canonical_name) in legacy.as_array().into_iter().flatten().zip([
        "vibex_delegate",
        "vibex_get_tasks",
        "vibex_cancel_task",
    ]) {
        if definitions.iter().any(|tool| tool.name == canonical_name) {
            tools.push(definition.clone());
        }
    }
    tools.extend(definitions.into_iter().map(|definition| {
        json!({
            "name": definition.name,
            "description": definition.description,
            "inputSchema": definition.input_schema,
        })
    }));
    json!({ "tools": tools })
}

fn legacy_delegate_arguments(params: &Value) -> VibexResult<Value> {
    let object = params.as_object().ok_or_else(|| {
        VibexError::validation(
            "agent_delegation_params_invalid",
            "delegation parameters must be an object",
        )
    })?;
    for key in [
        "task",
        "title",
        "agentId",
        "providerProfileId",
        "model",
        "reasoningEffort",
        "modeId",
        "idempotencyKey",
    ] {
        if object
            .get(key)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(VibexError::validation(
                "agent_delegation_params_invalid",
                format!("{key} must be a string"),
            ));
        }
    }
    let task = object
        .get("task")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let agent_id = parse_optional_id(object, "agentId", AgentId::parse)?;
    let profile = parse_optional_id(object, "providerProfileId", ProviderProfileId::parse)?;
    Ok(json!({
        "idempotencyKey": optional_string(object, "idempotencyKey")
            .unwrap_or_else(|| format!("request-{}", AgentDelegationId::new())),
        "session": { "kind": "new" },
        "task": {
            "prompt": task,
            "title": optional_string(object, "title"),
            "completionPolicy": "single_turn_legacy",
        },
        "target": {
            "agentId": agent_id,
            "providerProfileId": profile,
            "model": optional_string(object, "model"),
            "reasoningEffort": optional_string(object, "reasoningEffort"),
            "modeId": optional_string(object, "modeId"),
        },
    }))
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
    let diagnostics: Vec<Value> = error
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic.key.as_str(),
                "operationRef"
                    | "groupRef"
                    | "selectionRef"
                    | "phase"
                    | "reason"
                    | "catalogRevision"
                    | "controllerRevision"
                    | "revision"
                    | "maxDepth"
                    | "remainingExecutions"
                    | "expectedExecutionRef"
                    | "currentExecutionRef"
                    | "candidates"
                    | "members"
            )
        })
        .take(16)
        .map(|diagnostic| {
            json!({
                "key": diagnostic.key,
                "value": diagnostic.value.chars().take(4096).collect::<String>(),
            })
        })
        .collect();
    if !diagnostics.is_empty() {
        error_object.insert("diagnostics".to_string(), Value::Array(diagnostics));
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

#[cfg(test)]
mod tests;

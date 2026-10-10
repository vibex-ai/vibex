//! Concurrent stdio requests with one bounded connection per request.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

use super::framing::{encode_json, read_bounded_line, read_stdio_payload, write_frame};
use super::{
    AGENT_DELEGATION_ACTIVATION_ENV, AGENT_DELEGATION_AUTHORITY_ENV,
    AGENT_DELEGATION_MCP_SERVER_ID, BROKER_CALL_TIMEOUT, BROKER_IO_TIMEOUT, MAX_BROKER_LINE_BYTES,
    MAX_MCP_MESSAGE_BYTES,
};

const MAX_IN_FLIGHT_REQUESTS: usize = 16;
const MAX_REQUEST_ID_BYTES: usize = 256;

// Credentials never implement Debug or travel through a tool argument.
#[derive(Clone)]
pub(super) struct SidecarConfig {
    pub endpoint: SocketAddr,
    pub token: String,
    pub parent_session_id: String,
    pub authority: String,
    pub activation_revision: u64,
}

impl SidecarConfig {
    fn from_environment() -> Result<Self, String> {
        fn required(name: &str) -> Result<String, String> {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{name} is missing"))
        }
        let endpoint = required("VIBEX_AGENT_DELEGATION_ENDPOINT")?
            .parse::<SocketAddr>()
            .ok()
            .filter(|address| address.ip().is_loopback())
            .ok_or_else(|| {
                "the delegation broker endpoint must be a loopback address".to_string()
            })?;
        let parent_session_id = required("VIBEX_AGENT_DELEGATION_PARENT_SESSION")?;
        vibex_core::VibexSessionId::parse(parent_session_id.clone())
            .map_err(|_| "the delegation parent session is invalid".to_string())?;
        let activation_revision = required(AGENT_DELEGATION_ACTIVATION_ENV)?
            .parse::<u64>()
            .ok()
            .filter(|revision| *revision > 0)
            .ok_or_else(|| "the delegation activation revision is invalid".to_string())?;
        Ok(Self {
            endpoint,
            token: required("VIBEX_AGENT_DELEGATION_TOKEN")?,
            parent_session_id,
            authority: required(AGENT_DELEGATION_AUTHORITY_ENV)?,
            activation_revision,
        })
    }
}

/// Runs until stdin closes. Long polls, discovery and mutations use independent
/// request futures; cancelling a request drops only its broker connection.
pub fn run_delegation_mcp_stdio() -> Result<(), String> {
    let config = Arc::new(SidecarConfig::from_environment()?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let result = runtime.block_on(serve_stdio(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        config,
    ));
    // Tokio's stdin reader can remain blocked in a platform read after stdout
    // closes. Do not let runtime shutdown keep the sidecar process alive.
    runtime.shutdown_timeout(Duration::from_millis(100));
    result.map_err(|error| error.to_string())
}

enum Input {
    Message(Value),
    InvalidJson,
    ReadFailed(io::Error),
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct PendingRequest {
    id: Value,
    handle: AbortHandle,
}

pub(super) async fn serve_stdio<R, W>(
    mut reader: R,
    mut writer: W,
    config: Arc<SidecarConfig>,
) -> io::Result<()>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    // A dedicated reader keeps partial Content-Length frames intact when a
    // concurrent call finishes. Recreating a read future inside select would
    // discard partially consumed frames.
    let (sender, mut incoming) = mpsc::channel(1);
    let input = tokio::spawn(async move {
        loop {
            let input = match read_stdio_payload(&mut reader).await {
                Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
                    Ok(message) => Input::Message(message),
                    Err(_) => Input::InvalidJson,
                },
                Ok(None) => return,
                Err(error) => {
                    let _ = sender.send(Input::ReadFailed(error)).await;
                    return;
                }
            };
            if sender.send(input).await.is_err() {
                return;
            }
        }
    });
    let _input_guard = AbortOnDrop(input.abort_handle());
    let mut calls = JoinSet::new();
    let mut deliveries = JoinSet::new();
    let mut delivery_waiters: Vec<watch::Receiver<bool>> = Vec::new();
    let mut pending = HashMap::<String, PendingRequest>::new();
    let mut input_closed = false;
    loop {
        if input_closed && calls.is_empty() && deliveries.is_empty() {
            return Ok(());
        }
        tokio::select! {
            input = incoming.recv(), if !input_closed => {
                let message = match input {
                    None => {
                        input_closed = true;
                        continue;
                    }
                    Some(Input::ReadFailed(error)) => return Err(error),
                    Some(Input::InvalidJson) => {
                        let _ = emit_response(&mut writer, McpResponse::plain(rpc_error(
                            Value::Null, -32700, "invalid JSON",
                        ))).await?;
                        continue;
                    }
                    Some(Input::Message(message)) => message,
                };
                if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                    || !message.get("method").is_some_and(Value::is_string)
                {
                    let _ = emit_response(&mut writer, McpResponse::plain(rpc_error(
                        Value::Null, -32600, "invalid JSON-RPC request",
                    ))).await?;
                    continue;
                }
                let Some(id) = message.get("id") else {
                    if message.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
                        && let Some(key) = message.get("params").and_then(|params| params.get("requestId"))
                            .and_then(request_key)
                        && let Some(request) = pending.remove(&key)
                    {
                        request.handle.abort();
                    }
                    continue;
                };
                let Some(key) = request_key(id) else {
                    let _ = emit_response(&mut writer, McpResponse::plain(rpc_error(
                        Value::Null, -32600, "request id must be a bounded string or integer",
                    ))).await?;
                    continue;
                };
                if !pending.contains_key(&key)
                    && matches!(message.get("method").and_then(Value::as_str), Some("initialize" | "ping"))
                {
                    let response = handle_mcp_message(&config, message).await;
                    let _ = emit_response(&mut writer, response).await?;
                    continue;
                }
                if pending.contains_key(&key)
                    || calls.len() + deliveries.len() >= MAX_IN_FLIGHT_REQUESTS
                {
                    let message = if pending.contains_key(&key) {
                        "request id is already in flight"
                    } else {
                        "too many requests are in flight; retry after one completes"
                    };
                    let _ = emit_response(&mut writer, McpResponse::plain(rpc_error(
                        id.clone(), -32000, message,
                    ))).await?;
                    continue;
                }
                let id = id.clone();
                let request_key = key.clone();
                let config = config.clone();
                delivery_waiters.retain(|waiter| !*waiter.borrow());
                let waiters = if message.get("method").and_then(Value::as_str) == Some("tools/call")
                    && message.get("params").and_then(|params| params.get("name"))
                        .and_then(Value::as_str) == Some(vibex_core::VibexUseTool::AckEvents.name())
                {
                    delivery_waiters.clone()
                } else {
                    Vec::new()
                };
                let handle = calls.spawn(async move {
                    // The client may ACK immediately after reading stdout.
                    // Wait only for delivery receipts that preceded that ACK;
                    // discovery, long polls, ping and cancellation stay live.
                    for mut waiter in waiters {
                        while !*waiter.borrow_and_update() {
                            if waiter.changed().await.is_err() {
                                break;
                            }
                        }
                    }
                    (request_key, handle_mcp_message(&config, message).await)
                });
                pending.insert(key, PendingRequest { id, handle });
            }
            response = calls.join_next_with_id(), if !calls.is_empty() => {
                match response {
                    Some(Ok((task_id, (key, response)))) => {
                        if pending.get(&key).is_some_and(|request| request.handle.id() == task_id) {
                            pending.remove(&key);
                            if let Some(delivery) = emit_response(&mut writer, response).await? {
                                let (done, waiter) = watch::channel(false);
                                delivery_waiters.push(waiter);
                                deliveries.spawn(async move {
                                    if !matches!(
                                        tokio::time::timeout(BROKER_IO_TIMEOUT, delivery.confirm()).await,
                                        Ok(Ok(()))
                                    ) {
                                        tracing::debug!(target: "vibex_agent", "event response will remain eligible for redelivery");
                                    }
                                    done.send_replace(true);
                                });
                            }
                        }
                    }
                    Some(Err(error)) => {
                        let key = pending.iter().find(|(_, request)| request.handle.id() == error.id())
                            .map(|(key, _)| key.clone());
                        if let Some(key) = key
                            && let Some(request) = pending.remove(&key)
                            && !error.is_cancelled()
                        {
                            let _ = emit_response(&mut writer, McpResponse::plain(rpc_error(
                                request.id, -32603, "request could not be completed",
                            ))).await?;
                        }
                    }
                    None => {}
                }
            }
            _ = deliveries.join_next(), if !deliveries.is_empty() => {
                delivery_waiters.retain(|waiter| !*waiter.borrow());
            }
        }
    }
}

fn request_key(id: &Value) -> Option<String> {
    match id {
        Value::String(value) if value.len() <= MAX_REQUEST_ID_BYTES => Some(format!("s:{value}")),
        Value::Number(value) if value.is_i64() || value.is_u64() => Some(format!("n:{value}")),
        _ => None,
    }
}

pub(super) struct McpResponse {
    pub message: Value,
    delivery: Option<BrokerDelivery>,
}

impl McpResponse {
    fn plain(message: Value) -> Self {
        Self {
            message,
            delivery: None,
        }
    }
}

async fn handle_mcp_message(config: &SidecarConfig, message: Value) -> McpResponse {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    match message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "initialize" => McpResponse::plain(json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": message.get("params").and_then(|params| params.get("protocolVersion"))
                    .and_then(Value::as_str).unwrap_or("2024-11-05"),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": AGENT_DELEGATION_MCP_SERVER_ID, "version": env!("CARGO_PKG_VERSION") },
            },
        })),
        "ping" => McpResponse::plain(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
        "tools/list" => match call_broker(config, "list_tools", json!({})).await {
            Ok((value, _)) => McpResponse::plain(json!({
                "jsonrpc": "2.0", "id": id, "result": value,
            })),
            Err(error) => McpResponse::plain(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": "tool discovery failed", "data": error },
            })),
        },
        "tools/call" => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_broker(config, name, arguments).await {
                Ok((value, delivery)) => McpResponse {
                    message: tool_response(id, false, value),
                    delivery,
                },
                Err(error) => McpResponse::plain(tool_response(id, true, error)),
            }
        }
        _ => McpResponse::plain(rpc_error(id, -32601, "method not found")),
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_response(id: Value, is_error: bool, value: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id,
        "result": {
            "isError": is_error,
            "content": [{ "type": "text", "text": value.to_string() }],
            "structuredContent": value,
        },
    })
}

pub(super) struct BrokerDelivery {
    connection: BufReader<TcpStream>,
    receipt: String,
}

impl BrokerDelivery {
    async fn confirm(mut self) -> io::Result<()> {
        let bytes = encode_json(&json!({ "deliveryReceipt": self.receipt }), 1023)?;
        write_frame(self.connection.get_mut(), &bytes).await?;
        let response = read_bounded_line(&mut self.connection, 4096)
            .await?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "delivery receipt was not stored",
                )
            })?;
        let response: Value = serde_json::from_slice(&response).map_err(io::Error::other)?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(io::Error::other("delivery receipt was not stored"));
        }
        Ok(())
    }
}

async fn call_broker(
    config: &SidecarConfig,
    method: &str,
    params: Value,
) -> Result<(Value, Option<BrokerDelivery>), Value> {
    let request = json!({
        "token": config.token,
        "parentSessionId": config.parent_session_id,
        "authority": config.authority,
        "activationRevision": config.activation_revision,
        "method": method,
        "params": params,
    });
    let bytes = encode_json(&request, MAX_BROKER_LINE_BYTES - 1).map_err(|_| transport_error())?;
    let response = tokio::time::timeout(BROKER_CALL_TIMEOUT + BROKER_IO_TIMEOUT, async {
        let stream = TcpStream::connect(config.endpoint).await?;
        let mut connection = BufReader::new(stream);
        write_frame(connection.get_mut(), &bytes).await?;
        let response = read_bounded_line(&mut connection, MAX_BROKER_LINE_BYTES)
            .await?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "broker response is missing")
            })?;
        let response: Value = serde_json::from_slice(&response).map_err(io::Error::other)?;
        Ok::<_, io::Error>((connection, response))
    })
    .await
    .map_err(|_| transport_error())?
    .map_err(|_| transport_error())?;
    let (connection, response) = response;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(response
            .get("error")
            .cloned()
            .unwrap_or_else(transport_error));
    }
    let delivery = response
        .get("deliveryReceipt")
        .and_then(Value::as_str)
        .map(|receipt| BrokerDelivery {
            connection,
            receipt: receipt.to_string(),
        });
    Ok((
        response.get("value").cloned().unwrap_or(Value::Null),
        delivery,
    ))
}

fn transport_error() -> Value {
    json!({
        "code": "agent_delegation_transport_failed",
        "message": "the delegation broker request could not complete",
        "retryable": true,
    })
}

pub(super) async fn emit_response(
    writer: &mut (impl AsyncWrite + Unpin),
    mut response: McpResponse,
) -> io::Result<Option<BrokerDelivery>> {
    let bytes = match encode_json(&response.message, MAX_MCP_MESSAGE_BYTES - 1) {
        Ok(bytes) => bytes,
        Err(_) => {
            // The caller receives a bounded error instead of a partial JSON
            // frame. The discarded result must never establish event delivery.
            response.delivery = None;
            encode_json(
                &rpc_error(
                    response.message.get("id").cloned().unwrap_or(Value::Null),
                    -32000,
                    "response too large; request a smaller result page",
                ),
                MAX_MCP_MESSAGE_BYTES - 1,
            )?
        }
    };
    tokio::time::timeout(BROKER_IO_TIMEOUT, write_frame(writer, &bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "MCP response write timed out"))??;
    Ok(response.delivery)
}

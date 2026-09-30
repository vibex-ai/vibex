//! The loopback HTTP MCP endpoint and the stdio sidecar bridge.
//!
//! `POST /mcp` is a Streamable-HTTP MCP endpoint bound to `127.0.0.1` on an
//! ephemeral port, so an Agent that advertises HTTP MCP needs no sidecar
//! process at all. The security properties are the same ones the browser
//! endpoint relies on, and they are all load-bearing:
//!
//! * bound to `127.0.0.1` only, never `0.0.0.0`;
//! * `Host` must be a loopback authority, which defeats DNS rebinding;
//! * `Origin`, when present, must be loopback or absent;
//! * every request must carry `Authorization: Bearer <session token>`;
//! * a body size limit, because any local process can reach the port.
//!
//! The endpoint is a **localhost** surface. It is not the remote transport: a
//! client on another machine reaches computer use through the runtime's own
//! remote channel, and the frame hop for that is a separate contract with its
//! own explicit degradation.

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

use crate::error::ComputerError;
use crate::mcp::{ComputerMcpHandler, ComputerMcpHost};

/// Largest MCP request body accepted.
pub const MAX_MCP_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
struct EndpointState {
    handler: Arc<ComputerMcpHandler>,
    host: Arc<dyn ComputerMcpHost>,
}

/// A running loopback MCP endpoint.
pub struct ComputerMcpEndpoint {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for ComputerMcpEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComputerMcpEndpoint")
            .field("address", &self.address)
            .finish()
    }
}

impl ComputerMcpEndpoint {
    pub async fn start(
        handler: Arc<ComputerMcpHandler>,
        host: Arc<dyn ComputerMcpHost>,
    ) -> Result<Self, ComputerError> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| {
                ComputerError::process(
                    "computer_mcp_bind_failed",
                    "the runtime could not bind the computer MCP endpoint",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        let address = listener.local_addr().map_err(|error| {
            ComputerError::process(
                "computer_mcp_bind_failed",
                "the runtime could not determine the computer MCP endpoint address",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let router = Router::new()
            .route("/mcp", post(handle_post))
            .with_state(EndpointState { handler, host });
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(
                    target: "vibex_computer",
                    error = %error,
                    "the computer MCP endpoint stopped"
                );
            }
        });
        Ok(Self { address, task })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The URL an agent should be given.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.address.port())
    }

    pub fn authorization_header(token: &str) -> String {
        format!("Bearer {token}")
    }

    pub fn stop(self) {
        self.task.abort();
    }
}

async fn handle_post(
    State(state): State<EndpointState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > MAX_MCP_BODY_BYTES {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "computer_mcp_body_too_large");
    }
    if let Err(response) = validate_perimeter(&headers) {
        return response;
    }
    let Some(token) = bearer_token(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "computer_mcp_token_missing");
    };
    let Some(session) = state.host.resolve_token(&token).await else {
        return json_error(StatusCode::UNAUTHORIZED, "computer_mcp_token_invalid");
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "computer_mcp_body_invalid");
    };
    match state.handler.handle_message(&session, message).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Rejects a request whose host header is not a loopback authority.
#[allow(clippy::result_large_err)]
fn validate_perimeter(headers: &HeaderMap) -> Result<(), Response> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase());
    let Some(host) = host else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "computer_mcp_host_missing",
        ));
    };
    let host_name = host
        .rsplit_once(':')
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| host.clone());
    let loopback = matches!(
        host_name.as_str(),
        "127.0.0.1" | "localhost" | "[::1]" | "::1"
    );
    if !loopback {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "computer_mcp_host_not_loopback",
        ));
    }
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        let origin = origin.to_ascii_lowercase();
        let allowed = origin.starts_with("http://127.0.0.1")
            || origin.starts_with("http://localhost")
            || origin.starts_with("http://[::1]");
        if !allowed {
            return Err(json_error(
                StatusCode::FORBIDDEN,
                "computer_mcp_origin_rejected",
            ));
        }
    }
    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .trim();
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn json_error(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32000, "message": code },
        })),
    )
        .into_response()
}

/// Forwards one JSON-RPC message to a runtime MCP endpoint over HTTP.
///
/// This is what the stdio sidecar and the CLI path do for each message. Both
/// hold no state, which matters because the sidecar is spawned and owned by a
/// third-party agent CLI: the runtime cannot supervise it, restart it, or see
/// its stderr.
pub async fn forward_to_endpoint(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    message: &Value,
) -> Result<Option<Value>, ComputerError> {
    let response = client
        .post(endpoint)
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .json(message)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|error| {
            ComputerError::process(
                "computer_mcp_forward_failed",
                "the computer MCP client could not reach the runtime endpoint",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    if response.status() == reqwest::StatusCode::ACCEPTED {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(ComputerError::process(
            "computer_mcp_forward_rejected",
            format!(
                "the runtime computer MCP endpoint answered {}",
                response.status()
            ),
        ));
    }
    let value: Value = response.json().await.map_err(|error| {
        ComputerError::process(
            "computer_mcp_forward_invalid",
            "the runtime computer MCP endpoint returned an unreadable response",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (key, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn loopback_hosts_are_accepted_and_others_rejected() {
        for host in ["127.0.0.1:8080", "localhost:1", "[::1]:9", "127.0.0.1"] {
            assert!(validate_perimeter(&headers(&[("host", host)])).is_ok());
        }
        assert!(validate_perimeter(&headers(&[("host", "evil.test:8080")])).is_err());
        assert!(validate_perimeter(&headers(&[])).is_err());
    }

    #[test]
    fn non_loopback_origins_are_rejected() {
        assert!(
            validate_perimeter(&headers(&[
                ("host", "127.0.0.1:8080"),
                ("origin", "https://evil.test"),
            ]))
            .is_err()
        );
    }

    #[test]
    fn bearer_tokens_are_parsed() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer abc")])),
            Some("abc".to_string())
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer  ")])),
            None
        );
        assert_eq!(bearer_token(&headers(&[])), None);
    }
}

//! The loopback HTTP MCP endpoint and the stdio sidecar bridge.
//!
//! `POST /mcp` is a Streamable-HTTP MCP endpoint bound to `127.0.0.1` on an
//! ephemeral port. It exists so the runtime can hand browser tools to agents
//! that advertise `mcpCapabilities.http` without a sidecar process at all:
//! there is no third-party-spawned process to leak, no framing to get wrong,
//! and no per-message recovery problem.
//!
//! Security properties, all of which are load-bearing:
//!
//! * bound to `127.0.0.1` only, never `0.0.0.0`;
//! * `Host` must be a loopback authority, which defeats DNS rebinding;
//! * `Origin`, when present, must be loopback or absent;
//! * every request must carry `Authorization: Bearer <session token>`;
//! * a body size limit, because the endpoint is reachable by any local process.

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

use crate::error::BrowserError;
use crate::mcp::{BrowserMcpHandler, BrowserMcpHost};

/// Largest MCP request body accepted.
pub const MAX_MCP_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
struct EndpointState {
    handler: Arc<BrowserMcpHandler>,
    host: Arc<dyn BrowserMcpHost>,
}

/// A running loopback MCP endpoint.
pub struct BrowserMcpEndpoint {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for BrowserMcpEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserMcpEndpoint")
            .field("address", &self.address)
            .finish()
    }
}

impl BrowserMcpEndpoint {
    /// Binds the endpoint on a random loopback port and starts serving.
    ///
    /// The host is the runtime: it owns bearer-token verification and human
    /// approval, neither of which belongs in the browser crate.
    pub async fn start(
        handler: Arc<BrowserMcpHandler>,
        host: Arc<dyn BrowserMcpHost>,
    ) -> Result<Self, BrowserError> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| {
                BrowserError::process(
                    "browser_mcp_bind_failed",
                    "the runtime could not bind the browser MCP endpoint",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        let address = listener.local_addr().map_err(|error| {
            BrowserError::process(
                "browser_mcp_bind_failed",
                "the runtime could not determine the browser MCP endpoint address",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let router = Router::new()
            .route("/mcp", post(handle_post))
            .with_state(EndpointState { handler, host });
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(
                    target: "vibex_browser",
                    error = %error,
                    "the browser MCP endpoint stopped"
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

    /// The `Authorization` header value for one session.
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
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "browser_mcp_body_too_large");
    }
    if let Err(response) = validate_perimeter(&headers) {
        return response;
    }
    let Some(token) = bearer_token(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "browser_mcp_token_missing");
    };
    let Some(session) = state.host.resolve_token(&token).await else {
        return json_error(StatusCode::UNAUTHORIZED, "browser_mcp_token_invalid");
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "browser_mcp_body_invalid");
    };
    match state.handler.handle_message(&session, message).await {
        Some(response) => Json(response).into_response(),
        // A notification is acknowledged with `202 Accepted` and no body, as
        // Streamable HTTP requires.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Rejects a request whose host header is not a loopback authority.
///
/// A browser page cannot set `Host`, so requiring a loopback authority blocks
/// DNS-rebinding attacks where a public name resolves to `127.0.0.1`.
///
/// The `Err` variant is a full axum response, which is large by construction;
/// the function runs once per request, so boxing it would add an allocation on
/// the happy path for no benefit.
#[allow(clippy::result_large_err)]
fn validate_perimeter(headers: &HeaderMap) -> Result<(), Response> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase());
    let Some(host) = host else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "browser_mcp_host_missing",
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
            "browser_mcp_host_not_loopback",
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
                "browser_mcp_origin_rejected",
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
/// This is what the stdio sidecar does for each message: read one line, POST
/// it, write the response. It holds no state, which is essential because the
/// sidecar process is spawned and owned by a third-party agent CLI, not by
/// Vibex — the runtime cannot supervise it, restart it, or see its stderr.
pub async fn forward_to_endpoint(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    message: &Value,
) -> Result<Option<Value>, BrowserError> {
    let response = client
        .post(endpoint)
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .json(message)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|error| {
            BrowserError::process(
                "browser_mcp_forward_failed",
                "the browser MCP sidecar could not reach the runtime endpoint",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    if response.status() == reqwest::StatusCode::ACCEPTED {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(BrowserError::process(
            "browser_mcp_forward_rejected",
            format!(
                "the runtime browser MCP endpoint answered {}",
                response.status()
            ),
        ));
    }
    let value: Value = response.json().await.map_err(|error| {
        BrowserError::process(
            "browser_mcp_forward_invalid",
            "the runtime browser MCP endpoint returned an unreadable response",
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
    fn loopback_hosts_are_accepted() {
        for host in ["127.0.0.1:8080", "localhost:1", "[::1]:9", "127.0.0.1"] {
            assert!(
                validate_perimeter(&headers(&[("host", host)])).is_ok(),
                "{host} should be accepted"
            );
        }
    }

    #[test]
    fn non_loopback_hosts_are_rejected() {
        // This is the DNS-rebinding guard: a public name resolving to
        // 127.0.0.1 must not be able to reach the endpoint.
        assert!(validate_perimeter(&headers(&[("host", "evil.test:8080")])).is_err());
        assert!(validate_perimeter(&headers(&[("host", "0.0.0.0:8080")])).is_err());
    }

    #[test]
    fn a_missing_host_header_is_rejected() {
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
        assert!(
            validate_perimeter(&headers(&[
                ("host", "127.0.0.1:8080"),
                ("origin", "http://127.0.0.1:9999"),
            ]))
            .is_ok()
        );
    }

    #[test]
    fn bearer_tokens_are_parsed_and_empty_tokens_rejected() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer abc")])),
            Some("abc".to_string())
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "bearer abc")])),
            Some("abc".to_string())
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer  ")])),
            None
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Basic abc")])),
            None
        );
        assert_eq!(bearer_token(&headers(&[])), None);
    }

    #[test]
    fn authorization_header_shape_matches_the_mcp_client_expectation() {
        assert_eq!(
            BrowserMcpEndpoint::authorization_header("session_abc"),
            "Bearer session_abc"
        );
    }
}

//! The gateway's HTTP surface.
//!
//! Only loopback callers are served, and only with a loopback `Host` header: a
//! browser on the machine can otherwise be pointed at `127.0.0.1` by a hostile
//! page, and DNS rebinding would make that page same-origin with the gateway.
//! The request paths mirror the four vendor APIs exactly, so an Agent's own
//! base-URL setting is all that has to change.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::mpsc;

use crate::error::GatewayError;
use crate::service::ModelGatewayService;
use crate::wire::WireProtocol;

/// State the router carries.
#[derive(Clone)]
pub struct GatewayRouterState {
    pub service: Arc<ModelGatewayService>,
}

/// Builds the gateway router.
pub fn build_router(service: Arc<ModelGatewayService>) -> Router {
    let body_limit = service.max_body_bytes();
    let state = GatewayRouterState { service };
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/responses", post(responses))
        .route("/v1/messages", post(anthropic_messages))
        .route("/messages", post(anthropic_messages))
        // Gemini's model name is part of the path and is followed by an action
        // suffix, so the whole tail is captured and parsed here.
        .route("/v1beta/models/{*call}", post(gemini_call))
        .route("/models", get(list_models))
        .route("/v1/models", get(list_models))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

async fn chat_completions(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    serve(state, WireProtocol::ChatCompletions, headers, body).await
}

async fn responses(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    serve(state, WireProtocol::Responses, headers, body).await
}

async fn anthropic_messages(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    serve(state, WireProtocol::AnthropicMessages, headers, body).await
}

async fn gemini_call(
    State(state): State<GatewayRouterState>,
    headers: HeaderMap,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    // `/v1beta/models/<model>:generateContent` and its streaming twin.
    let path = uri.path();
    let Some(tail) = path.strip_prefix("/v1beta/models/") else {
        return not_found();
    };
    let Some((model, action)) = tail.rsplit_once(':') else {
        return not_found();
    };
    if model.is_empty() || !matches!(action, "generateContent" | "streamGenerateContent") {
        return not_found();
    }
    // The model is part of the path in this protocol, and the Google SDKs never
    // repeat it in the body. Everything downstream — routing, the ledger, the
    // upstream `model` field — reads the request body, so the path model has to
    // be carried into it here or every call resolves as "no model at all".
    let model = percent_decode(model);
    let streaming = action == "streamGenerateContent";
    let body = match body_with_path_selection(&body, &model, streaming) {
        Ok(body) => body,
        Err(error) => {
            return state
                .service
                .error_response(WireProtocol::GeminiGenerateContent, &error);
        }
    };
    serve(state, WireProtocol::GeminiGenerateContent, headers, body).await
}

/// Decodes the model segment of a Gemini path.
///
/// A model id reaches this point already percent-encoded, because a route group
/// id is a model id here and the Agent escapes the slashes inside it. A segment
/// that is not valid percent-encoding is taken as written rather than refused.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(high) = bytes.get(index + 1).and_then(|byte| hex_value(*byte))
            && let Some(low) = bytes.get(index + 2).and_then(|byte| hex_value(*byte))
        {
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Returns the request body with the path's model and streaming intent filled in.
///
/// Gemini is the one protocol that states both in the URL: the model is a path
/// segment, and the action suffix (`:generateContent` versus
/// `:streamGenerateContent`) decides whether the reply is one object or SSE.
/// The client's JSON carries neither. Everything downstream — routing, the
/// ledger, the upstream request — reads the body, so both facts are copied into
/// it here.
///
/// A body that already names a model keeps it: the path is the transport route,
/// and a caller that also set the field said which one it means. Every other
/// field is passed through untouched, so a vendor field the gateway does not
/// model still reaches the upstream.
fn body_with_path_selection(
    body: &[u8],
    model: &str,
    streaming: bool,
) -> Result<bytes::Bytes, GatewayError> {
    let mut value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| GatewayError::MalformedRequest("the body is not valid JSON".to_string()))?;
    let Some(fields) = value.as_object_mut() else {
        return Err(GatewayError::MalformedRequest(
            "a Gemini request body must be a JSON object".to_string(),
        ));
    };
    let named = fields
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|existing| !existing.trim().is_empty());
    if !named {
        fields.insert(
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        );
    }
    // The action suffix is the client's stream flag, and the body has no such
    // field. Writing it here lets the shared pipeline make the same
    // streaming-versus-gathered decision it makes for the other three
    // protocols; without it a `:streamGenerateContent` call was answered with
    // one JSON object and the CLI received a body it could not read as SSE.
    fields.insert("stream".to_string(), serde_json::Value::Bool(streaming));
    serde_json::to_vec(&value)
        .map(bytes::Bytes::from)
        .map_err(|_| GatewayError::Internal("the Gemini body could not be re-encoded".to_string()))
}

/// Refuses a caller that is not loopback, or a browser request.
fn perimeter(headers: &HeaderMap) -> Result<(), GatewayError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let host_name = host
        .rsplit_once(':')
        .map(|(name, _)| name)
        .unwrap_or(host)
        .trim_matches(['[', ']']);
    if !matches!(host_name, "127.0.0.1" | "localhost" | "::1") {
        return Err(GatewayError::NotConfigured(
            "the gateway only accepts loopback requests".to_string(),
        ));
    }
    // A browser sends Origin on every cross-origin request and on same-origin
    // preflighted ones. An Agent CLI never does, so any Origin means a page is
    // driving the connection.
    if headers.contains_key(header::ORIGIN) {
        return Err(GatewayError::NotConfigured(
            "the gateway does not accept browser requests".to_string(),
        ));
    }
    Ok(())
}

/// The wait before one patience retry, capped by the schedule's last step.
///
/// The first four waits double, then the schedule repeats its ceiling. A
/// `None` means the retry budget is spent.
fn patience_delay(retries: u32) -> Option<std::time::Duration> {
    const STEPS_MS: [u64; 5] = [2_000, 4_000, 8_000, 16_000, 30_000];
    let index = retries as usize;
    let millis = STEPS_MS.get(index).copied().unwrap_or(*STEPS_MS.last()?);
    Some(std::time::Duration::from_millis(millis))
}

/// The route group a raw request body names.
///
/// Only reached on a failure path where `prepare` did not produce a call. The
/// group is the breaker's scope, so the count has to land on the right one even
/// when nothing was prepared; a body that does not name a group counts nothing,
/// because there is no group it could belong to.
fn model_group_id(body: &axum::body::Bytes) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let model = value.get("model")?.as_str()?;
    vibex_core::route_group_model_ref(model).map(|parsed| parsed.group_id.to_string())
}

async fn serve(
    state: GatewayRouterState,
    protocol: WireProtocol,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(error) = perimeter(&headers) {
        return state.service.error_response(protocol, &error);
    }
    let service = Arc::clone(&state.service);
    let mut prepared = match service.prepare(protocol, &headers, &body).await {
        Ok(prepared) => prepared,
        Err(error) => {
            // A routing refusal is a failed turn from the caller's point of
            // view: the model it asked for produced no answer. Counting it is
            // what lets a dead upstream reach the threshold even after its only
            // Key has cooled.
            //
            // What is *not* counted is anything that says nothing about the
            // upstream: the gateway's own refusals (a degraded group whose
            // probe slot is taken never consulted anyone), a body the vendor
            // read as the wrong protocol, and a request the gateway could not
            // shape. Counting those is how one wrong protocol guess closed a
            // whole gateway.
            if error.blames_upstream()
                && let Some(group_id) = model_group_id(&body)
            {
                service.note_failure(&group_id);
            }
            // The ledger is the one place a user can see why an Agent's turn
            // failed. A refusal before routing never reached `record`, so
            // without this row the ledger simply has no trace of the request the
            // Agent is complaining about.
            service.record_refusal(protocol, &headers, &body, &error);
            return service.error_response(protocol, &error);
        }
    };
    // Patience is a route's willingness to wait out a transient failure after
    // every member refused and before a byte reached the client. The loop
    // re-prepares, which is what refreshes Key cooldowns and member rotation;
    // retrying a frozen preparation would keep asking Keys that are still
    // cooling and prove nothing.
    let patience_deadline = prepared
        .patience_ms
        .filter(|millis| *millis > 0)
        .map(|millis| std::time::Instant::now() + std::time::Duration::from_millis(millis));
    let mut retries = 0u32;
    let mut attempt_result = service.open_attempt(&prepared).await;
    while let Err(error) = &attempt_result {
        let Some(deadline) = patience_deadline else {
            break;
        };
        if !error.is_recoverable() {
            break;
        }
        let Some(delay) = patience_delay(retries) else {
            break;
        };
        let now = std::time::Instant::now();
        if now + delay >= deadline {
            break;
        }
        tokio::time::sleep(delay).await;
        retries = retries.saturating_add(1);
        match service.prepare(protocol, &headers, &body).await {
            Ok(fresh) => {
                prepared = fresh;
                attempt_result = service.open_attempt(&prepared).await;
            }
            Err(error) => {
                // The re-read can fail for a recoverable reason too: every Key
                // is still cooling, for instance. That is exactly what the
                // next patience step is for, so keep the loop instead of
                // turning a wait into an immediate refusal.
                attempt_result = Err(error);
                continue;
            }
        }
    }
    prepared.retries = retries;
    prepared.route_decision.retries = retries;
    let (attempt, key) = match attempt_result {
        Ok(attempt) => attempt,
        Err(error) => {
            // A request that could not reach any upstream is what degrades a
            // group; a request that never got past its own body is not.
            if error.blames_upstream()
                && let Some(group_id) = prepared.group_id.as_deref()
            {
                service.note_failure(group_id);
            }
            return service.error_response(protocol, &error);
        }
    };

    if !prepared.stream {
        return match crate::service::gather(&service, &prepared, attempt, &key).await {
            Ok(value) => json_response(StatusCode::OK, &value),
            Err(error) => service.error_response(protocol, &error),
        };
    }

    let (sender, receiver) = mpsc::channel::<Result<bytes::Bytes, GatewayError>>(16);
    let service_for_forward = Arc::clone(&service);
    let prepared_for_forward = prepared.clone();
    tokio::spawn(async move {
        crate::service::forward(
            service_for_forward,
            prepared_for_forward,
            attempt,
            &key,
            sender,
        )
        .await;
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Some(Ok(bytes)) => Some((Ok::<_, GatewayError>(bytes), receiver)),
            Some(Err(error)) => Some((Err(error), receiver)),
            None => None,
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    // A reverse proxy buffering the stream would turn a token stream into one
    // long pause. It is only set for the unlikely case of a proxy in between.
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

async fn list_models(State(state): State<GatewayRouterState>, headers: HeaderMap) -> Response {
    if let Err(error) = perimeter(&headers) {
        return state
            .service
            .error_response(WireProtocol::ChatCompletions, &error);
    }
    let snapshot = match state.service.cached_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return state
                .service
                .error_response(WireProtocol::ChatCompletions, &error);
        }
    };
    let catalog = snapshot.model_catalog();
    let data = catalog
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "object": "model",
                "owned_by": entry.provider_name,
            })
        })
        .collect::<Vec<_>>();
    // The Config Center reads this list through the Provider model-fetch path,
    // which only understands the OpenAI `data` envelope.
    let body = serde_json::json!({"object": "list", "data": data});
    json_response(StatusCode::OK, &body)
}

fn json_response(status: StatusCode, value: &serde_json::Value) -> Response {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": {
                "message": "the gateway does not serve this path",
                "type": "not_found",
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn perimeter_accepts_loopback_hosts() {
        for host in ["127.0.0.1:3425", "localhost:3425", "[::1]:3425"] {
            assert!(
                perimeter(&headers(&[("host", host)])).is_ok(),
                "{host} should be accepted"
            );
        }
    }

    #[test]
    fn perimeter_rejects_a_foreign_host_or_a_browser_origin() {
        assert!(perimeter(&headers(&[("host", "evil.example:3425")])).is_err());
        assert!(perimeter(&headers(&[])).is_err());
        assert!(
            perimeter(&headers(&[
                ("host", "127.0.0.1:3425"),
                ("origin", "https://evil.example"),
            ]))
            .is_err()
        );
        // A loopback Origin is still a browser.
        assert!(
            perimeter(&headers(&[
                ("host", "127.0.0.1:3425"),
                ("origin", "http://127.0.0.1:3425"),
            ]))
            .is_err()
        );
    }
}

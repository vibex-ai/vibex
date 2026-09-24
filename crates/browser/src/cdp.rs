//! A Chrome DevTools Protocol client with a pipe transport and a loopback
//! websocket fallback.
//!
//! * **pipe** (`--remote-debugging-pipe`, default on Unix) — Chrome reads
//!   commands from file descriptor 3 and writes responses to file descriptor 4,
//!   with NUL-delimited JSON. No TCP port is opened, so no other process on the
//!   machine can reach the browser's debugging surface.
//! * **websocket** (loopback port mode, used where the pipe transport is not
//!   implemented) — Chrome is started with `--remote-debugging-port=0` and the
//!   real port is read back from the `DevToolsActivePort` file in the profile
//!   directory. It exposes a loopback port any local process could connect to,
//!   so it is a deliberate fallback rather than the default.
//!
//! Every command carries a deadline. The DevTools channel is known never to
//! settle when the target page is wedged, so a command without a timeout is a
//! hang waiting to happen.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

use crate::error::{BrowserError, BrowserResult};

/// Largest single CDP message the client will accept or emit.
pub const MAX_CDP_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// Capacity of the event broadcast channel. Events are advisory; a lagging
/// subscriber is told so instead of stalling the reader.
const CDP_EVENT_CAPACITY: usize = 512;

/// An event pushed by the browser, scoped to a flattened session when present.
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub session_id: Option<String>,
    pub method: String,
    pub params: Value,
}

/// Where a CDP connection's bytes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdpTransportKind {
    Pipe,
    WebSocket,
}

pub(crate) type WebSocketSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;

/// Writes CDP messages to the browser.
enum CdpWriter {
    #[cfg(unix)]
    Pipe(tokio::sync::Mutex<tokio::net::UnixStream>),
    WebSocket(tokio::sync::Mutex<WebSocketSink>),
}

impl CdpWriter {
    async fn write_message(&self, payload: &[u8]) -> BrowserResult<()> {
        match self {
            #[cfg(unix)]
            Self::Pipe(stream) => {
                use tokio::io::AsyncWriteExt;
                let mut guard = stream.lock().await;
                guard.write_all(payload).await?;
                guard.flush().await?;
                Ok(())
            }
            Self::WebSocket(sink) => {
                use futures_util::SinkExt;
                let mut guard = sink.lock().await;
                guard
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        String::from_utf8_lossy(payload).into_owned().into(),
                    ))
                    .await
                    .map_err(|error| {
                        BrowserError::cdp("cdp_websocket_write_failed", error.to_string())
                    })
            }
        }
    }
}

struct CdpInner {
    transport: CdpTransportKind,
    writer: CdpWriter,
    next_id: AtomicI64,
    pending: std::sync::Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    events: broadcast::Sender<CdpEvent>,
    closed: AtomicBool,
}

impl CdpInner {
    fn fail_all_pending(&self, reason: &str) {
        let mut pending = match self.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(reason.to_string()));
        }
    }

    fn route(&self, message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            let sender = {
                let mut pending = match self.pending.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                pending.remove(&id)
            };
            let Some(sender) = sender else {
                return;
            };
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the browser rejected the command")
                    .to_string();
                let _ = sender.send(Err(text));
            } else {
                let _ = sender.send(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
            }
            return;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        let _ = self.events.send(CdpEvent {
            session_id: message
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string),
            method: method.to_string(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
        });
    }

    fn mark_closed(&self, reason: &str) {
        self.closed.store(true, Ordering::SeqCst);
        self.fail_all_pending(reason);
    }
}

/// A live CDP connection.
///
/// Dropping it closes the channel and fails every in-flight command rather than
/// leaving callers parked forever.
pub struct CdpConnection {
    inner: Arc<CdpInner>,
    reader: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for CdpConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CdpConnection")
            .field("transport", &self.inner.transport)
            .field("closed", &self.inner.closed.load(Ordering::SeqCst))
            .finish()
    }
}

impl Drop for CdpConnection {
    fn drop(&mut self) {
        self.inner.mark_closed("cdp_connection_closed");
        self.reader.abort();
    }
}

impl CdpConnection {
    #[cfg(unix)]
    pub fn from_pipe(writer: tokio::net::UnixStream, reader: tokio::net::UnixStream) -> Self {
        let (events, _) = broadcast::channel(CDP_EVENT_CAPACITY);
        let inner = Arc::new(CdpInner {
            transport: CdpTransportKind::Pipe,
            writer: CdpWriter::Pipe(tokio::sync::Mutex::new(writer)),
            next_id: AtomicI64::new(1),
            pending: std::sync::Mutex::new(HashMap::new()),
            events,
            closed: AtomicBool::new(false),
        });
        let pump = Arc::clone(&inner);
        let task = tokio::spawn(async move { pump_null_delimited(pump, reader).await });
        Self {
            inner,
            reader: task,
        }
    }

    pub fn from_websocket(sink: WebSocketSink, read: WebSocketRead) -> Self {
        let (events, _) = broadcast::channel(CDP_EVENT_CAPACITY);
        let inner = Arc::new(CdpInner {
            transport: CdpTransportKind::WebSocket,
            writer: CdpWriter::WebSocket(tokio::sync::Mutex::new(sink)),
            next_id: AtomicI64::new(1),
            pending: std::sync::Mutex::new(HashMap::new()),
            events,
            closed: AtomicBool::new(false),
        });
        let pump = Arc::clone(&inner);
        let task = tokio::spawn(async move { pump_websocket(pump, read).await });
        Self {
            inner,
            reader: task,
        }
    }

    pub fn transport(&self) -> CdpTransportKind {
        self.inner.transport
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    /// Marks the connection unusable without waiting for the reader to notice.
    pub fn mark_closed(&self, reason: &str) {
        self.inner.mark_closed(reason);
    }

    /// Sends a command on the browser-wide session.
    pub async fn command(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> BrowserResult<Value> {
        self.command_on(None, method, params, timeout).await
    }

    /// Sends a command scoped to a flattened CDP session.
    pub async fn command_on(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> BrowserResult<Value> {
        if self.is_closed() {
            return Err(BrowserError::cdp(
                "cdp_connection_closed",
                "the browser debugging channel is closed",
            ));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = match self.inner.pending.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            pending.insert(id, sender);
        }
        let mut message = json!({ "id": id, "method": method });
        if !params.is_null() {
            message["params"] = params;
        }
        if let Some(session_id) = session_id {
            message["sessionId"] = Value::String(session_id.to_string());
        }
        let mut encoded = serde_json::to_vec(&message)
            .map_err(|error| BrowserError::cdp("cdp_message_encode_failed", error.to_string()))?;
        if encoded.len() > MAX_CDP_MESSAGE_BYTES {
            self.forget(id);
            return Err(BrowserError::cdp(
                "cdp_message_too_large",
                "the CDP command exceeds the maximum message size",
            ));
        }
        encoded.push(0);
        if let Err(error) = self.inner.writer.write_message(&encoded).await {
            self.forget(id);
            return Err(error);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(message))) => {
                Err(BrowserError::cdp("cdp_command_failed", message)
                    .with_diagnostic("method", method))
            }
            Ok(Err(_)) => Err(BrowserError::cdp(
                "cdp_connection_closed",
                "the browser debugging channel closed while a command was in flight",
            )
            .with_diagnostic("method", method)),
            Err(_) => {
                self.forget(id);
                Err(BrowserError::timeout(
                    "cdp_command_timeout",
                    format!(
                        "the browser did not answer `{method}` within {}ms",
                        timeout.as_millis()
                    ),
                ))
            }
        }
    }

    fn forget(&self, id: i64) {
        let mut pending = match self.inner.pending.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.remove(&id);
    }
}

/// The read half of a websocket connection.
pub type WebSocketRead = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// Connects to a browser websocket endpoint, returning its split halves.
pub async fn connect_websocket(url: &str) -> BrowserResult<(WebSocketSink, WebSocketRead)> {
    // The endpoint is produced by the runtime from `DevToolsActivePort`, never
    // from page content, so it is already trusted; the parse is still explicit
    // so a malformed value fails loudly.
    let parsed = url::Url::parse(url).map_err(|error| {
        BrowserError::cdp(
            "cdp_endpoint_invalid",
            "the debugging endpoint is not a valid URL",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let (stream, _) = tokio_tungstenite::connect_async(parsed.as_str())
        .await
        .map_err(|error| {
            BrowserError::cdp(
                "cdp_connect_failed",
                "the runtime could not connect to the browser debugging endpoint",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    let (sink, read) = futures_util::StreamExt::split(stream);
    Ok((sink, read))
}

/// Splits a byte stream into NUL-delimited CDP messages.
///
/// Chrome's pipe transport uses `\0` as the frame separator; a message may be
/// split across reads and several messages may arrive in one read.
#[derive(Default)]
pub struct NullDelimitedFramer {
    buffer: Vec<u8>,
}

/// One decoded frame.
pub enum FramedMessage {
    Json(Value),
    /// A frame that was not valid UTF-8 JSON. The connection stays usable; only
    /// the offending frame is dropped. A single bad frame must never take the
    /// whole channel down.
    Malformed(String),
}

impl NullDelimitedFramer {
    pub fn push(&mut self, chunk: &[u8]) -> BrowserResult<Vec<FramedMessage>> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_CDP_MESSAGE_BYTES {
            self.buffer.clear();
            return Err(BrowserError::cdp(
                "cdp_frame_too_large",
                "a CDP frame exceeded the maximum message size",
            ));
        }
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == 0) {
            let frame: Vec<u8> = self.buffer.drain(..=index).collect();
            let frame = &frame[..frame.len() - 1];
            if frame.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Value>(frame) {
                Ok(value) => frames.push(FramedMessage::Json(value)),
                Err(error) => frames.push(FramedMessage::Malformed(error.to_string())),
            }
        }
        Ok(frames)
    }
}

async fn pump_null_delimited<R>(connection: Arc<CdpInner>, mut reader: R)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut framer = NullDelimitedFramer::default();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => match framer.push(&chunk[..read]) {
                Ok(frames) => {
                    for frame in frames {
                        match frame {
                            FramedMessage::Json(value) => connection.route(value),
                            FramedMessage::Malformed(error) => {
                                tracing::debug!(
                                    target: "vibex_browser",
                                    error = %error,
                                    "dropped a malformed CDP frame"
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "vibex_browser", error = %error, "CDP frame stream failed");
                    break;
                }
            },
            Err(error) => {
                tracing::debug!(target: "vibex_browser", error = %error, "CDP reader stopped");
                break;
            }
        }
    }
    connection.mark_closed("cdp_connection_closed");
}

async fn pump_websocket<R>(connection: Arc<CdpInner>, mut stream: R)
where
    R: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures_util::StreamExt;
    while let Some(message) = stream.next().await {
        match message {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                route_text(&connection, text.as_bytes());
            }
            Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes)) => {
                route_text(&connection, &bytes);
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(target: "vibex_browser", error = %error, "CDP websocket stopped");
                break;
            }
        }
    }
    connection.mark_closed("cdp_connection_closed");
}

fn route_text(connection: &Arc<CdpInner>, bytes: &[u8]) {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => connection.route(value),
        Err(error) => tracing::debug!(
            target: "vibex_browser",
            error = %error,
            "dropped a malformed CDP websocket frame"
        ),
    }
}

/// A flattened CDP session bound to one target.
#[derive(Clone)]
pub struct CdpSession {
    connection: Arc<CdpConnection>,
    pub session_id: String,
    pub target_id: String,
}

impl CdpSession {
    pub fn new(connection: Arc<CdpConnection>, session_id: String, target_id: String) -> Self {
        Self {
            connection,
            session_id,
            target_id,
        }
    }

    pub fn connection(&self) -> &CdpConnection {
        &self.connection
    }

    pub async fn command(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> BrowserResult<Value> {
        self.connection
            .command_on(Some(&self.session_id), method, params, timeout)
            .await
    }
}

impl std::fmt::Debug for CdpSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CdpSession")
            .field("session_id", &self.session_id)
            .field("target_id", &self.target_id)
            .finish()
    }
}

/// Parses `DevToolsActivePort`: the first line is the port, the second is the
/// browser websocket path.
pub fn parse_devtools_active_port(contents: &str) -> Option<(u16, Option<String>)> {
    let mut lines = contents.lines();
    let port = lines.next()?.trim().parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }
    let path = lines
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string);
    Some((port, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framer_handles_split_and_coalesced_frames() {
        let mut framer = NullDelimitedFramer::default();
        assert!(framer.push(b"{\"id\":1}").unwrap().is_empty());
        let frames = framer.push(b"\0{\"id\":2}\0").unwrap();
        assert_eq!(frames.len(), 2);
        match &frames[0] {
            FramedMessage::Json(value) => assert_eq!(value["id"], 1),
            FramedMessage::Malformed(error) => panic!("unexpected malformed frame: {error}"),
        }
        match &frames[1] {
            FramedMessage::Json(value) => assert_eq!(value["id"], 2),
            FramedMessage::Malformed(error) => panic!("unexpected malformed frame: {error}"),
        }
    }

    #[test]
    fn framer_keeps_going_after_one_bad_frame() {
        let mut framer = NullDelimitedFramer::default();
        let frames = framer
            .push(b"{not json\0{\"method\":\"Page.loadEventFired\"}\0")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert!(matches!(frames[0], FramedMessage::Malformed(_)));
        assert!(matches!(frames[1], FramedMessage::Json(_)));
    }

    #[test]
    fn framer_ignores_empty_frames() {
        let mut framer = NullDelimitedFramer::default();
        assert!(framer.push(b"\0\0\0").unwrap().is_empty());
    }

    #[test]
    fn devtools_active_port_is_parsed_defensively() {
        assert_eq!(
            parse_devtools_active_port("9222\n/devtools/browser/abc\n"),
            Some((9222, Some("/devtools/browser/abc".to_string())))
        );
        assert_eq!(parse_devtools_active_port("9222\n"), Some((9222, None)));
        assert_eq!(parse_devtools_active_port("0\n/devtools/browser/abc"), None);
        assert_eq!(parse_devtools_active_port(""), None);
        assert_eq!(parse_devtools_active_port("not-a-port"), None);
    }
}

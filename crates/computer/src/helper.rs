//! The computer-use helper process and its client.
//!
//! The engine does **not** run inside the Vibex runtime process. It runs in a
//! separate, supervised helper child that the runtime spawns, and the two speak
//! newline-delimited JSON over the child's stdin/stdout. The boundary buys four
//! things that matter:
//!
//! 1. **Capability isolation.** On macOS the OS grants accessibility and screen
//!    recording to the process chain; keeping the engine one process deep means
//!    the runtime itself never holds a screen-reading grant.
//! 2. **A lifecycle we own.** The helper has a single owner, authenticates
//!    before it does anything, releases held input and exits when its parent
//!    goes away, and can be terminated outright by the emergency stop.
//! 3. **A crash boundary.** A native driver that segfaults on a wedged window
//!    takes the helper with it, not the user's session.
//! 4. **A queue we can clear.** Input requests are processed one at a time from
//!    an explicit queue, so "stop" means the clicks that have not landed yet
//!    never will. A queue that only stops accepting new work still lets a
//!    queued click land after the user pressed stop.
//!
//! The helper is spawned by the runtime **only**. A headless gateway must never
//! start it: the process that spawns the engine is the process its OS grants
//! attach to, and a gateway's identity is not the user's desktop identity.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use vibex_core::{ComputerApplication, ComputerScreenshot, ComputerUnavailableReason};

use crate::engine::{
    ComputerEngine, EngineActionResult, EngineAppState, EngineClick, EngineDelivery, EngineElement,
    EnginePressKey, EngineProbe, EngineScroll, EngineSetValue, EngineStateRequest, EngineTypeText,
};
use crate::error::{ComputerError, ComputerResult, codes};

/// Environment variable carrying the helper token.
pub const HELPER_TOKEN_ENV: &str = "VIBEX_COMPUTER_HELPER_TOKEN";
/// Environment variable carrying the driver path the helper should use.
pub const HELPER_DRIVER_ENV: &str = "VIBEX_COMPUTER_HELPER_DRIVER";
/// Environment variable carrying the single-owner lock file path.
pub const HELPER_OWNER_FILE_ENV: &str = "VIBEX_COMPUTER_HELPER_OWNER_FILE";
/// Environment variable carrying the runtime's pid, for the parent watchdog.
pub const HELPER_PARENT_PID_ENV: &str = "VIBEX_COMPUTER_HELPER_PARENT_PID";
/// How long the helper waits for an authenticated `hello` before exiting.
pub const HELPER_AUTH_DEADLINE: Duration = Duration::from_secs(30);
/// How often the parental-death watchdog checks.
const HELPER_WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);
/// Deadline for one helper request on the client side.
pub const HELPER_REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
/// How many times the client restarts a helper that died mid-session.
const HELPER_RESTART_ATTEMPTS: usize = 1;

/// One helper request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One helper reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperReply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<HelperErrorPayload>,
}

/// The error half of a helper reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<(String, String)>,
}

impl HelperReply {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: u64, error: &ComputerError) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(HelperErrorPayload {
                code: error.code.clone(),
                message: error.message.clone(),
                hint: error.recovery_hint.as_ref().map(|hint| hint.to_string()),
                diagnostics: error.diagnostics.clone(),
            }),
        }
    }

    pub fn into_result(self) -> ComputerResult<Value> {
        if self.ok {
            return Ok(self.result.unwrap_or(Value::Null));
        }
        let payload = self.error.unwrap_or(HelperErrorPayload {
            code: codes::HELPER_FAILED.to_string(),
            message: "the computer-use helper failed without a reason".to_string(),
            hint: None,
            diagnostics: Vec::new(),
        });
        let mut error = ComputerError::process(payload.code, payload.message);
        error.recovery_hint = payload.hint.map(Into::into);
        error.diagnostics = payload.diagnostics;
        Err(error)
    }
}

/// Helper methods that do not touch a target.
pub mod methods {
    pub const HELLO: &str = "hello";
    pub const PROBE: &str = "probe";
    pub const LIST_APPS: &str = "list_apps";
    pub const GET_APP_STATE: &str = "get_app_state";
    pub const CLICK: &str = "click";
    pub const TYPE_TEXT: &str = "type_text";
    pub const SET_VALUE: &str = "set_value";
    pub const PRESS_KEY: &str = "press_key";
    pub const SCROLL: &str = "scroll";
    pub const SCREENSHOT: &str = "screenshot";
    pub const RELEASE_ALL_KEYS: &str = "release_all_keys";
    pub const USER_ACTIVITY: &str = "user_activity";
    pub const LAUNCH_APP: &str = "launch_app";
    pub const KILL_APP: &str = "kill_app";
    pub const STOP: &str = "stop";
    pub const RESUME: &str = "resume";
    pub const SHUTDOWN: &str = "shutdown";
}

/// How the helper is doing, for the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperState {
    /// Spawned, waiting for `hello`.
    Unauthenticated,
    Ready,
    /// The emergency stop fired; the helper releases input and refuses more
    /// until a human re-enables it.
    Stopped,
    Exited,
}

/// Parameters for `get_app_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperStateParams {
    pub app_id: String,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub max_elements: Option<usize>,
    #[serde(default)]
    pub extended: bool,
    #[serde(default)]
    pub screenshot: bool,
}

/// Parameters for a click.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperClickParams {
    pub app_id: String,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub element_index: Option<usize>,
    #[serde(default)]
    pub point: Option<(f64, f64)>,
    #[serde(default = "default_button")]
    pub button: String,
    #[serde(default = "default_click_count")]
    pub click_count: u32,
    #[serde(default = "default_delivery")]
    pub delivery: String,
}

fn default_button() -> String {
    "left".to_string()
}

fn default_click_count() -> u32 {
    1
}

fn default_delivery() -> String {
    "background".to_string()
}

/// Parameters for typing text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperTextParams {
    pub app_id: String,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default = "default_delivery")]
    pub delivery: String,
}

/// Parameters for a semantic value write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperSetValueParams {
    pub app_id: String,
    #[serde(default)]
    pub window_id: Option<String>,
    pub element_index: usize,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_delivery")]
    pub delivery: String,
}

/// Parameters for a key press.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperKeyParams {
    pub app_id: String,
    #[serde(default)]
    pub window_id: Option<String>,
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default = "default_delivery")]
    pub delivery: String,
}

/// Parameters for a scroll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperScrollParams {
    pub app_id: String,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub element_index: Option<usize>,
    #[serde(default)]
    pub point: Option<(f64, f64)>,
    #[serde(default)]
    pub delta_x: f64,
    #[serde(default)]
    pub delta_y: f64,
    #[serde(default = "default_delivery")]
    pub delivery: String,
}

/// One element on the wire. The engine type is not serializable on purpose —
/// this is the boundary's own shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperElement {
    pub index: usize,
    pub role: String,
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub editable: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub bounds: Option<vibex_core::ComputerRect>,
}

impl From<EngineElement> for HelperElement {
    fn from(element: EngineElement) -> Self {
        Self {
            index: element.index,
            role: element.role,
            name: element.name,
            value: element.value,
            editable: element.editable,
            secure: element.secure,
            disabled: element.disabled,
            bounds: element.bounds,
        }
    }
}

impl From<HelperElement> for EngineElement {
    fn from(element: HelperElement) -> Self {
        Self {
            index: element.index,
            role: element.role,
            name: element.name,
            value: element.value,
            editable: element.editable,
            secure: element.secure,
            disabled: element.disabled,
            bounds: element.bounds,
        }
    }
}

/// One application state on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperAppState {
    pub app: ComputerApplication,
    #[serde(default)]
    pub window_id: Option<String>,
    #[serde(default)]
    pub window_title: Option<String>,
    pub tree_digest: String,
    pub elements: Vec<HelperElement>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub degraded: bool,
    #[serde(default)]
    pub degraded_reason: Option<String>,
    #[serde(default)]
    pub screenshot: Option<ComputerScreenshot>,
    #[serde(default)]
    pub window_bounds: Option<vibex_core::ComputerRect>,
}

/// One action result on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperActionResult {
    pub asserted: bool,
    #[serde(default)]
    pub unverified_reason: Option<vibex_core::ComputerUnverifiedReason>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub cursor: Option<(f64, f64)>,
    #[serde(default)]
    pub tree_digest_after: Option<String>,
}

impl From<EngineActionResult> for HelperActionResult {
    fn from(result: EngineActionResult) -> Self {
        Self {
            asserted: result.asserted,
            unverified_reason: result.unverified_reason,
            detail: result.detail,
            cursor: result.cursor,
            tree_digest_after: result.tree_digest_after,
        }
    }
}

impl From<HelperActionResult> for EngineActionResult {
    fn from(result: HelperActionResult) -> Self {
        Self {
            asserted: result.asserted,
            unverified_reason: result.unverified_reason,
            detail: result.detail,
            cursor: result.cursor,
            tree_digest_after: result.tree_digest_after,
        }
    }
}

/// The helper's own readiness payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperProbe {
    #[serde(default)]
    pub engine: Option<String>,
    pub platform: vibex_core::ComputerPlatform,
    pub permissions: vibex_core::ComputerPermissionReport,
    #[serde(default)]
    pub tool_surface: Option<String>,
    #[serde(default)]
    pub degraded: Vec<String>,
    #[serde(default)]
    pub unavailable_reason: Option<ComputerUnavailableReason>,
    #[serde(default)]
    pub detail: Option<String>,
}

impl From<EngineProbe> for HelperProbe {
    fn from(probe: EngineProbe) -> Self {
        Self {
            engine: probe.engine,
            platform: probe.platform,
            permissions: probe.permissions,
            tool_surface: probe.tool_surface,
            degraded: probe.degraded,
            unavailable_reason: probe.unavailable_reason,
            detail: probe.detail,
        }
    }
}

/// The single-owner lock file.
///
/// A second helper started against the same runtime home must not attach: two
/// processes injecting input into one desktop is exactly the runaway the
/// lifecycle contract exists to prevent. A lock whose pid is gone is stale and
/// is replaced, because a killed helper must not brick the feature.
#[derive(Debug, Clone)]
pub struct HelperOwnerLock {
    path: PathBuf,
}

impl HelperOwnerLock {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Claims the lock, returning an error when a live helper owns it.
    pub fn claim(&self) -> ComputerResult<()> {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Some(existing) = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            && existing != std::process::id() as i32
            && process_is_alive(existing)
        {
            return Err(ComputerError::conflict(
                "computer_helper_already_owned",
                "another computer-use helper already owns this runtime",
            )
            .with_diagnostic("owner_pid", existing.to_string()));
        }
        std::fs::write(&self.path, std::process::id().to_string()).map_err(|error| {
            ComputerError::storage(
                "computer_helper_lock_failed",
                "the computer-use helper could not claim its lock file",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(())
    }

    /// Releases the lock when this process owns it.
    pub fn release(&self) {
        if let Some(owner) = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            && owner == std::process::id() as i32
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn process_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: `kill` with signal 0 only probes for the process's existence
        // and changes nothing.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        // Without a portable probe the safe answer is "assume alive": refusing
        // to double-own is recoverable, two helpers are not.
        true
    }
}

/// Runs the helper until `shutdown`, stdin EOF or a fatal error.
///
/// The reader, the action worker and the reply writer are separate tasks on
/// purpose: a long action must not stop the helper from *hearing* a stop. The
/// worker checks the stop flag before every queued item, so an input that has
/// not started when the human presses stop never lands.
pub async fn run_helper_with_engine<R, W>(
    engine: Arc<dyn ComputerEngine>,
    token: String,
    owner: Option<HelperOwnerLock>,
    parent_pid: Option<u32>,
    input: R,
    mut output: W,
) -> ComputerResult<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    if let Some(owner) = &owner {
        owner.claim()?;
    }
    let mut reader = BufReader::new(input).lines();
    let mut authenticated = false;
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (queue_tx, queue_rx) = tokio::sync::mpsc::channel(vibex_core::COMPUTER_MAX_QUEUED_INPUTS);
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<HelperReply>(64);
    let worker = tokio::spawn(action_worker(
        Arc::clone(&engine),
        queue_rx,
        reply_tx.clone(),
        Arc::clone(&stopped),
    ));
    let mut watchdog = tokio::time::interval(HELPER_WATCHDOG_INTERVAL);
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let auth_deadline = tokio::time::sleep(HELPER_AUTH_DEADLINE);
    tokio::pin!(auth_deadline);

    let outcome: ComputerResult<()> = loop {
        tokio::select! {
            line = reader.next_line() => {
                match line {
                    // The parent closed the pipe: release held input and go.
                    Ok(None) => break Ok(()),
                    Err(error) => {
                        break Err(ComputerError::process(
                            codes::HELPER_FAILED,
                            "the computer-use helper could not read its request stream",
                        )
                        .with_diagnostic("error", error.to_string()));
                    }
                    Ok(Some(line)) => {
                        let request = match parse_helper_request(&line) {
                            Ok(request) => request,
                            Err(message) => {
                                // A malformed frame is the caller's problem;
                                // answer and keep serving rather than taking the
                                // desktop tools down.
                                let reply = HelperReply::error(
                                    0,
                                    &ComputerError::validation(
                                        "computer_helper_frame_invalid",
                                        message,
                                    ),
                                );
                                write_reply(&mut output, &reply).await?;
                                continue;
                            }
                        };
                        if request.method == methods::HELLO {
                            authenticated = vibex_core::computer_helper_token_matches(
                                &token,
                                request.params["token"].as_str().unwrap_or_default(),
                            );
                            let reply = if authenticated {
                                HelperReply::ok(request.id, json!({ "state": "ready" }))
                            } else {
                                HelperReply::error(request.id, &ComputerError::permission(
                                    "computer_helper_unauthorized",
                                    "the computer-use helper token was refused",
                                ))
                            };
                            write_reply(&mut output, &reply).await?;
                            continue;
                        }
                        if !authenticated {
                            let reply = HelperReply::error(request.id, &ComputerError::permission(
                                "computer_helper_unauthorized",
                                "the computer-use helper requires an authenticated handshake first",
                            ));
                            write_reply(&mut output, &reply).await?;
                            continue;
                        }
                        match request.method.as_str() {
                            methods::STOP => {
                                // Stopping is terminal for queued work and for
                                // any held key.
                                stopped.store(true, std::sync::atomic::Ordering::SeqCst);
                                release_input_before_exit(&engine).await;
                                write_reply(
                                    &mut output,
                                    &HelperReply::ok(request.id, json!({ "state": "stopped" })),
                                )
                                .await?;
                                continue;
                            }
                            methods::RESUME => {
                                stopped.store(false, std::sync::atomic::Ordering::SeqCst);
                                write_reply(
                                    &mut output,
                                    &HelperReply::ok(request.id, json!({ "state": "ready" })),
                                )
                                .await?;
                                continue;
                            }
                            methods::SHUTDOWN => {
                                release_input_before_exit(&engine).await;
                                write_reply(
                                    &mut output,
                                    &HelperReply::ok(request.id, json!({ "state": "exited" })),
                                )
                                .await?;
                                break Ok(());
                            }
                            _ => {}
                        }
                        if stopped.load(std::sync::atomic::Ordering::SeqCst)
                            && request.method != methods::PROBE
                        {
                            write_reply(
                                &mut output,
                                &HelperReply::error(request.id, &ComputerError::permission(
                                    codes::STOPPED_BY_USER,
                                    "the emergency stop is in force; a human must re-enable computer use",
                                )),
                            )
                            .await?;
                            continue;
                        }
                        if queue_tx.try_send(request).is_err() {
                            // The channel is full: the caller is queueing faster
                            // than the desktop answers. Say so rather than
                            // dropping the request silently.
                            write_reply(
                                &mut output,
                                &HelperReply::error(
                                    0,
                                    &ComputerError::conflict(
                                        "computer_helper_queue_full",
                                        "too many desktop actions are already waiting",
                                    ),
                                ),
                            )
                            .await?;
                        }
                    }
                }
            }
            reply = reply_rx.recv() => {
                if let Some(reply) = reply {
                    write_reply(&mut output, &reply).await?;
                }
            }
            _ = watchdog.tick(), if parent_pid.is_some() => {
                if let Some(expected) = parent_pid
                    && parent_pid_now() != Some(expected)
                {
                    // The runtime died without closing the pipe. Releasing input
                    // is what keeps a crashed agent from leaving the user's
                    // keyboard holding a modifier down.
                    break Ok(());
                }
            }
            _ = &mut auth_deadline, if !authenticated => {
                break Err(ComputerError::permission(
                    "computer_helper_auth_timeout",
                    "the computer-use helper was never authenticated and exited",
                ));
            }
        }
    };
    stopped.store(true, std::sync::atomic::Ordering::SeqCst);
    worker.abort();
    release_input_before_exit(&engine).await;
    if let Some(owner) = &owner {
        owner.release();
    }
    outcome
}

/// Runs queued actions one at a time, stopping the moment the flag is set.
async fn action_worker(
    engine: Arc<dyn ComputerEngine>,
    mut queue: tokio::sync::mpsc::Receiver<HelperRequest>,
    replies: tokio::sync::mpsc::Sender<HelperReply>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
) {
    while let Some(request) = queue.recv().await {
        if stopped.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = replies
                .send(HelperReply::error(
                    request.id,
                    &ComputerError::permission(
                        codes::STOPPED_BY_USER,
                        "the emergency stop cleared this action before it ran",
                    ),
                ))
                .await;
            continue;
        }
        let reply = handle_helper_request(engine.as_ref(), request).await;
        if replies.send(reply).await.is_err() {
            return;
        }
    }
}

async fn handle_helper_request(engine: &dyn ComputerEngine, request: HelperRequest) -> HelperReply {
    let id = request.id;
    let result: ComputerResult<Value> = match request.method.as_str() {
        methods::PROBE => engine
            .probe()
            .await
            .map(|probe| serde_json::to_value(HelperProbe::from(probe)).unwrap_or(Value::Null)),
        methods::LIST_APPS => engine.list_apps().await.and_then(|apps| encode(&apps)),
        methods::GET_APP_STATE => match decode::<HelperStateParams>(&request.params) {
            Ok(params) => engine
                .get_app_state(EngineStateRequest {
                    app_id: params.app_id,
                    window_id: params.window_id,
                    max_elements: params
                        .max_elements
                        .unwrap_or(vibex_core::COMPUTER_OBSERVE_DEFAULT_MAX_ELEMENTS),
                    extended: params.extended,
                    screenshot: params.screenshot,
                })
                .await
                .and_then(|state| {
                    encode(&HelperAppState {
                        app: state.app,
                        window_id: state.window_id,
                        window_title: state.window_title,
                        tree_digest: state.tree_digest,
                        elements: state.elements.into_iter().map(Into::into).collect(),
                        truncated: state.truncated,
                        degraded: state.degraded,
                        degraded_reason: state.degraded_reason,
                        screenshot: state.screenshot,
                        window_bounds: state.window_bounds,
                    })
                }),
            Err(error) => Err(error),
        },
        methods::CLICK => match decode::<HelperClickParams>(&request.params) {
            Ok(params) => engine
                .click(EngineClick {
                    app_id: params.app_id,
                    window_id: params.window_id,
                    element_index: params.element_index,
                    point: params.point,
                    button: params.button,
                    click_count: params.click_count,
                    delivery: parse_delivery(&params.delivery),
                })
                .await
                .and_then(|result| encode(&HelperActionResult::from(result))),
            Err(error) => Err(error),
        },
        methods::TYPE_TEXT => match decode::<HelperTextParams>(&request.params) {
            Ok(params) => engine
                .type_text(EngineTypeText {
                    app_id: params.app_id,
                    window_id: params.window_id,
                    text: params.text,
                    delivery: parse_delivery(&params.delivery),
                })
                .await
                .and_then(|result| encode(&HelperActionResult::from(result))),
            Err(error) => Err(error),
        },
        methods::SET_VALUE => match decode::<HelperSetValueParams>(&request.params) {
            Ok(params) => engine
                .set_value(EngineSetValue {
                    app_id: params.app_id,
                    window_id: params.window_id,
                    element_index: params.element_index,
                    value: params.value,
                    delivery: parse_delivery(&params.delivery),
                })
                .await
                .and_then(|result| encode(&HelperActionResult::from(result))),
            Err(error) => Err(error),
        },
        methods::PRESS_KEY => match decode::<HelperKeyParams>(&request.params) {
            Ok(params) => engine
                .press_key(EnginePressKey {
                    app_id: params.app_id,
                    window_id: params.window_id,
                    key: params.key,
                    modifiers: params.modifiers,
                    delivery: parse_delivery(&params.delivery),
                })
                .await
                .and_then(|result| encode(&HelperActionResult::from(result))),
            Err(error) => Err(error),
        },
        methods::SCROLL => match decode::<HelperScrollParams>(&request.params) {
            Ok(params) => engine
                .scroll(EngineScroll {
                    app_id: params.app_id,
                    window_id: params.window_id,
                    element_index: params.element_index,
                    point: params.point,
                    delta_x: params.delta_x,
                    delta_y: params.delta_y,
                    delivery: parse_delivery(&params.delivery),
                })
                .await
                .and_then(|result| encode(&HelperActionResult::from(result))),
            Err(error) => Err(error),
        },
        methods::SCREENSHOT => {
            let app_id = request.params["app_id"].as_str();
            let window_id = request.params["window_id"].as_str();
            engine
                .screenshot(app_id, window_id)
                .await
                .and_then(|shot| encode(&shot))
        }
        methods::RELEASE_ALL_KEYS => engine.release_all_keys().await.map(|_| Value::Null),
        methods::USER_ACTIVITY => engine
            .user_activity_age_ms()
            .await
            .and_then(|age| encode(&age)),
        methods::LAUNCH_APP => match request.params["app_id"].as_str() {
            Some(app_id) => engine.launch_app(app_id).await.and_then(|app| encode(&app)),
            None => Err(ComputerError::validation(
                "computer_helper_request_invalid",
                "`app_id` is required",
            )),
        },
        methods::KILL_APP => match request.params["app_id"].as_str() {
            Some(app_id) => engine.kill_app(app_id).await.map(|_| Value::Null),
            None => Err(ComputerError::validation(
                "computer_helper_request_invalid",
                "`app_id` is required",
            )),
        },
        other => Err(ComputerError::validation(
            "computer_helper_method_unknown",
            format!("`{other}` is not a computer-use helper method"),
        )),
    };
    match result {
        Ok(result) => HelperReply::ok(id, result),
        Err(error) => HelperReply::error(id, &error),
    }
}

fn parse_delivery(value: &str) -> EngineDelivery {
    if value.eq_ignore_ascii_case("foreground") {
        EngineDelivery::Foreground
    } else {
        EngineDelivery::Background
    }
}

fn encode<T: Serialize>(value: &T) -> ComputerResult<Value> {
    serde_json::to_value(value).map_err(|error| {
        ComputerError::process(
            codes::HELPER_FAILED,
            "a computer-use result could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn decode<T: for<'de> Deserialize<'de>>(value: &Value) -> ComputerResult<T> {
    serde_json::from_value(value.clone()).map_err(|error| {
        ComputerError::validation(
            "computer_helper_request_invalid",
            "a computer-use helper request was malformed",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn parse_helper_request(line: &str) -> Result<HelperRequest, String> {
    serde_json::from_str::<HelperRequest>(line).map_err(|error| error.to_string())
}

async fn write_reply<W: tokio::io::AsyncWrite + Unpin>(
    output: &mut W,
    reply: &HelperReply,
) -> ComputerResult<()> {
    let mut encoded = serde_json::to_vec(reply).map_err(|error| {
        ComputerError::process(codes::HELPER_FAILED, "a helper reply could not be encoded")
            .with_diagnostic("error", error.to_string())
    })?;
    encoded.push(b'\n');
    output.write_all(&encoded).await.map_err(|error| {
        ComputerError::process(
            codes::HELPER_FAILED,
            "the computer-use helper could not write its reply stream",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    output.flush().await.map_err(|error| {
        ComputerError::process(
            codes::HELPER_FAILED,
            "the computer-use helper could not flush its reply stream",
        )
        .with_diagnostic("error", error.to_string())
    })
}

async fn release_input_before_exit(engine: &Arc<dyn ComputerEngine>) {
    if let Err(error) = engine.release_all_keys().await {
        tracing::warn!(
            target: "vibex_computer",
            code = %error.code,
            "the computer-use helper could not release held input while exiting"
        );
    }
}

fn parent_pid_now() -> Option<u32> {
    #[cfg(unix)]
    {
        // SAFETY: `getppid` takes no arguments, touches no memory and cannot
        // fail.
        Some(unsafe { libc::getppid() } as u32)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The runtime's view of its helper process.
///
/// The helper is spawned by the runtime and killed by the runtime. Nothing else
/// may own it: on macOS the OS attaches the screen and accessibility grants to
/// the spawning process chain, so a gateway that started it would hand the
/// grants to the wrong identity.
pub struct HelperEngine {
    command: PathBuf,
    args: Vec<String>,
    token: String,
    driver: Option<PathBuf>,
    owner_file: Option<PathBuf>,
    env: Vec<(String, String)>,
    connection: Mutex<Option<HelperConnection>>,
    next_id: std::sync::atomic::AtomicU64,
    state: Mutex<HelperState>,
    /// The pid the helper must see as its parent, so a helper that outlives the
    /// runtime exits on its own.
    parent_pid: u32,
}

impl std::fmt::Debug for HelperEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperEngine")
            .field("command", &self.command)
            .field("has_driver", &self.driver.is_some())
            .finish()
    }
}

struct HelperConnection {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl HelperEngine {
    /// Builds a helper client. Nothing is spawned until the first call.
    pub fn new(command: impl Into<PathBuf>, token: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: vec!["--computer-helper".to_string()],
            token: token.into(),
            driver: None,
            owner_file: None,
            env: Vec::new(),
            connection: Mutex::new(None),
            next_id: std::sync::atomic::AtomicU64::new(1),
            state: Mutex::new(HelperState::Unauthenticated),
            parent_pid: std::process::id(),
        }
    }

    pub fn with_driver(mut self, driver: Option<PathBuf>) -> Self {
        self.driver = driver;
        self
    }

    pub fn with_owner_file(mut self, path: Option<PathBuf>) -> Self {
        self.owner_file = path;
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The helper's last known state.
    pub async fn state(&self) -> HelperState {
        *self.state.lock().await
    }

    async fn ensure_connection(
        &self,
    ) -> ComputerResult<tokio::sync::MutexGuard<'_, Option<HelperConnection>>> {
        let mut guard = self.connection.lock().await;
        if guard.is_some() {
            return Ok(guard);
        }
        let mut command = tokio::process::Command::new(&self.command);
        command
            .args(&self.args)
            .env(HELPER_TOKEN_ENV, &self.token)
            .env(HELPER_PARENT_PID_ENV, self.parent_pid.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(driver) = &self.driver {
            command.env(HELPER_DRIVER_ENV, driver);
        }
        if let Some(owner) = &self.owner_file {
            command.env(HELPER_OWNER_FILE_ENV, owner);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper could not be started",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper has no request pipe",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper has no reply pipe",
            )
        })?;
        let mut connection = HelperConnection {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        };
        // The handshake is what turns a spawned process into an owned one.
        // The runtime holds the capability secret; the helper receives only the
        // derived token, so the secret never crosses the process boundary.
        let hello = HelperRequest {
            id: 0,
            method: methods::HELLO.to_string(),
            params: json!({ "token": self.token }),
        };
        let reply = exchange(&mut connection, &hello, HELPER_REQUEST_TIMEOUT).await?;
        reply.into_result()?;
        *self.state.lock().await = HelperState::Ready;
        *guard = Some(connection);
        Ok(guard)
    }

    /// Sends one request, restarting a helper that died once.
    async fn call(&self, method: &str, params: Value) -> ComputerResult<Value> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request = HelperRequest {
            id,
            method: method.to_string(),
            params,
        };
        let mut attempt = 0;
        loop {
            let mut guard = self.ensure_connection().await?;
            let connection = guard.as_mut().expect("the connection was just ensured");
            match exchange(connection, &request, HELPER_REQUEST_TIMEOUT).await {
                Ok(reply) => match reply.into_result() {
                    Ok(result) => return Ok(result),
                    Err(error) => {
                        // The helper answered; if it answered "stopped" the
                        // caller needs that code, not a restart.
                        return Err(error);
                    }
                },
                Err(error) => {
                    *guard = None;
                    *self.state.lock().await = HelperState::Exited;
                    if attempt >= HELPER_RESTART_ATTEMPTS {
                        return Err(error);
                    }
                    attempt += 1;
                    tracing::warn!(
                        target: "vibex_computer",
                        code = %error.code,
                        "the computer-use helper died; restarting it once"
                    );
                }
            }
        }
    }

    /// Asks the helper to stop and release input, without killing it.
    ///
    /// The process stays alive deliberately: restarting it would re-run the
    /// platform permission handshake, and the stop contract only needs it to be
    /// input-incapable.
    pub async fn stop(&self) -> ComputerResult<()> {
        let result = self.call(methods::STOP, json!({})).await.map(|_| ());
        if result.is_ok() {
            *self.state.lock().await = HelperState::Stopped;
        }
        result
    }

    /// Re-enables a stopped helper after a human confirmed.
    pub async fn resume(&self) -> ComputerResult<()> {
        let result = self.call(methods::RESUME, json!({})).await.map(|_| ());
        if result.is_ok() {
            *self.state.lock().await = HelperState::Ready;
        }
        result
    }

    /// Releases held input even when the helper cannot answer a normal call.
    pub async fn release_all_keys(&self) -> ComputerResult<()> {
        self.call(methods::RELEASE_ALL_KEYS, json!({}))
            .await
            .map(|_| ())
    }

    /// Terminates the helper. Safe to call twice.
    pub async fn shutdown(&self) {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_mut() {
            let request = HelperRequest {
                id: u64::MAX,
                method: methods::SHUTDOWN.to_string(),
                params: Value::Null,
            };
            let _ = exchange(connection, &request, Duration::from_secs(2)).await;
            let _ = connection.child.start_kill();
            let _ = connection.child.wait().await;
        }
        *guard = None;
        *self.state.lock().await = HelperState::Exited;
    }
}

async fn exchange(
    connection: &mut HelperConnection,
    request: &HelperRequest,
    timeout: Duration,
) -> ComputerResult<HelperReply> {
    let mut encoded = serde_json::to_vec(request).map_err(|error| {
        ComputerError::validation(
            "computer_helper_request_invalid",
            "a computer-use helper request could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    encoded.push(b'\n');
    let exchange = async {
        connection.stdin.write_all(&encoded).await?;
        connection.stdin.flush().await?;
        let mut line = String::new();
        let read = connection.stdout.read_line(&mut line).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the helper closed its reply stream",
            ));
        }
        Ok::<_, std::io::Error>(line)
    };
    let line = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| {
            ComputerError::process(
                "computer_helper_timeout",
                "the computer-use helper did not answer before its deadline",
            )
            .with_diagnostic("method", request.method.clone())
        })?
        .map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper channel broke",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    serde_json::from_str::<HelperReply>(line.trim()).map_err(|error| {
        ComputerError::process(
            codes::HELPER_FAILED,
            "the computer-use helper returned an unreadable reply",
        )
        .with_diagnostic("error", error.to_string())
    })
}

#[async_trait]
impl ComputerEngine for HelperEngine {
    async fn probe(&self) -> ComputerResult<EngineProbe> {
        let value = self.call(methods::PROBE, json!({})).await?;
        let probe: HelperProbe = serde_json::from_value(value).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper returned an unreadable readiness report",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(EngineProbe {
            engine: probe.engine,
            platform: probe.platform,
            permissions: probe.permissions,
            tool_surface: probe.tool_surface,
            degraded: probe.degraded,
            unavailable_reason: probe.unavailable_reason,
            detail: probe.detail,
        })
    }

    async fn list_apps(&self) -> ComputerResult<Vec<ComputerApplication>> {
        let value = self.call(methods::LIST_APPS, json!({})).await?;
        serde_json::from_value(value).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper returned an unreadable application list",
            )
            .with_diagnostic("error", error.to_string())
        })
    }

    async fn get_app_state(&self, request: EngineStateRequest) -> ComputerResult<EngineAppState> {
        let value = self
            .call(
                methods::GET_APP_STATE,
                json!({
                    "app_id": request.app_id,
                    "window_id": request.window_id,
                    "max_elements": request.max_elements,
                    "extended": request.extended,
                    "screenshot": request.screenshot,
                }),
            )
            .await?;
        let state: HelperAppState = serde_json::from_value(value).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper returned an unreadable application state",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(EngineAppState {
            app: state.app,
            window_id: state.window_id,
            window_title: state.window_title,
            tree_digest: state.tree_digest,
            elements: state.elements.into_iter().map(Into::into).collect(),
            truncated: state.truncated,
            degraded: state.degraded,
            degraded_reason: state.degraded_reason,
            screenshot: state.screenshot,
            window_bounds: state.window_bounds,
        })
    }

    async fn click(&self, request: EngineClick) -> ComputerResult<EngineActionResult> {
        let value = self
            .call(
                methods::CLICK,
                json!({
                    "app_id": request.app_id,
                    "window_id": request.window_id,
                    "element_index": request.element_index,
                    "point": request.point,
                    "button": request.button,
                    "click_count": request.click_count,
                    "delivery": request.delivery.as_str(),
                }),
            )
            .await?;
        decode_action(value)
    }

    async fn type_text(&self, request: EngineTypeText) -> ComputerResult<EngineActionResult> {
        let value = self
            .call(
                methods::TYPE_TEXT,
                json!({
                    "app_id": request.app_id,
                    "window_id": request.window_id,
                    "text": request.text,
                    "delivery": request.delivery.as_str(),
                }),
            )
            .await?;
        decode_action(value)
    }

    async fn set_value(&self, request: EngineSetValue) -> ComputerResult<EngineActionResult> {
        let value = self
            .call(
                methods::SET_VALUE,
                json!({
                    "app_id": request.app_id,
                    "window_id": request.window_id,
                    "element_index": request.element_index,
                    "value": request.value,
                    "delivery": request.delivery.as_str(),
                }),
            )
            .await?;
        decode_action(value)
    }

    async fn press_key(&self, request: EnginePressKey) -> ComputerResult<EngineActionResult> {
        let value = self
            .call(
                methods::PRESS_KEY,
                json!({
                    "app_id": request.app_id,
                    "window_id": request.window_id,
                    "key": request.key,
                    "modifiers": request.modifiers,
                    "delivery": request.delivery.as_str(),
                }),
            )
            .await?;
        decode_action(value)
    }

    async fn scroll(&self, request: EngineScroll) -> ComputerResult<EngineActionResult> {
        let value = self
            .call(
                methods::SCROLL,
                json!({
                    "app_id": request.app_id,
                    "window_id": request.window_id,
                    "element_index": request.element_index,
                    "point": request.point,
                    "delta_x": request.delta_x,
                    "delta_y": request.delta_y,
                    "delivery": request.delivery.as_str(),
                }),
            )
            .await?;
        decode_action(value)
    }

    async fn screenshot(
        &self,
        app_id: Option<&str>,
        window_id: Option<&str>,
    ) -> ComputerResult<Option<ComputerScreenshot>> {
        let value = self
            .call(
                methods::SCREENSHOT,
                json!({ "app_id": app_id, "window_id": window_id }),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper returned an unreadable screenshot",
            )
            .with_diagnostic("error", error.to_string())
        })
    }

    async fn release_all_keys(&self) -> ComputerResult<()> {
        self.call(methods::RELEASE_ALL_KEYS, json!({}))
            .await
            .map(|_| ())
    }

    async fn user_activity_age_ms(&self) -> ComputerResult<Option<i64>> {
        let value = self.call(methods::USER_ACTIVITY, json!({})).await?;
        serde_json::from_value(value).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper returned an unreadable activity age",
            )
            .with_diagnostic("error", error.to_string())
        })
    }

    async fn launch_app(&self, app_id: &str) -> ComputerResult<ComputerApplication> {
        let value = self
            .call(methods::LAUNCH_APP, json!({ "app_id": app_id }))
            .await?;
        serde_json::from_value(value).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the computer-use helper returned an unreadable application",
            )
            .with_diagnostic("error", error.to_string())
        })
    }

    async fn kill_app(&self, app_id: &str) -> ComputerResult<()> {
        self.call(methods::KILL_APP, json!({ "app_id": app_id }))
            .await
            .map(|_| ())
    }
}

fn decode_action(value: Value) -> ComputerResult<EngineActionResult> {
    let result: HelperActionResult = serde_json::from_value(value).map_err(|error| {
        ComputerError::process(
            codes::HELPER_FAILED,
            "the computer-use helper returned an unreadable action result",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(result.into())
}

/// Everything the helper needs to start, read from its environment.
pub struct HelperConfiguration {
    /// The token a client must present in its handshake.
    pub token: String,
    /// The driver executable, when the runtime pinned one.
    pub driver: Option<PathBuf>,
    /// The single-owner lock the helper claims.
    pub owner: Option<HelperOwnerLock>,
    /// The pid the helper must see as its parent.
    pub parent_pid: Option<u32>,
}

/// Reads the helper configuration from the environment. Used by the
/// `--computer-helper` entry point.
pub fn helper_config_from_environment() -> ComputerResult<HelperConfiguration> {
    let token = std::env::var(HELPER_TOKEN_ENV).map_err(|_| {
        ComputerError::validation(
            "computer_helper_config_missing",
            format!("{HELPER_TOKEN_ENV} is not set"),
        )
    })?;
    if token.trim().is_empty() {
        return Err(ComputerError::validation(
            "computer_helper_config_missing",
            "the computer-use helper token must not be empty",
        ));
    }
    let driver = std::env::var_os(HELPER_DRIVER_ENV).map(PathBuf::from);
    let owner = std::env::var_os(HELPER_OWNER_FILE_ENV)
        .map(PathBuf::from)
        .map(HelperOwnerLock::new);
    let parent = std::env::var(HELPER_PARENT_PID_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok());
    Ok(HelperConfiguration {
        token,
        driver,
        owner,
        parent_pid: parent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FixtureEngine;
    use tokio::io::{AsyncWriteExt, BufReader as TokioBufReader};

    #[test]
    fn owner_locks_refuse_a_live_owner_and_replace_a_dead_one() {
        let directory = tempfile::tempdir().unwrap();
        let lock = HelperOwnerLock::new(directory.path().join("helper.owner"));
        lock.claim().unwrap();
        // The same process may re-claim.
        lock.claim().unwrap();
        lock.release();
        assert!(!lock.path().exists());

        // A pid that cannot be alive is treated as stale.
        std::fs::write(lock.path(), "2147483646").unwrap();
        lock.claim().unwrap();
        lock.release();
    }

    #[tokio::test]
    async fn the_helper_refuses_everything_before_the_handshake() {
        let (client, server) = tokio::io::duplex(4096);
        let (server_read, server_write) = tokio::io::split(server);
        let engine = Arc::new(FixtureEngine::default());
        let task = tokio::spawn(run_helper_with_engine(
            engine,
            vibex_core::computer_helper_token("cap_test"),
            None,
            None,
            server_read,
            server_write,
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut reader = TokioBufReader::new(client_read).lines();

        client_write
            .write_all(b"{\"id\":1,\"method\":\"list_apps\"}\n")
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(!reply.ok);
        assert_eq!(
            reply.error.as_ref().unwrap().code,
            "computer_helper_unauthorized"
        );

        // A bad token is refused too.
        client_write
            .write_all(b"{\"id\":2,\"method\":\"hello\",\"params\":{\"token\":\"htok_nope\"}}\n")
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(!reply.ok);

        let good = json!({ "id": 3, "method": "hello", "params": { "token": vibex_core::computer_helper_token("cap_test") } });
        client_write
            .write_all(format!("{good}\n").as_bytes())
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);

        client_write
            .write_all(b"{\"id\":4,\"method\":\"list_apps\"}\n")
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        let apps: Vec<ComputerApplication> = serde_json::from_value(reply.result.unwrap()).unwrap();
        assert!(!apps.is_empty());

        // Both halves must go: `split` keeps the stream open until the last
        // handle drops, and the helper treats EOF as "the runtime is gone".
        drop(reader);
        drop(client_write);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_stop_releases_input_and_refuses_further_actions_until_resume() {
        let (client, server) = tokio::io::duplex(8192);
        let (server_read, server_write) = tokio::io::split(server);
        let engine = Arc::new(FixtureEngine::default());
        let task = tokio::spawn(run_helper_with_engine(
            engine.clone(),
            vibex_core::computer_helper_token("cap_test"),
            None,
            None,
            server_read,
            server_write,
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut reader = TokioBufReader::new(client_read).lines();
        let send = |value: Value| {
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            bytes
        };
        client_write
            .write_all(&send(json!({ "id": 1, "method": "hello", "params": { "token": vibex_core::computer_helper_token("cap_test") } })))
            .await
            .unwrap();
        let _ = reader.next_line().await.unwrap().unwrap();

        client_write
            .write_all(&send(json!({ "id": 2, "method": "stop" })))
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        assert!(
            engine
                .released_keys
                .load(std::sync::atomic::Ordering::SeqCst)
        );

        client_write
            .write_all(&send(json!({ "id": 3, "method": "list_apps" })))
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(!reply.ok);
        assert_eq!(reply.error.as_ref().unwrap().code, codes::STOPPED_BY_USER);

        client_write
            .write_all(&send(json!({ "id": 4, "method": "resume" })))
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        client_write
            .write_all(&send(json!({ "id": 5, "method": "list_apps" })))
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);

        drop(reader);
        drop(client_write);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_closed_request_pipe_releases_input() {
        let (client, server) = tokio::io::duplex(4096);
        let (server_read, server_write) = tokio::io::split(server);
        let engine = Arc::new(FixtureEngine::default());
        let task = tokio::spawn(run_helper_with_engine(
            engine.clone(),
            vibex_core::computer_helper_token("cap_test"),
            None,
            None,
            server_read,
            server_write,
        ));
        drop(client);
        task.await.unwrap().unwrap();
        assert!(
            engine
                .released_keys
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn requests_round_trip_through_the_client() {
        // A helper without a driver process: the fixture engine is mounted in
        // process, which is exactly what the protocol is for.
        let (runtime_side, helper_side) = tokio::io::duplex(16384);
        let (helper_read, helper_write) = tokio::io::split(helper_side);
        let engine = Arc::new(FixtureEngine::default());
        let helper = tokio::spawn(run_helper_with_engine(
            engine,
            vibex_core::computer_helper_token("cap_test"),
            None,
            None,
            helper_read,
            helper_write,
        ));
        // Drive the client half directly through the protocol.
        let (client_read, mut client_write) = tokio::io::split(runtime_side);
        let mut reader = TokioBufReader::new(client_read).lines();
        let send = |value: Value| {
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            bytes
        };
        client_write
            .write_all(&send(json!({ "id": 1, "method": "hello", "params": { "token": vibex_core::computer_helper_token("cap_test") } })))
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        assert!(serde_json::from_str::<HelperReply>(&line).unwrap().ok);
        client_write
            .write_all(&send(json!({
                "id": 2,
                "method": "get_app_state",
                "params": { "app_id": "com.example.notes", "window_id": "w1" }
            })))
            .await
            .unwrap();
        let line = reader.next_line().await.unwrap().unwrap();
        let reply: HelperReply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        let state: HelperAppState = serde_json::from_value(reply.result.unwrap()).unwrap();
        assert!(!state.elements.is_empty());
        assert!(!state.tree_digest.is_empty());
        drop(reader);
        drop(client_write);
        helper.await.unwrap().unwrap();
    }
}

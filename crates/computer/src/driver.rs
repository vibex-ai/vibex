//! The third-party desktop-driver adapter.
//!
//! The shipped engine is an external, versioned driver binary that already
//! solved the hard part of this feature — driving a window without stealing
//! focus — on all three desktop platforms. This module is the *only* place
//! that knows that binary's command line and result shape, and it is the seam
//! an engine upgrade has to pass through:
//!
//! * The mapping goes one way. Vibex's tool vocabulary is defined in
//!   [`crate::engine`]; nothing upstream is passed through to an Agent.
//! * The binary is located by an explicit path or a `PATH` lookup and invoked
//!   without a shell, so a model-supplied string can never become an argument.
//! * Every invocation carries a deadline. A driver call that never settles is
//!   a known failure mode, and an Agent tool call must not hang forever on it.
//! * Parsing is tolerant about key spelling and strict about absence: a reply
//!   that does not contain what was asked for is an error, never an empty
//!   success.
//!
//! The field mapping is versioned against a pinned driver. Phase 0 of the
//! rollout records `list-tools` output next to the pinned commit and this
//! module's tests pin the mapping, so an upgrade that changes the surface fails
//! a test instead of silently degrading every action.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use vibex_core::{
    ComputerApplication, ComputerPermissionReport, ComputerPermissionState, ComputerPlatform,
    ComputerRect, ComputerScreenshot, ComputerUnavailableReason, ComputerUnverifiedReason,
    ComputerWindow,
};

use crate::engine::{
    ComputerEngine, EngineActionResult, EngineAppState, EngineClick, EngineElement, EnginePressKey,
    EngineProbe, EngineScroll, EngineSetValue, EngineStateRequest, EngineTypeText,
};
use crate::error::{ComputerError, ComputerResult, codes};

/// Environment variable overriding the driver path.
pub const DRIVER_PATH_ENV: &str = "VIBEX_COMPUTER_DRIVER";
/// Default command name looked up on `PATH`.
pub const DRIVER_COMMAND: &str = "cua-driver";
/// Per-invocation deadline.
pub const DRIVER_TIMEOUT_MS: u64 = 20_000;
/// The same deadline as a `Duration`, for the calls that use `tokio::time`.
const DRIVER_PROBE_TIMEOUT_DURATION: Duration = Duration::from_millis(DRIVER_PROBE_TIMEOUT_MS);
/// Deadline for the readiness probe, which must never hold startup.
pub const DRIVER_PROBE_TIMEOUT_MS: u64 = 5_000;

/// A driver binary that speaks the CLI protocol.
///
/// The driver is a daemon plus a client: `list-tools`, `doctor` and `status`
/// answer on their own, while a tool call goes through `call <tool> <json>` on a
/// socket. `serve` is what starts that daemon, and this client starts it as its
/// own child when needed — the process that spawns the daemon is the process
/// the operating system attaches its screen and accessibility grants to, so a
/// daemon started here carries the helper's identity rather than a shell's.
#[derive(Debug)]
pub struct CuaDriverCli {
    executable: PathBuf,
    /// Environment the driver needs. The Wayland opt-in is the one that
    /// matters: without it the driver refuses background input rather than
    /// pretending to work, and the runtime passes the user's choice through
    /// instead of deciding for them.
    extra_env: Vec<(String, String)>,
    /// The socket the daemon listens on. `None` uses the driver's own default,
    /// which is what lets it share a daemon the user already started.
    socket: Option<PathBuf>,
    /// The daemon this client started, kept so it dies with the helper.
    daemon: tokio::sync::Mutex<Option<tokio::process::Child>>,
}

impl Clone for CuaDriverCli {
    fn clone(&self) -> Self {
        Self {
            executable: self.executable.clone(),
            extra_env: self.extra_env.clone(),
            socket: self.socket.clone(),
            // A clone is a second handle on the same daemon, not a second
            // daemon: the child stays with the original.
            daemon: tokio::sync::Mutex::new(None),
        }
    }
}

impl CuaDriverCli {
    /// Uses an explicit driver path.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            extra_env: Vec::new(),
            socket: None,
            daemon: tokio::sync::Mutex::new(None),
        }
    }

    /// Uses a specific daemon socket instead of the driver's default.
    pub fn with_socket(mut self, socket: impl Into<PathBuf>) -> Self {
        self.socket = Some(socket.into());
        self
    }

    fn socket_args(&self) -> Vec<String> {
        match &self.socket {
            Some(socket) => vec!["--socket".to_string(), socket.to_string_lossy().to_string()],
            None => Vec::new(),
        }
    }

    /// Adds one environment variable for every driver invocation.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// Locates the driver: explicit override first, then `PATH`, then the
    /// install locations the project documents.
    ///
    /// The lookup is read-only. The runtime never installs the driver on a
    /// user's behalf; a missing engine is reported with the documented install
    /// step instead.
    pub fn discover() -> Option<Self> {
        let mut driver = if let Some(path) = std::env::var_os(DRIVER_PATH_ENV) {
            let path = PathBuf::from(path);
            if is_executable(&path) {
                Self::new(path)
            } else {
                return None;
            }
        } else if let Some(path) = lookup_on_path(DRIVER_COMMAND) {
            Self::new(path)
        } else {
            Self::new(discover_well_known()?)
        };
        if let Some(enabled) = std::env::var_os(ComputerPlatform::wayland_opt_in_variable()) {
            driver = driver.with_env(
                ComputerPlatform::wayland_opt_in_variable(),
                enabled.to_string_lossy().to_string(),
            );
        }
        Some(driver)
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Runs one driver tool and returns its parsed reply.
    ///
    /// A call that finds no daemon starts one and retries once: the daemon is
    /// part of the engine, and asking the user to start it by hand would make
    /// the settings page's Install button a half-measure.
    async fn invoke(&self, tool: &str, params: &Value) -> ComputerResult<Value> {
        match self
            .invoke_with_timeout(tool, params, DRIVER_TIMEOUT_MS)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if error.code == "computer_driver_daemon_missing" => {
                self.ensure_daemon().await?;
                self.invoke_with_timeout(tool, params, DRIVER_TIMEOUT_MS)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    /// Starts the engine's daemon if nothing is listening yet.
    async fn ensure_daemon(&self) -> ComputerResult<()> {
        let mut guard = self.daemon.lock().await;
        if self.daemon_is_running().await {
            return Ok(());
        }
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("serve")
            .args(self.socket_args())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // The daemon holds the desktop session; it must not outlive the
            // helper that owns it.
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let child = command.spawn().map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the desktop driver daemon could not be started",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        *guard = Some(child);
        // Readiness is the daemon answering, not the process existing.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if self.daemon_is_running().await {
                return Ok(());
            }
        }
        Err(ComputerError::process(
            codes::HELPER_FAILED,
            "the desktop driver daemon did not become ready",
        ))
    }

    /// Reads the driver's own permission report (macOS).
    ///
    /// Read-only: the probe never prompts, because a permission dialog belongs
    /// to a click in the settings rather than to a startup path.
    async fn run_permissions_status(&self) -> Option<Value> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("permissions")
            .arg("status")
            .arg("--json")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let output = tokio::time::timeout(DRIVER_PROBE_TIMEOUT_DURATION, command.output())
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::from_slice(&output.stdout).ok()
    }

    /// Whether the driver reports a live daemon.
    pub async fn daemon_is_running(&self) -> bool {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("status")
            .args(self.socket_args())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let Ok(output) = tokio::time::timeout(Duration::from_secs(5), command.output()).await
        else {
            return false;
        };
        let Ok(output) = output else { return false };
        let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        text.contains("daemon is running")
    }

    async fn invoke_with_timeout(
        &self,
        tool: &str,
        params: &Value,
        timeout_ms: u64,
    ) -> ComputerResult<Value> {
        let encoded = serde_json::to_string(params).map_err(|error| {
            ComputerError::validation(
                "computer_driver_request_invalid",
                "the driver request could not be encoded",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let mut command = Command::new(&self.executable);
        command
            .arg("call")
            .arg(tool)
            .arg(&encoded)
            .args(self.socket_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let output = tokio::time::timeout(Duration::from_millis(timeout_ms), command.output())
            .await
            .map_err(|_| {
                ComputerError::process(
                    "computer_driver_timeout",
                    "the desktop driver did not answer before its deadline",
                )
                .with_diagnostic("tool", tool)
            })?
            .map_err(|error| {
                ComputerError::process(
                codes::ENGINE_MISSING,
                "the desktop driver could not be started",
            )
            .with_diagnostic("error", error.to_string())
            .with_recovery_hint(
                "Install the driver the runtime documents, or point VIBEX_COMPUTER_DRIVER at it.",
            )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let bounded = bounded_text(&stderr, 400);
            // A daemon that is not up is not a rejected action: the engine is
            // installed and simply has not been started yet, and the caller
            // starts it and retries.
            if bounded
                .to_ascii_lowercase()
                .contains("daemon is not running")
            {
                return Err(ComputerError::capability(
                    "computer_driver_daemon_missing",
                    "the desktop driver is installed but its daemon is not running",
                )
                .with_diagnostic("stderr", bounded));
            }
            // The driver reports an unsupported background delivery as a
            // distinct failure; mapping it here is what lets the approval flow
            // offer a foreground escalation instead of a generic error.
            if bounded
                .to_ascii_lowercase()
                .contains("background_unavailable")
                || bounded
                    .to_ascii_lowercase()
                    .contains("background unavailable")
            {
                return Err(ComputerError::capability(
                    codes::BACKGROUND_UNAVAILABLE,
                    "the driver could not deliver this action in the background",
                )
                .with_diagnostic("stderr", bounded));
            }
            return Err(ComputerError::process(
                codes::HELPER_FAILED,
                format!("the desktop driver rejected `{tool}`"),
            )
            .with_diagnostic("stderr", bounded));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_driver_reply(&stdout).ok_or_else(|| {
            ComputerError::process(
                codes::HELPER_FAILED,
                format!("the desktop driver returned an unreadable reply for `{tool}`"),
            )
            .with_diagnostic("stdout", bounded_text(&stdout, 400))
        })
    }

    /// Reads the driver's own tool list, for the pinned surface fingerprint.
    ///
    /// `list-tools` is a subcommand that answers on its own, so this doubles as
    /// the cheapest proof that the binary really is the engine: it needs no
    /// daemon and touches no desktop.
    pub async fn tool_surface(&self) -> ComputerResult<String> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("list-tools")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let output = tokio::time::timeout(DRIVER_PROBE_TIMEOUT_DURATION, command.output())
            .await
            .map_err(|_| {
                ComputerError::process(
                    codes::HELPER_FAILED,
                    "the desktop driver did not answer before its deadline",
                )
            })?
            .map_err(|error| {
                ComputerError::process(
                    codes::ENGINE_MISSING,
                    "the desktop driver could not be started",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        if !output.status.success() {
            return Err(ComputerError::process(
                codes::HELPER_FAILED,
                "the desktop driver refused to list its tools",
            )
            .with_diagnostic(
                "stderr",
                bounded_text(&String::from_utf8_lossy(&output.stderr), 400),
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let names = collect_tool_names_from_listing(&stdout);
        if names.is_empty() {
            return Err(ComputerError::process(
                codes::HELPER_FAILED,
                "the desktop driver reported no tools",
            ));
        }
        Ok(names.join(","))
    }
}

#[async_trait]
impl ComputerEngine for CuaDriverCli {
    async fn probe(&self) -> ComputerResult<EngineProbe> {
        // `doctor` answers without a daemon and reports the whole readiness
        // picture: the binary, the display server, the X connection and the
        // accessibility bus. That is the same question this probe exists to
        // answer, so it is asked once, in the driver's own vocabulary.
        let doctor = self.run_doctor().await.ok();
        let daemon_running = self.daemon_is_running().await;
        let platform = detect_platform();
        // macOS gates screen capture and accessibility through TCC, and the
        // driver is the only process whose answer carries the granted identity.
        // Elsewhere the three are not gated per application, and the readiness
        // that matters is in the doctor probes above.
        let permissions = if platform == ComputerPlatform::Macos {
            match self.run_permissions_status().await {
                Some(reply) => parse_permissions(&reply, platform),
                None => ComputerPermissionReport::unsupported(),
            }
        } else {
            ComputerPermissionReport::unsupported()
        };
        let mut degraded = Vec::new();
        let mut unavailable_reason = None;
        if let Some(doctor) = &doctor {
            degraded.extend(doctor.degraded.iter().cloned());
            if let Some(reason) = doctor.unavailable_reason {
                unavailable_reason = Some(reason);
            }
        }
        if !daemon_running {
            // A driver whose daemon is not up is still a working installation:
            // the first tool call starts it. Saying so beats leaving the reader
            // to guess why nothing has happened yet.
            degraded.push(
                "the driver daemon is not running yet; it starts with the first action".to_string(),
            );
        }
        if platform == ComputerPlatform::LinuxWayland
            && std::env::var_os(ComputerPlatform::wayland_opt_in_variable()).is_none()
        {
            degraded.push(
                "Wayland: background input is disabled until \
                 CUA_DRIVER_RS_ENABLE_WAYLAND=1 is set"
                    .to_string(),
            );
        }
        if unavailable_reason.is_none() && !permissions.structured_usable() {
            unavailable_reason = permissions.blocking_reason();
        }
        let tool_surface = self.tool_surface().await.ok();
        let engine = doctor
            .as_ref()
            .and_then(|doctor| doctor.version.clone())
            .or_else(|| Some(DRIVER_COMMAND.to_string()));
        Ok(EngineProbe {
            engine,
            platform,
            permissions,
            tool_surface,
            degraded,
            unavailable_reason,
            detail: None,
        })
    }

    async fn list_apps(&self) -> ComputerResult<Vec<ComputerApplication>> {
        let reply = self.invoke("list_apps", &json!({})).await?;
        Ok(parse_applications(&reply))
    }

    async fn get_app_state(&self, request: EngineStateRequest) -> ComputerResult<EngineAppState> {
        let tool = if request.extended {
            "get_accessibility_tree"
        } else {
            "get_window_state"
        };
        let reply = self
            .invoke(
                tool,
                &json!({
                    "app": request.app_id,
                    "window_id": request.window_id,
                    "include_screenshot": request.screenshot,
                    "max_elements": request.max_elements,
                }),
            )
            .await?;
        parse_app_state(&reply, &request)
    }

    async fn click(&self, request: EngineClick) -> ComputerResult<EngineActionResult> {
        let mut params = json!({
            "app": request.app_id,
            "window_id": request.window_id,
            "delivery_mode": request.delivery.as_str(),
        });
        if let Some(index) = request.element_index {
            params["element_index"] = json!(index);
        }
        if let Some((x, y)) = request.point {
            params["x"] = json!(x);
            params["y"] = json!(y);
        }
        if request.button == "right" {
            let reply = self.invoke("right_click", &params).await?;
            return Ok(parse_action_result(&reply));
        }
        if request.click_count >= 2 {
            let reply = self.invoke("double_click", &params).await?;
            return Ok(parse_action_result(&reply));
        }
        let reply = self.invoke("click", &params).await?;
        Ok(parse_action_result(&reply))
    }

    async fn type_text(&self, request: EngineTypeText) -> ComputerResult<EngineActionResult> {
        let reply = self
            .invoke(
                "type_text",
                &json!({
                    "app": request.app_id,
                    "window_id": request.window_id,
                    "text": request.text,
                    "delivery_mode": request.delivery.as_str(),
                }),
            )
            .await?;
        Ok(parse_action_result(&reply))
    }

    async fn set_value(&self, request: EngineSetValue) -> ComputerResult<EngineActionResult> {
        let reply = self
            .invoke(
                "set_value",
                &json!({
                    "app": request.app_id,
                    "window_id": request.window_id,
                    "element_index": request.element_index,
                    "value": request.value,
                    "delivery_mode": request.delivery.as_str(),
                }),
            )
            .await?;
        Ok(parse_action_result(&reply))
    }

    async fn press_key(&self, request: EnginePressKey) -> ComputerResult<EngineActionResult> {
        let tool = if request.modifiers.is_empty() {
            "press_key"
        } else {
            "hotkey"
        };
        let reply = self
            .invoke(
                tool,
                &json!({
                    "app": request.app_id,
                    "window_id": request.window_id,
                    "key": request.key,
                    "modifiers": request.modifiers,
                    "delivery_mode": request.delivery.as_str(),
                }),
            )
            .await?;
        Ok(parse_action_result(&reply))
    }

    async fn scroll(&self, request: EngineScroll) -> ComputerResult<EngineActionResult> {
        let mut params = json!({
            "app": request.app_id,
            "window_id": request.window_id,
            "delta_x": request.delta_x,
            "delta_y": request.delta_y,
            "delivery_mode": request.delivery.as_str(),
        });
        if let Some(index) = request.element_index {
            params["element_index"] = json!(index);
        }
        if let Some((x, y)) = request.point {
            params["x"] = json!(x);
            params["y"] = json!(y);
        }
        let reply = self.invoke("scroll", &params).await?;
        Ok(parse_action_result(&reply))
    }

    async fn screenshot(
        &self,
        app_id: Option<&str>,
        window_id: Option<&str>,
    ) -> ComputerResult<Option<ComputerScreenshot>> {
        let tool = if app_id.is_some() {
            "get_window_state"
        } else {
            "get_desktop_state"
        };
        let reply = self
            .invoke(
                tool,
                &json!({
                    "app": app_id,
                    "window_id": window_id,
                    "include_screenshot": true,
                    "screenshot_only": true,
                }),
            )
            .await?;
        Ok(parse_screenshot(&reply))
    }

    async fn release_all_keys(&self) -> ComputerResult<()> {
        // The driver exposes the primitive as `set_config`; treating a refusal
        // as an error is deliberate — a stop that could not release a held key
        // must be visible, not assumed to have worked.
        self.invoke("set_config", &json!({ "release_all_input": true }))
            .await
            .map(|_| ())
    }

    async fn user_activity_age_ms(&self) -> ComputerResult<Option<i64>> {
        let reply = self
            .invoke_with_timeout("get_cursor_position", &json!({}), DRIVER_PROBE_TIMEOUT_MS)
            .await?;
        Ok(integer_at(
            &reply,
            &["idle_ms", "user_idle_ms", "activity_age_ms"],
        ))
    }

    async fn launch_app(&self, app_id: &str) -> ComputerResult<ComputerApplication> {
        let reply = self.invoke("launch_app", &json!({ "app": app_id })).await?;
        let mut apps = parse_applications(&reply);
        apps.pop().ok_or_else(|| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the desktop driver did not report the application it started",
            )
        })
    }

    async fn kill_app(&self, app_id: &str) -> ComputerResult<()> {
        self.invoke("kill_app", &json!({ "app": app_id }))
            .await
            .map(|_| ())
    }
}

/// What the driver's own readiness report said.
#[derive(Debug, Clone, Default)]
struct DriverDoctor {
    version: Option<String>,
    degraded: Vec<String>,
    unavailable_reason: Option<ComputerUnavailableReason>,
}

impl CuaDriverCli {
    /// Runs the driver's own doctor and reads it into this crate's vocabulary.
    ///
    /// Read-only, needs no daemon, and it is the only place that knows what
    /// this platform's readiness actually depends on.
    async fn run_doctor(&self) -> ComputerResult<DriverDoctor> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("doctor")
            .arg("--json")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let output = tokio::time::timeout(DRIVER_PROBE_TIMEOUT_DURATION, command.output())
            .await
            .map_err(|_| {
                ComputerError::process(
                    codes::HELPER_FAILED,
                    "the desktop driver did not answer before its deadline",
                )
            })?
            .map_err(|error| {
                ComputerError::process(
                    codes::ENGINE_MISSING,
                    "the desktop driver could not be started",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        if !output.status.success() {
            return Err(ComputerError::process(
                codes::HELPER_FAILED,
                "the desktop driver reported that it is not healthy",
            )
            .with_diagnostic(
                "stderr",
                bounded_text(&String::from_utf8_lossy(&output.stderr), 400),
            ));
        }
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            ComputerError::process(
                codes::HELPER_FAILED,
                "the desktop driver returned an unreadable readiness report",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(parse_doctor(&value))
    }
}

/// Reads the doctor payload.
///
/// The probe labels are the driver's own; the mapping to this crate's
/// `ComputerUnavailableReason` is what the settings page shows, so a failing
/// probe becomes a named reason instead of a log line.
fn parse_doctor(value: &Value) -> DriverDoctor {
    let mut doctor = DriverDoctor::default();
    let Some(probes) = value.get("probes").and_then(Value::as_array) else {
        return doctor;
    };
    for probe in probes {
        let label = string_at(probe, &["label"]).unwrap_or_default();
        let status = string_at(probe, &["status"]).unwrap_or_default();
        let message = string_at(probe, &["message"]).unwrap_or_default();
        let detail = string_at(probe, &["detail"]).unwrap_or_default();
        match label.as_str() {
            "binary" => doctor.version = Some(message.clone()),
            "display server" => {
                if !status.eq_ignore_ascii_case("ok") {
                    doctor.unavailable_reason = Some(ComputerUnavailableReason::NoDesktopSession);
                    doctor.degraded.push(format!("display server: {message}"));
                }
            }
            "AT-SPI" => {
                if !status.eq_ignore_ascii_case("ok") {
                    doctor.unavailable_reason =
                        Some(ComputerUnavailableReason::AccessibilityBridgeMissing);
                    doctor
                        .degraded
                        .push(format!("accessibility bus: {message}"));
                }
            }
            _ => {
                if !status.eq_ignore_ascii_case("ok") {
                    // A warning is a degradation the user should see; a failure
                    // is a capability gap. Both are worth keeping, neither is
                    // turned into "unsupported" on its own.
                    let text = if detail.is_empty() { message } else { detail };
                    doctor.degraded.push(format!("{label}: {text}"));
                }
            }
        }
    }
    doctor
}

/// Reads the tool names out of `list-tools`.
///
/// The command prints one `name: description` line per tool, so the parser
/// accepts that shape and a JSON array, because a driver that grows a `--json`
/// flag should not break the pin.
fn collect_tool_names_from_listing(stdout: &str) -> Vec<String> {
    let trimmed = stdout.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Some(value) = parse_driver_reply(trimmed)
    {
        let names = collect_tool_names(&value);
        if !names.is_empty() {
            return names;
        }
    }
    let mut names: Vec<String> = trimmed
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Parses a driver reply, accepting both a bare document and the `{"result":…}`
/// envelope the driver uses for some tools.
fn parse_driver_reply(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    Some(match value.get("result") {
        Some(inner) if value.get("error").is_none() => inner.clone(),
        _ => value,
    })
}

fn collect_tool_names(reply: &Value) -> Vec<String> {
    let mut names = Vec::new();
    let candidates = reply
        .get("tools")
        .and_then(Value::as_array)
        .or_else(|| reply.get("result").and_then(Value::as_array));
    if let Some(items) = candidates {
        for item in items {
            let name = item
                .as_str()
                .map(str::to_string)
                .or_else(|| string_at(item, &["name", "tool"]));
            if let Some(name) = name {
                names.push(name);
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Reads the first present key from a JSON object.
fn value_at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let object = value.as_object()?;
    for key in keys {
        if let Some(found) = object.get(*key)
            && !found.is_null()
        {
            return Some(found);
        }
    }
    None
}

fn string_at(value: &Value, keys: &[&str]) -> Option<String> {
    value_at(value, keys).and_then(|found| {
        found
            .as_str()
            .map(str::to_string)
            .or_else(|| found.as_i64().map(|number| number.to_string()))
    })
}

fn bool_at(value: &Value, keys: &[&str]) -> Option<bool> {
    value_at(value, keys).and_then(Value::as_bool)
}

fn integer_at(value: &Value, keys: &[&str]) -> Option<i64> {
    value_at(value, keys).and_then(|found| {
        found
            .as_i64()
            .or_else(|| found.as_f64().map(|number| number as i64))
    })
}

fn number_at(value: &Value, keys: &[&str]) -> Option<f64> {
    value_at(value, keys).and_then(|found| {
        found
            .as_f64()
            .or_else(|| found.as_i64().map(|number| number as f64))
    })
}

fn array_at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    value_at(value, keys).and_then(Value::as_array)
}

fn rect_at(value: &Value, keys: &[&str]) -> Option<ComputerRect> {
    let node = value_at(value, keys)?;
    let x = number_at(node, &["x", "left"]).unwrap_or(0.0);
    let y = number_at(node, &["y", "top"]).unwrap_or(0.0);
    let width = number_at(node, &["width", "w"]).unwrap_or(0.0);
    let height = number_at(node, &["height", "h"]).unwrap_or(0.0);
    if width <= 0.0 && height <= 0.0 {
        return None;
    }
    Some(ComputerRect {
        x,
        y,
        width,
        height,
    })
}

fn parse_applications(reply: &Value) -> Vec<ComputerApplication> {
    let Some(items) = array_at(reply, &["apps", "applications", "items", "windows"]) else {
        // A single-application reply is accepted too: `launch_app` and
        // `get_window_state` answer with one document rather than a list.
        if reply.get("app").is_some() || reply.get("app_id").is_some() {
            return parse_applications(&json!({ "apps": [reply] }));
        }
        if reply.get("id").is_some() || reply.get("name").is_some() {
            return parse_applications(&json!({ "apps": [reply] }));
        }
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let app_id = string_at(
                item,
                &["app_id", "id", "bundle_id", "aumid", "desktop_file"],
            )
            .or_else(|| string_at(item, &["name", "display_name"]))?;
            let display_name = string_at(item, &["display_name", "name", "title", "label"])
                .unwrap_or_else(|| app_id.clone());
            let windows: Vec<ComputerWindow> = array_at(item, &["windows"])
                .map(|windows| windows.iter().filter_map(parse_window).collect())
                .unwrap_or_default();
            Some(ComputerApplication {
                app_id,
                display_name,
                executable_path: string_at(
                    item,
                    &["executable_path", "executable", "path", "app_path"],
                ),
                bundle_id: string_at(item, &["bundle_id", "bundle"]),
                running: bool_at(item, &["running", "is_running"]).unwrap_or(!windows.is_empty()),
                pid: integer_at(item, &["pid", "process_id"]).map(|pid| pid as i32),
                windows,
            })
        })
        .collect()
}

fn parse_window(item: &Value) -> Option<ComputerWindow> {
    let window_id = string_at(item, &["window_id", "id", "handle"])
        .or_else(|| integer_at(item, &["window_id"]).map(|id| id.to_string()))?;
    Some(ComputerWindow {
        window_id,
        title: string_at(item, &["title", "name"]).unwrap_or_default(),
        bounds: rect_at(item, &["bounds", "frame", "rect"]),
        focused: bool_at(item, &["focused", "is_focused", "key"]).unwrap_or(false),
        frontmost: bool_at(item, &["frontmost", "is_frontmost", "active"]).unwrap_or(false),
    })
}

fn parse_elements(reply: &Value) -> Vec<EngineElement> {
    let items = array_at(reply, &["elements", "nodes", "tree", "accessibility_tree"])
        .cloned()
        .unwrap_or_default();
    let mut elements = Vec::with_capacity(items.len());
    for (position, item) in items.iter().enumerate() {
        let index = integer_at(
            item,
            &["index", "node_idx", "element_index", "elementIndex"],
        )
        .map(|index| index.max(0) as usize)
        .unwrap_or(position);
        let role =
            string_at(item, &["role", "ax_role", "type"]).unwrap_or_else(|| "element".to_string());
        let name = string_at(item, &["name", "title", "label", "description"]).unwrap_or_default();
        let secure = bool_at(item, &["secure", "is_secure"]).unwrap_or(false)
            || vibex_core::is_secure_field_role(&role);
        let editable =
            bool_at(item, &["editable", "is_editable", "settable"]).unwrap_or_else(|| {
                matches!(
                    role.to_ascii_lowercase().as_str(),
                    "axtextfield" | "axtextarea" | "textfield" | "textbox" | "edit" | "combobox"
                )
            });
        elements.push(EngineElement {
            index,
            role,
            name,
            value: if secure {
                // A credential value is never read, not even to redact it.
                None
            } else {
                string_at(item, &["value", "ax_value", "text"])
            },
            editable,
            secure,
            disabled: bool_at(item, &["disabled", "is_disabled", "enabled"])
                .map(|enabled| !enabled)
                .unwrap_or(false),
            bounds: rect_at(item, &["bounds", "frame", "rect", "position"]),
        });
    }
    elements
}

fn parse_app_state(reply: &Value, request: &EngineStateRequest) -> ComputerResult<EngineAppState> {
    let app = {
        let mut apps = parse_applications(reply);
        apps.pop().unwrap_or_else(|| ComputerApplication {
            app_id: request.app_id.clone(),
            display_name: request.app_id.clone(),
            executable_path: None,
            bundle_id: None,
            running: true,
            pid: integer_at(reply, &["pid", "process_id"]).map(|pid| pid as i32),
            windows: Vec::new(),
        })
    };
    let elements = parse_elements(reply);
    let degraded = bool_at(reply, &["degraded", "is_degraded"]).unwrap_or(false);
    let degraded_reason = string_at(reply, &["degraded_reason", "degradedReason"]);
    if elements.is_empty() && !degraded {
        // An empty list without a degradation flag is a driver that answered
        // nothing useful. Reporting it as an empty window would tell the model
        // there is nothing to click.
        return Err(ComputerError::capability(
            codes::A11Y_MISSING,
            "the desktop driver returned no accessibility elements and no degradation reason",
        )
        .with_diagnostic("app", request.app_id.clone()));
    }
    let tree_digest = string_at(reply, &["tree_digest", "digest", "sha1", "hash"])
        .unwrap_or_else(|| digest_elements(&elements));
    let window = array_at(reply, &["windows"]).and_then(|windows| windows.first().cloned());
    let window_id = string_at(reply, &["window_id", "id"]).or_else(|| {
        window
            .as_ref()
            .and_then(|window| string_at(window, &["window_id", "id"]))
    });
    let window_title = string_at(reply, &["window_title", "title"]).or_else(|| {
        window
            .as_ref()
            .and_then(|window| string_at(window, &["title", "name"]))
    });
    let window_bounds = rect_at(reply, &["window_bounds", "bounds", "frame"]).or_else(|| {
        window
            .as_ref()
            .and_then(|window| rect_at(window, &["bounds", "frame", "rect"]))
    });
    Ok(EngineAppState {
        app,
        window_id,
        window_title,
        tree_digest,
        elements,
        truncated: bool_at(reply, &["truncated", "is_truncated"]).unwrap_or(false),
        degraded,
        degraded_reason,
        screenshot: parse_screenshot(reply),
        window_bounds,
    })
}

fn parse_action_result(reply: &Value) -> EngineActionResult {
    let asserted = bool_at(reply, &["verified", "asserted", "confirmed"]).unwrap_or(false);
    if asserted {
        return EngineActionResult {
            asserted: true,
            unverified_reason: None,
            detail: string_at(reply, &["detail", "message"]),
            cursor: cursor_at(reply),
            tree_digest_after: string_at(reply, &["tree_digest", "digest"]),
        };
    }
    let reason = string_at(reply, &["unverified_reason", "unverified", "reason"]).map(|reason| {
        let reason = reason.to_ascii_lowercase();
        if reason.contains("accessibility") {
            ComputerUnverifiedReason::AccessibilityActionUnasserted
        } else if reason.contains("clipboard") {
            ComputerUnverifiedReason::ClipboardPaste
        } else if reason.contains("foreground") {
            ComputerUnverifiedReason::ForegroundEscalation
        } else if reason.contains("metadata") {
            ComputerUnverifiedReason::MissingMetadata
        } else {
            ComputerUnverifiedReason::SyntheticInput
        }
    });
    EngineActionResult {
        asserted: false,
        // No metadata at all is `MissingMetadata`, never success.
        unverified_reason: Some(reason.unwrap_or(ComputerUnverifiedReason::MissingMetadata)),
        detail: string_at(reply, &["detail", "message"]),
        cursor: cursor_at(reply),
        tree_digest_after: string_at(reply, &["tree_digest", "digest"]),
    }
}

fn cursor_at(reply: &Value) -> Option<(f64, f64)> {
    let cursor = value_at(reply, &["cursor", "cursor_position", "pointer"])?;
    Some((number_at(cursor, &["x"])?, number_at(cursor, &["y"])?))
}

fn parse_screenshot(reply: &Value) -> Option<ComputerScreenshot> {
    let node = value_at(reply, &["screenshot", "image", "frame"])?;
    let base64 = node
        .as_str()
        .map(str::to_string)
        .or_else(|| string_at(node, &["data", "base64", "bytes"]))?;
    if base64.is_empty() {
        return None;
    }
    let mime_type = string_at(node, &["mime_type", "mimeType", "format"])
        .map(|mime| {
            if mime.contains('/') {
                mime
            } else {
                format!("image/{mime}")
            }
        })
        .unwrap_or_else(|| "image/png".to_string());
    Some(ComputerScreenshot {
        mime_type,
        base64,
        width: integer_at(node, &["width"]).unwrap_or(0).max(0) as u32,
        height: integer_at(node, &["height"]).unwrap_or(0).max(0) as u32,
        scale: number_at(node, &["scale", "vision_scale"]).unwrap_or(1.0),
    })
}

fn parse_permissions(reply: &Value, platform: ComputerPlatform) -> ComputerPermissionReport {
    let state = |keys: &[&str]| -> ComputerPermissionState {
        match value_at(reply, keys) {
            Some(Value::Bool(true)) => ComputerPermissionState::Granted,
            Some(Value::Bool(false)) => ComputerPermissionState::Denied,
            Some(Value::String(text)) => match text.to_ascii_lowercase().as_str() {
                "granted" | "allowed" | "authorized" | "ok" => ComputerPermissionState::Granted,
                "denied" | "refused" | "blocked" => ComputerPermissionState::Denied,
                "not_determined" | "notdetermined" | "prompt" => {
                    ComputerPermissionState::NotDetermined
                }
                "restart_required" | "restart" => ComputerPermissionState::RestartRequired,
                _ => ComputerPermissionState::Unknown,
            },
            _ => ComputerPermissionState::Unknown,
        }
    };
    let mut report = ComputerPermissionReport {
        accessibility: state(&["accessibility", "ax", "accessibility_granted"]),
        screen_recording: state(&["screen_recording", "screen_capture", "screenRecording"]),
        input_injection: state(&["input_injection", "input", "event_posting"]),
        restart_required_after_grant: bool_at(reply, &["restart_required", "needs_restart"])
            .unwrap_or(false),
    };
    if platform == ComputerPlatform::LinuxX11 || platform == ComputerPlatform::LinuxWayland {
        // Linux has no per-application grant ceremony; the checks that matter
        // are the desktop session and the accessibility bus, which the doctor
        // reports separately.
        report = ComputerPermissionReport {
            accessibility: if report.accessibility == ComputerPermissionState::Unknown {
                ComputerPermissionState::NotRequired
            } else {
                report.accessibility
            },
            screen_recording: ComputerPermissionState::NotRequired,
            input_injection: ComputerPermissionState::NotRequired,
            restart_required_after_grant: false,
        };
    }
    if platform == ComputerPlatform::Windows {
        report = ComputerPermissionReport {
            restart_required_after_grant: false,
            ..report
        };
    }
    report
}

/// The platform this process is running on, from the environment the desktop
/// session actually exports.
pub fn detect_platform() -> ComputerPlatform {
    if cfg!(target_os = "macos") {
        return ComputerPlatform::Macos;
    }
    if cfg!(target_os = "windows") {
        return ComputerPlatform::Windows;
    }
    if cfg!(target_os = "linux") {
        let session = std::env::var("XDG_SESSION_TYPE")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let wayland_display = std::env::var_os("WAYLAND_DISPLAY").is_some();
        if session == "wayland" || wayland_display {
            return ComputerPlatform::LinuxWayland;
        }
        if session == "x11" || std::env::var_os("DISPLAY").is_some() {
            return ComputerPlatform::LinuxX11;
        }
    }
    ComputerPlatform::Unknown
}

/// A stable digest for an element list, used when the driver does not supply
/// one.
fn digest_elements(elements: &[EngineElement]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for element in elements {
        hasher.update(element.index.to_le_bytes());
        hasher.update(element.role.as_bytes());
        hasher.update([0u8]);
        hasher.update(element.name.as_bytes());
        hasher.update([0u8]);
        if let Some(value) = &element.value {
            hasher.update(value.as_bytes());
        }
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn lookup_on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = directory.join(format!("{command}.exe"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// The install locations the driver's own installer uses on each platform.
fn discover_well_known() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/bin").join(DRIVER_COMMAND));
        candidates.push(home.join(".cua").join("bin").join(DRIVER_COMMAND));
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join("Applications")
                    .join("CuaDriver.app")
                    .join("Contents")
                    .join("MacOS")
                    .join(DRIVER_COMMAND),
            );
        }
        candidates
            .push(PathBuf::from("/Applications/CuaDriver.app/Contents/MacOS").join(DRIVER_COMMAND));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local)
                    .join("CuaDriver")
                    .join(format!("{DRIVER_COMMAND}.exe")),
            );
        }
    }
    candidates.into_iter().find(|path| is_executable(path))
}

fn bounded_text(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut bounded: String = trimmed.chars().take(limit).collect();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_read_through_the_result_envelope() {
        assert_eq!(
            parse_driver_reply("{\"result\":{\"apps\":[]}}").unwrap(),
            json!({ "apps": [] })
        );
        assert_eq!(
            parse_driver_reply("{\"apps\":[]}").unwrap(),
            json!({ "apps": [] })
        );
        assert!(parse_driver_reply("   ").is_none());
        assert!(parse_driver_reply("not json").is_none());
    }

    #[test]
    fn tool_names_are_collected_from_both_shapes() {
        assert_eq!(
            collect_tool_names(&json!({ "tools": [{ "name": "click" }, "list_apps"] })),
            vec!["click".to_string(), "list_apps".to_string()]
        );
        assert_eq!(
            collect_tool_names(&json!({ "tools": ["b", "a", "a"] })),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(collect_tool_names(&json!({})).is_empty());
    }

    #[test]
    fn the_tool_listing_parses_both_shapes() {
        let listing = "bring_to_front: Persistently activate a window\n\
                       list_apps: List Linux apps\n\
                       list_apps: duplicate\n";
        assert_eq!(
            collect_tool_names_from_listing(listing),
            vec!["bring_to_front".to_string(), "list_apps".to_string()]
        );
        let json_listing = r#"{"tools":[{"name":"click"},"list_apps"]}"#;
        assert_eq!(
            collect_tool_names_from_listing(json_listing),
            vec!["click".to_string(), "list_apps".to_string()]
        );
        assert!(collect_tool_names_from_listing("").is_empty());
        // The usage banner is not a tool list.
        assert!(collect_tool_names_from_listing("cua-driver 0.31.0 — cross-platform").is_empty());
    }

    #[test]
    fn the_doctor_payload_becomes_named_reasons() {
        let healthy = parse_doctor(&json!({
            "ok": true,
            "probes": [
                { "label": "binary", "status": "ok", "message": "cua-driver 0.31.0 (x86_64-linux)" },
                { "label": "display server", "status": "ok", "message": "Wayland+XWayland" },
                { "label": "AT-SPI", "status": "ok", "message": "org.a11y.Bus reachable" },
                { "label": "X11 connection", "status": "warn", "message": "no top-level windows" }
            ]
        }));
        assert_eq!(
            healthy.version.as_deref(),
            Some("cua-driver 0.31.0 (x86_64-linux)")
        );
        assert!(healthy.unavailable_reason.is_none());
        assert_eq!(healthy.degraded.len(), 1);
        assert!(healthy.degraded[0].contains("X11 connection"));

        let no_display = parse_doctor(&json!({
            "ok": false,
            "probes": [{ "label": "display server", "status": "fail", "message": "no display" }]
        }));
        assert_eq!(
            no_display.unavailable_reason,
            Some(ComputerUnavailableReason::NoDesktopSession)
        );

        let no_bus = parse_doctor(&json!({
            "ok": true,
            "probes": [{ "label": "AT-SPI", "status": "fail", "message": "not reachable" }]
        }));
        assert_eq!(
            no_bus.unavailable_reason,
            Some(ComputerUnavailableReason::AccessibilityBridgeMissing)
        );
    }

    #[test]
    fn applications_parse_a_list_and_a_single_document() {
        let list = parse_applications(&json!({
            "apps": [
                { "app_id": "com.apple.mail", "display_name": "Mail", "running": true,
                  "windows": [{ "window_id": "w1", "title": "Inbox", "frontmost": true }] },
                { "bundle_id": "com.apple.safari", "name": "Safari" }
            ]
        }));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].windows.len(), 1);
        assert!(list[0].windows[0].frontmost);
        assert!(list[0].running);
        assert_eq!(list[1].app_id, "com.apple.safari");

        let single = parse_applications(&json!({ "app_id": "notes", "name": "Notes" }));
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].display_name, "Notes");
    }

    #[test]
    fn secure_elements_never_carry_a_value() {
        let elements = parse_elements(&json!({
            "elements": [
                { "index": 0, "role": "AXSecureTextField", "name": "Password", "value": "hunter2" },
                { "index": 1, "role": "AXTextField", "name": "User", "value": "ada" }
            ]
        }));
        assert!(elements[0].secure);
        assert!(elements[0].value.is_none());
        assert!(!elements[1].secure);
        assert_eq!(elements[1].value.as_deref(), Some("ada"));
    }

    #[test]
    fn an_empty_tree_without_a_degradation_reason_is_an_error() {
        let request = EngineStateRequest {
            app_id: "notes".to_string(),
            window_id: None,
            max_elements: 10,
            extended: false,
            screenshot: false,
        };
        let error = parse_app_state(&json!({ "elements": [] }), &request).unwrap_err();
        assert_eq!(error.code, codes::A11Y_MISSING);
        // With a reason it is a degraded observation instead.
        let state = parse_app_state(
            &json!({ "elements": [], "degraded": true, "degraded_reason": "a11y bus down" }),
            &request,
        )
        .unwrap();
        assert!(state.degraded);
        assert_eq!(state.degraded_reason.as_deref(), Some("a11y bus down"));
    }

    #[test]
    fn action_results_default_to_unverified() {
        let result = parse_action_result(&json!({ "message": "sent" }));
        assert!(!result.asserted);
        assert_eq!(
            result.unverified_reason,
            Some(ComputerUnverifiedReason::MissingMetadata)
        );
        let asserted =
            parse_action_result(&json!({ "verified": true, "cursor": { "x": 1, "y": 2 } }));
        assert!(asserted.asserted);
        assert_eq!(asserted.cursor, Some((1.0, 2.0)));
        let accessibility =
            parse_action_result(&json!({ "unverified_reason": "accessibility action" }));
        assert_eq!(
            accessibility.unverified_reason,
            Some(ComputerUnverifiedReason::AccessibilityActionUnasserted)
        );
    }

    #[test]
    fn screenshots_parse_and_keep_their_scale() {
        let screenshot = parse_screenshot(&json!({
            "screenshot": { "data": "QUJD", "format": "jpeg", "width": 1280, "height": 800, "vision_scale": 0.85 }
        }))
        .unwrap();
        assert_eq!(screenshot.mime_type, "image/jpeg");
        assert_eq!(screenshot.width, 1280);
        assert_eq!(screenshot.scale, 0.85);
        assert!(parse_screenshot(&json!({ "screenshot": { "data": "" } })).is_none());
        assert!(parse_screenshot(&json!({})).is_none());
    }

    use vibex_core::ComputerUnavailableReason;

    #[test]
    fn permission_probes_map_onto_the_shared_report() {
        let macos = parse_permissions(
            &json!({ "accessibility": true, "screen_recording": "restart_required" }),
            ComputerPlatform::Macos,
        );
        assert_eq!(macos.accessibility, ComputerPermissionState::Granted);
        assert_eq!(
            macos.screen_recording,
            ComputerPermissionState::RestartRequired
        );
        assert_eq!(
            macos.blocking_reason(),
            Some(ComputerUnavailableReason::PermissionRestartRequired)
        );
        // Linux has no per-application grant ceremony.
        let linux = parse_permissions(&json!({}), ComputerPlatform::LinuxX11);
        assert_eq!(linux.accessibility, ComputerPermissionState::NotRequired);
        assert_eq!(linux.blocking_reason(), None);
    }

    #[test]
    fn platform_detection_reads_the_session_environment() {
        // The unit test cannot control the real environment safely, so it pins
        // the classification rule instead: the compile target decides macOS
        // and Windows, and Linux is decided by XDG_SESSION_TYPE / WAYLAND_DISPLAY
        // before DISPLAY.
        let detected = detect_platform();
        if cfg!(target_os = "macos") {
            assert_eq!(detected, ComputerPlatform::Macos);
        } else if cfg!(target_os = "windows") {
            assert_eq!(detected, ComputerPlatform::Windows);
        }
    }

    #[test]
    fn digests_are_stable_and_content_sensitive() {
        let element = EngineElement {
            index: 0,
            role: "button".to_string(),
            name: "Save".to_string(),
            value: None,
            editable: false,
            secure: false,
            disabled: false,
            bounds: None,
        };
        let first = digest_elements(std::slice::from_ref(&element));
        assert_eq!(first, digest_elements(std::slice::from_ref(&element)));
        let mut changed = element.clone();
        changed.name = "Delete".to_string();
        assert_ne!(first, digest_elements(&[changed]));
    }

    /// The Phase 0 check: does this crate's view of a real driver match?
    ///
    /// Ignored by default because it needs an installed engine and a desktop
    /// session; run it with `--ignored` on a machine that has both.
    #[tokio::test]
    #[ignore = "requires the desktop driver and a desktop session"]
    async fn a_real_driver_answers_this_crates_probe() {
        let driver = CuaDriverCli::discover().expect("the driver is installed on this machine");
        let probe = driver
            .probe()
            .await
            .expect("the driver answers its own probe");
        assert!(
            probe.engine.is_some(),
            "the driver reports which binary answered"
        );
        assert!(
            probe.tool_surface.is_some(),
            "the tool listing parses; a real driver prints `name: description` lines"
        );
        let surface = probe.tool_surface.unwrap();
        assert!(surface.contains("list_apps"), "{surface}");
        // The doctor's own vocabulary is what the settings page shows.
        for entry in &probe.degraded {
            assert!(!entry.is_empty());
        }
    }

    /// The other half of the Phase 0 check: a tool call reaches the desktop.
    ///
    /// Ignored by default; run with `--ignored` where the engine is installed.
    /// The daemon is started by this call and dies with the process.
    #[tokio::test]
    #[ignore = "requires the desktop driver and a desktop session"]
    async fn a_real_driver_lists_applications() {
        let driver = CuaDriverCli::discover().expect("the driver is installed on this machine");
        let apps = driver.list_apps().await.expect("list_apps answers");
        assert!(
            !apps.is_empty(),
            "a running desktop reports at least one application"
        );
        assert!(
            apps.iter().all(|app| !app.display_name.is_empty()),
            "every application carries a name the model can address"
        );
    }

    #[test]
    fn discovery_is_read_only_and_respects_the_override() {
        let temporary = tempfile::tempdir().unwrap();
        let script = temporary.path().join("cua-driver");
        std::fs::write(&script, "#!/bin/sh\necho '{}'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // SAFETY: the test process sets and restores one environment variable
        // around a single-threaded section; no other test reads it.
        let previous = std::env::var_os(DRIVER_PATH_ENV);
        unsafe { std::env::set_var(DRIVER_PATH_ENV, &script) };
        let discovered = CuaDriverCli::discover().expect("the override should be found");
        assert_eq!(discovered.executable(), script.as_path());
        match previous {
            Some(value) => unsafe { std::env::set_var(DRIVER_PATH_ENV, value) },
            None => unsafe { std::env::remove_var(DRIVER_PATH_ENV) },
        }
    }
}

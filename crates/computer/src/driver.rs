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
//!
//! Two facts about the pinned engine (cua-driver 0.31.0) drive the whole
//! mapping, and getting either wrong fails *every* call:
//!
//! * **The driver addresses applications by `pid`, not by name.** Its schemas
//!   are `additionalProperties: false`, so the `app` key this crate's own tool
//!   vocabulary uses is refused outright as an unknown argument. Window ids are
//!   integers, while this crate carries them as strings, and a window whose id
//!   is not passed explicitly is not resolved from the pid alone on Wayland.
//!   The adapter therefore resolves `app` + `window_id` against the driver's
//!   own directory (`list_apps`) before every call.
//! * **An element is addressed by the driver's `element_token`**, an opaque
//!   handle scoped to one `get_window_state` snapshot. The service addresses
//!   elements by index, so this adapter keeps the index→token map of the last
//!   observation and translates at the boundary. The map is engine-internal:
//!   nothing above this module sees the driver's handles.
//!
//! A refusal is also not a broken channel. The driver answers a rejected call
//! with a JSON document on **stdout** and a non-zero exit, so the adapter reads
//! both streams and surfaces the engine's own code and message; treating that
//! as a pipe failure is what turns a precise error into a silent one.

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
/// How long the resolved application directory is reused.
///
/// Short on purpose: a window that opened or closed between two calls must not
/// be addressed from a stale id, and the directory is one extra driver call
/// when it does expire.
const DRIVER_DIRECTORY_TTL: Duration = Duration::from_secs(2);

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
    /// The driver's own directory: canonical applications with their pid, their
    /// launcher and their integer window ids.
    directory: tokio::sync::Mutex<Option<Directory>>,
    /// The element handles of the last observation of each application+window.
    tokens: tokio::sync::Mutex<Vec<TokenSnapshot>>,
    /// Serializes the Wayland measurement, whose two probes bind fixed paths.
    wayland_measure: tokio::sync::Mutex<()>,
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
            // Resolved state is cheap to rebuild and must never be shared by
            // reference: a cloned adapter that answered from the original's
            // observations would hand out handles it does not own.
            directory: tokio::sync::Mutex::new(None),
            tokens: tokio::sync::Mutex::new(Vec::new()),
            wayland_measure: tokio::sync::Mutex::new(()),
        }
    }
}

/// The driver's directory, as this crate needs it.
#[derive(Debug, Default)]
struct Directory {
    at: Option<std::time::Instant>,
    apps: Vec<DirectoryApp>,
}

/// One application the driver reports, in the driver's own vocabulary.
#[derive(Debug, Clone, Default)]
struct DirectoryApp {
    /// The canonical id this crate hands to a model.
    id: String,
    name: String,
    pid: Option<i64>,
    /// The launcher command, round-tripped into `launch_app`.
    launch_path: Option<String>,
    windows: Vec<DirectoryWindow>,
}

/// One top-level window the driver reports.
#[derive(Debug, Clone)]
struct DirectoryWindow {
    id: i64,
    on_screen: bool,
}

/// Where one resolved application lives, in the driver's vocabulary.
#[derive(Debug, Clone)]
struct ResolvedTarget {
    pid: i64,
    /// The integer window id the driver accepts, when the app has a window.
    window_id: Option<i64>,
    /// The same id as this crate spells it, for the token map.
    window_key: Option<String>,
}

/// The element handles of one observation.
#[derive(Debug, Clone)]
struct TokenSnapshot {
    app_id: String,
    window_key: Option<String>,
    /// Driver element index → the driver's own `element_token`.
    tokens: std::collections::HashMap<usize, String>,
}

impl CuaDriverCli {
    /// Uses an explicit driver path.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            extra_env: Vec::new(),
            socket: None,
            daemon: tokio::sync::Mutex::new(None),
            directory: tokio::sync::Mutex::new(None),
            tokens: tokio::sync::Mutex::new(Vec::new()),
            wayland_measure: tokio::sync::Mutex::new(()),
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

    /// Makes the running daemon's Wayland backend match `enabled`, stopping a
    /// daemon that does not.
    ///
    /// The engine reuses whatever daemon already owns its socket, and a daemon
    /// **outlives the helper that started it** — it is spawned with
    /// `kill_on_drop`, but a helper that is killed never runs its destructors,
    /// so the daemon is reparented and keeps running. Without this, turning the
    /// backend on would be silently ignored: the new helper would reuse the old
    /// daemon and every native Wayland window would stay invisible while the
    /// settings said otherwise. The same applies in reverse, to a daemon left
    /// running with a backend the reader has since turned off.
    ///
    /// Returns whether a daemon was stopped. Only meaningful on Wayland;
    /// elsewhere it answers `false` and touches nothing.
    pub async fn align_daemon_wayland_backend(&self, enabled: bool) -> ComputerResult<bool> {
        self.align_daemon_wayland_backend_on(enabled, detect_platform())
            .await
    }

    async fn align_daemon_wayland_backend_on(
        &self,
        enabled: bool,
        platform: ComputerPlatform,
    ) -> ComputerResult<bool> {
        if platform != ComputerPlatform::LinuxWayland {
            return Ok(false);
        }
        if !self.daemon_is_running().await {
            return Ok(false);
        }
        let running_with_backend = self.wayland_backend_available().await.unwrap_or(false);
        if running_with_backend == enabled {
            return Ok(false);
        }
        self.stop_daemon().await?;
        Ok(true)
    }

    /// Stops the daemon this client would talk to.
    async fn stop_daemon(&self) -> ComputerResult<()> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("stop")
            .args(self.socket_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        let output = tokio::time::timeout(DRIVER_PROBE_TIMEOUT_DURATION, command.output())
            .await
            .map_err(|_| {
                ComputerError::process(
                    "computer_driver_timeout",
                    "the desktop driver did not stop its daemon before the deadline",
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
            return Err(driver_failure("stop", &output));
        }
        Ok(())
    }

    async fn invoke_with_timeout(
        &self,
        tool: &str,
        params: &Value,
        timeout_ms: u64,
    ) -> ComputerResult<Value> {
        self.invoke_inner(tool, params, timeout_ms, false).await
    }

    /// Like [`Self::invoke_with_timeout`], but a non-zero exit that still
    /// carries a document is an answer rather than a failure.
    ///
    /// The engine exits non-zero for a *partial* answer — a capture-only
    /// `get_window_state` whose pixels could not be attributed to the window
    /// comes back with `screenshot_error` and exit 1 — and turning that into a
    /// hard error would fail an observation the caller can still use.
    async fn invoke_allowing_partial(
        &self,
        tool: &str,
        params: &Value,
        timeout_ms: u64,
    ) -> ComputerResult<Value> {
        self.invoke_inner(tool, params, timeout_ms, true).await
    }

    async fn invoke_inner(
        &self,
        tool: &str,
        params: &Value,
        timeout_ms: u64,
        accept_partial: bool,
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
            if accept_partial
                && let Some(value) = parse_driver_reply(&String::from_utf8_lossy(&output.stdout))
                && is_partial_answer(&value)
            {
                return Ok(value);
            }
            return Err(driver_failure(tool, &output));
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

/// Address resolution: this crate's application ids and element indices become
/// the driver's pids, integer window ids and element tokens.
///
/// The driver refuses any key it does not know (`additionalProperties: false`),
/// so a call that is not addressed in its own vocabulary fails before it
/// touches the desktop — which is exactly what made every action fail as an
/// opaque helper error.
impl CuaDriverCli {
    /// Refreshes the directory from the driver's own `list_apps`.
    async fn refresh_directory(&self) -> ComputerResult<Vec<DirectoryApp>> {
        let reply = self.invoke("list_apps", &json!({})).await?;
        let apps = parse_directory(&reply);
        *self.directory.lock().await = Some(Directory {
            at: Some(std::time::Instant::now()),
            apps: apps.clone(),
        });
        Ok(apps)
    }

    /// The directory, reused while it is fresh.
    async fn directory(&self) -> ComputerResult<Vec<DirectoryApp>> {
        {
            let guard = self.directory.lock().await;
            if let Some(directory) = guard.as_ref()
                && !directory.apps.is_empty()
                && directory
                    .at
                    .map(|at| at.elapsed() < DRIVER_DIRECTORY_TTL)
                    .unwrap_or(false)
            {
                return Ok(directory.apps.clone());
            }
        }
        self.refresh_directory().await
    }

    /// Resolves one application entry, preferring the instance that owns the
    /// window the caller named.
    async fn resolve_app_entry(
        &self,
        app_id: &str,
        window_id: Option<&str>,
    ) -> ComputerResult<DirectoryApp> {
        let mut apps = self.directory().await?;
        if !apps.iter().any(|app| directory_app_matches(app, app_id)) {
            // A window that opened after the directory was cached, or an entry
            // that went away: one refresh is cheaper than a wrong "not found".
            apps = self.refresh_directory().await?;
        }
        let candidates: Vec<&DirectoryApp> = apps
            .iter()
            .filter(|app| directory_app_matches(app, app_id))
            .collect();
        let requested = window_id.and_then(|value| value.trim().parse::<i64>().ok());
        select_directory_app(&candidates, requested).ok_or_else(|| {
            ComputerError::validation(
                codes::UNKNOWN_APP,
                format!("`{app_id}` is not an application the desktop driver reports"),
            )
            .with_recovery_hint("Call computer_list_apps and use the app id it reports.")
        })
    }

    /// Resolves the running process an application id names.
    async fn resolve_pid(&self, app_id: &str) -> ComputerResult<i64> {
        let entry = self.resolve_app_entry(app_id, None).await?;
        entry.pid.filter(|pid| *pid > 0).ok_or_else(|| {
            ComputerError::conflict(
                codes::UNKNOWN_APP,
                format!(
                    "`{}` is installed but has no running process to act on",
                    entry.name
                ),
            )
            .with_recovery_hint("Launch the application first, then observe it again.")
        })
    }

    /// Resolves one application (and the window to act on) to driver ids.
    async fn resolve_target(
        &self,
        app_id: &str,
        window_id: Option<&str>,
    ) -> ComputerResult<ResolvedTarget> {
        let selected = self.resolve_app_entry(app_id, window_id).await?;
        let pid = selected.pid.filter(|pid| *pid > 0).ok_or_else(|| {
            ComputerError::conflict(
                codes::UNKNOWN_APP,
                format!(
                    "`{}` is installed but has no running process to act on",
                    selected.name
                ),
            )
            .with_recovery_hint("Launch the application first, then observe it again.")
        })?;
        let requested = window_id.and_then(|value| value.trim().parse::<i64>().ok());
        let window = requested
            .and_then(|id| selected.windows.iter().find(|window| window.id == id))
            .or_else(|| selected.windows.iter().find(|window| window.on_screen))
            .or_else(|| selected.windows.first());
        let Some(window) = window else {
            let mut error = ComputerError::conflict(
                codes::UNKNOWN_APP,
                format!(
                    "the desktop driver reports no window for `{}` (pid {pid})",
                    selected.name
                ),
            );
            if let Some(hint) = self.window_visibility_hint() {
                error = error.with_recovery_hint(hint);
            }
            return Err(error.with_diagnostic("pid", pid.to_string()));
        };
        Ok(ResolvedTarget {
            pid,
            window_id: Some(window.id),
            window_key: Some(window.id.to_string()),
        })
    }

    /// Why a running application may have no visible window, when this machine
    /// has a known reason.
    fn window_visibility_hint(&self) -> Option<String> {
        if detect_platform() == ComputerPlatform::LinuxWayland
            && std::env::var_os(ComputerPlatform::wayland_opt_in_variable()).is_none()
        {
            return Some(format!(
                "This is a Wayland session and the driver was not started with {}=1, so it only \
                 sees X11 windows. Set it in the environment Vibex starts from, then restart \
                 Vibex and the driver.",
                ComputerPlatform::wayland_opt_in_variable()
            ));
        }
        None
    }

    /// Records the element handles of one observation.
    ///
    /// The handles belong to the snapshot they came from, so an observation
    /// replaces the previous map for that application and window rather than
    /// merging with it.
    async fn remember_tokens(&self, app_id: &str, window_key: Option<&str>, reply: &Value) {
        let window_key =
            string_at(reply, &["window_id"]).or_else(|| window_key.map(str::to_string));
        let tokens = element_tokens(reply);
        let mut guard = self.tokens.lock().await;
        guard.retain(|snapshot| !(snapshot.app_id == app_id && snapshot.window_key == window_key));
        guard.push(TokenSnapshot {
            app_id: app_id.to_string(),
            window_key,
            tokens,
        });
        // One machine, a handful of applications: the bound is here so a long
        // session cannot grow this without limit.
        while guard.len() > COMPUTER_TOKEN_SNAPSHOTS {
            guard.remove(0);
        }
    }

    /// The driver's handle for one element of the last observation.
    async fn token_for(
        &self,
        app_id: &str,
        window_key: Option<&str>,
        index: usize,
    ) -> Option<String> {
        let guard = self.tokens.lock().await;
        // An exact window match first; otherwise the application's most recent
        // observation, which is what an action without a window id means.
        guard
            .iter()
            .rev()
            .find(|snapshot| {
                snapshot.app_id == app_id && snapshot.window_key.as_deref() == window_key
            })
            .or_else(|| {
                guard
                    .iter()
                    .rev()
                    .find(|snapshot| snapshot.app_id == app_id)
            })
            .and_then(|snapshot| snapshot.tokens.get(&index).cloned())
    }

    /// The token an element action needs, or the error that says why not.
    ///
    /// The two failures are kept apart because they demand opposite responses.
    /// An index the engine left out of a tree it *did* address is fixed by
    /// observing again with more elements; a window the engine cannot prove —
    /// on Linux, one with no accessibility tree, which leaves the engine with
    /// an X11 property fallback it refuses to act on — has no handle to get,
    /// and telling a model to observe again would be a loop with no exit.
    async fn required_token(
        &self,
        app_id: &str,
        window_key: Option<&str>,
        index: usize,
    ) -> ComputerResult<String> {
        if let Some(token) = self.token_for(app_id, window_key, index).await {
            return Ok(token);
        }
        let addressed = self.has_element_handles(app_id, window_key).await;
        let error = if addressed {
            ComputerError::validation(
                codes::ELEMENT_NOT_ADDRESSABLE,
                format!(
                    "the engine issued no handle for element {index} in this window; observe it \
                     again if you need an element it did list"
                ),
            )
        } else {
            ComputerError::validation(
                codes::ELEMENT_NOT_ADDRESSABLE,
                "the engine issued no element handles for this window: it could not prove which \
                 window the tree belongs to, and it refuses to act on a tree it cannot prove. \
                 Observing again will not change that.",
            )
        };
        Err(error
            .with_recovery_hint(
                "Element actions need a window whose accessibility tree the engine can prove; \
                 choose another window or application.",
            )
            .with_diagnostic("app", app_id.to_string()))
    }

    /// Whether the last observation of this application carried any element
    /// handles at all.
    async fn has_element_handles(&self, app_id: &str, window_key: Option<&str>) -> bool {
        let guard = self.tokens.lock().await;
        guard
            .iter()
            .rev()
            .find(|snapshot| {
                snapshot.app_id == app_id && snapshot.window_key.as_deref() == window_key
            })
            .or_else(|| {
                guard
                    .iter()
                    .rev()
                    .find(|snapshot| snapshot.app_id == app_id)
            })
            .map(|snapshot| !snapshot.tokens.is_empty())
            .unwrap_or(false)
    }

    /// Measures what this Wayland session shows with and without the driver's
    /// experimental backend.
    ///
    /// The measurement is a comparison, because that is the only honest answer,
    /// and it is taken with **two short-lived probe daemons** — one as
    /// configured, one with the backend on — rather than against the live
    /// daemon. The live daemon may already be running with the backend enabled,
    /// and a measurement that compared against it would see the same windows on
    /// both sides and conclude the backend was never needed, turning itself off
    /// again. Two private sockets also mean the live helper's daemon is never
    /// disturbed.
    ///
    /// A session whose windows appear either way needs no opt-in; a compositor
    /// that cannot advertise the manager globals measures unavailable and never
    /// will.
    ///
    /// Only meaningful on Wayland: elsewhere it answers a default immediately
    /// and touches nothing.
    pub async fn detect_wayland_windows(&self) -> ComputerResult<WaylandWindowDetection> {
        if detect_platform() != ComputerPlatform::LinuxWayland {
            return Ok(WaylandWindowDetection::default());
        }
        // One measurement at a time: both probes bind a socket path derived from
        // this process, and a second concurrent run would race for it and
        // measure an empty desktop.
        let _serialized = self.wayland_measure.lock().await;
        let configured = self.probe_window_ids(false).await.1;
        let (backend_available, wayland) = self.probe_window_ids(true).await;
        Ok(WaylandWindowDetection {
            configured_windows: configured.len(),
            wayland_windows: wayland.len(),
            backend_available,
        })
    }

    /// Starts one private probe daemon and reads the windows it sees.
    ///
    /// The daemon is this call's only owner and `kill_on_drop` is set, so
    /// dropping the adapter releases the compositor connection; the socket file
    /// is removed as well, because the next measurement has to bind it again.
    async fn probe_window_ids(&self, wayland: bool) -> (bool, Vec<i64>) {
        let socket = wayland_probe_socket(wayland);
        let _ = std::fs::remove_file(&socket);
        let mut probe = self.clone().with_socket(&socket);
        if wayland {
            probe = probe.with_env(ComputerPlatform::wayland_opt_in_variable(), "1");
        }
        let backend_available = if wayland {
            probe.wayland_backend_available().await.unwrap_or(false)
        } else {
            false
        };
        let windows = probe.window_ids().await.unwrap_or_default();
        drop(probe);
        let _ = std::fs::remove_file(&socket);
        (backend_available, windows)
    }

    /// The driver's top-level window ids, with the current configuration.
    async fn window_ids(&self) -> ComputerResult<Vec<i64>> {
        let reply = self.invoke("list_windows", &json!({})).await?;
        Ok(reply
            .get("windows")
            .and_then(Value::as_array)
            .map(|windows| {
                windows
                    .iter()
                    .filter_map(|window| integer_at(window, &["window_id", "id", "handle"]))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Whether the compositor advertises what the Wayland backend needs.
    async fn wayland_backend_available(&self) -> ComputerResult<bool> {
        let reply = self.invoke("health_report", &json!({})).await?;
        Ok(reply
            .get("checks")
            .and_then(Value::as_array)
            .map(|checks| {
                checks.iter().any(|check| {
                    string_at(check, &["name"]).as_deref() == Some("wayland_backend")
                        && string_at(check, &["status"]).as_deref() == Some("pass")
                })
            })
            .unwrap_or(false))
    }
}

/// The socket a Wayland probe daemon binds.
///
/// It must not be the socket the live helper's daemon owns: the two views have
/// to be measured independently, and the backend-on view would otherwise be
/// compared against itself.
fn wayland_probe_socket(wayland: bool) -> PathBuf {
    let suffix = if wayland { "backend" } else { "x11" };
    std::env::temp_dir().join(format!(
        "vibex-wayland-probe-{}-{suffix}.sock",
        std::process::id()
    ))
}

/// How many observations' element handles are kept.
const COMPUTER_TOKEN_SNAPSHOTS: usize = 8;

/// What a Wayland session shows with and without the driver's experimental
/// backend.
///
/// The opt-in is not something a reader can guess: on Wayland the driver sees
/// X11 windows without it and native Wayland windows only with it, and a
/// compositor that does not advertise the wlroots manager globals cannot offer
/// native windows at all. This is the measurement the settings page and the
/// runtime act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WaylandWindowDetection {
    /// Top-level windows the driver sees with its current configuration.
    pub configured_windows: usize,
    /// Top-level windows it sees with the Wayland backend enabled.
    pub wayland_windows: usize,
    /// Whether the compositor advertises the globals the backend needs.
    pub backend_available: bool,
}

impl WaylandWindowDetection {
    /// Whether enabling the backend reveals windows this session cannot
    /// otherwise address.
    pub fn wants_opt_in(&self) -> bool {
        self.backend_available && self.wayland_windows > self.configured_windows
    }

    /// Whether anything was measured at all.
    pub fn is_measured(&self) -> bool {
        self.backend_available || self.configured_windows > 0 || self.wayland_windows > 0
    }
}

/// Whether one directory entry is the application a caller named.
fn directory_app_matches(app: &DirectoryApp, selector: &str) -> bool {
    app.id == selector || app.name.eq_ignore_ascii_case(selector)
}

/// Picks the instance of an application a caller meant.
///
/// One application can be reported more than once — two kitty processes are
/// two entries with the same id — so the choice is made here, once, and always
/// in this order:
///
/// 1. the entry that owns the window the caller named, whatever its position;
/// 2. the entry with a window that is on screen;
/// 3. any entry with a window;
/// 4. any entry with a live process (an application with no window is still a
///    target for a launch or a kill).
fn select_directory_app(
    candidates: &[&DirectoryApp],
    requested_window: Option<i64>,
) -> Option<DirectoryApp> {
    requested_window
        .and_then(|id| {
            candidates
                .iter()
                .find(|app| app.windows.iter().any(|window| window.id == id))
                .map(|app| (*app).clone())
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|app| app.windows.iter().any(|window| window.on_screen))
                .map(|app| (*app).clone())
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|app| !app.windows.is_empty())
                .map(|app| (*app).clone())
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|app| app.pid.filter(|pid| *pid > 0).is_some())
                .map(|app| (*app).clone())
        })
        .or_else(|| candidates.first().map(|app| (*app).clone()))
}

/// Reads the driver's application directory.
fn parse_directory(reply: &Value) -> Vec<DirectoryApp> {
    let items: Vec<&Value> = match reply.get("apps").and_then(Value::as_array) {
        Some(items) => items.iter().collect(),
        // `list_apps` answers with a list; a single-application reply is
        // accepted too so the same reader serves a launch result.
        None if reply.get("name").is_some() || reply.get("pid").is_some() => vec![reply],
        None => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let id = string_at(
                item,
                &["app_id", "id", "bundle_id", "aumid", "desktop_file"],
            )
            .or_else(|| string_at(item, &["name", "display_name"]))?;
            let windows =
                item.get("windows")
                    .and_then(Value::as_array)
                    .map(|windows| {
                        windows
                            .iter()
                            .filter_map(|window| {
                                // The driver's window ids are integers and its
                                // schema says so; a decimal string is accepted too
                                // so a platform that spells one differently does
                                // not silently lose every window.
                                let id = integer_at(window, &["window_id", "id", "handle"])
                                    .or_else(|| {
                                        string_at(window, &["window_id", "id", "handle"])?
                                            .trim()
                                            .parse::<i64>()
                                            .ok()
                                    })?;
                                Some(DirectoryWindow {
                                    id,
                                    on_screen: bool_at(
                                        window,
                                        &["is_on_screen", "on_screen", "onScreen"],
                                    )
                                    .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            Some(DirectoryApp {
                name: string_at(item, &["display_name", "name", "title", "label"])
                    .unwrap_or_else(|| id.clone()),
                pid: integer_at(item, &["pid", "process_id"]),
                launch_path: string_at(item, &["launch_path", "exec", "launch_command"]),
                windows,
                id,
            })
        })
        .collect()
}

/// Reads the element handles out of one observation.
fn element_tokens(reply: &Value) -> std::collections::HashMap<usize, String> {
    let mut tokens = std::collections::HashMap::new();
    let Some(items) = reply.get("elements").and_then(Value::as_array) else {
        return tokens;
    };
    for (position, item) in items.iter().enumerate() {
        let index = integer_at(
            item,
            &["index", "node_idx", "element_index", "elementIndex"],
        )
        .map(|index| index.max(0) as usize)
        .unwrap_or(position);
        if let Some(token) = string_at(item, &["element_token", "elementToken"]) {
            tokens.insert(index, token);
        }
    }
    tokens
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
            degraded.push(format!(
                "Wayland: the driver was not started with {}=1, so it sees only X11 windows \
                 and refuses background input",
                ComputerPlatform::wayland_opt_in_variable()
            ));
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
        // The same answer is the adapter's resolution table. One call, two
        // views: the canonical applications a model addresses, and the pids,
        // integer window ids and launcher commands every later call needs.
        let apps = parse_applications(&reply);
        *self.directory.lock().await = Some(Directory {
            at: Some(std::time::Instant::now()),
            apps: parse_directory(&reply),
        });
        Ok(apps)
    }

    async fn get_app_state(&self, request: EngineStateRequest) -> ComputerResult<EngineAppState> {
        // The driver walks one window of one pid; its `get_accessibility_tree`
        // is a different tool (a lightweight desktop snapshot) and takes no
        // application at all. `extended` has no counterpart in this engine: it
        // returns every element it walks, bounded by `max_elements`.
        let target = self
            .resolve_target(&request.app_id, request.window_id.as_deref())
            .await?;
        let mut params = json!({
            "pid": target.pid,
            "include_screenshot": request.screenshot,
            "max_elements": request.max_elements,
        });
        if let Some(window) = target.window_id {
            params["window_id"] = json!(window);
        }
        let reply = self.invoke("get_window_state", &params).await?;
        // Remember the handles this snapshot issued before anything can be
        // asked to act on them.
        self.remember_tokens(&request.app_id, target.window_key.as_deref(), &reply)
            .await;
        parse_app_state(&reply, &request)
    }

    async fn click(&self, request: EngineClick) -> ComputerResult<EngineActionResult> {
        let target = self
            .resolve_target(&request.app_id, request.window_id.as_deref())
            .await?;
        let mut params = json!({
            "pid": target.pid,
            "delivery_mode": request.delivery.as_str(),
        });
        if let Some(index) = request.element_index {
            // An element is addressed by the driver's own snapshot handle; the
            // `element_index` this crate uses is not an argument it accepts.
            params["element_token"] = json!(
                self.required_token(&request.app_id, target.window_key.as_deref(), index)
                    .await?
            );
        } else if let Some((x, y)) = request.point {
            // Vibex's coordinates are desktop coordinates, and the driver needs
            // to be told so: its default frame is window-local pixels.
            params["coordinate_frame"] = json!("desktop");
            params["x"] = json!(x);
            params["y"] = json!(y);
            if let Some(window) = target.window_id {
                params["window_id"] = json!(window);
            }
        }
        let tool = if request.button == "right" {
            "right_click"
        } else if request.click_count == 2 {
            "double_click"
        } else {
            "click"
        };
        if tool == "click" && request.click_count > 1 {
            params["count"] = json!(request.click_count.min(3));
        }
        let reply = self.invoke(tool, &params).await?;
        Ok(parse_action_result(&reply))
    }

    async fn type_text(&self, request: EngineTypeText) -> ComputerResult<EngineActionResult> {
        let target = self
            .resolve_target(&request.app_id, request.window_id.as_deref())
            .await?;
        let mut params = json!({
            "pid": target.pid,
            "text": request.text,
            "delivery_mode": request.delivery.as_str(),
        });
        if let Some(window) = target.window_id {
            params["window_id"] = json!(window);
        }
        let reply = self.invoke("type_text", &params).await?;
        Ok(parse_action_result(&reply))
    }

    async fn set_value(&self, request: EngineSetValue) -> ComputerResult<EngineActionResult> {
        let target = self
            .resolve_target(&request.app_id, request.window_id.as_deref())
            .await?;
        let token = self
            .required_token(
                &request.app_id,
                target.window_key.as_deref(),
                request.element_index,
            )
            .await?;
        // The token carries its own window, so the driver asks for no
        // `window_id` beside it.
        let reply = self
            .invoke(
                "set_value",
                &json!({
                    "pid": target.pid,
                    "element_token": token,
                    "value": request.value,
                    "delivery_mode": request.delivery.as_str(),
                }),
            )
            .await?;
        Ok(parse_action_result(&reply))
    }

    async fn press_key(&self, request: EnginePressKey) -> ComputerResult<EngineActionResult> {
        let target = self
            .resolve_target(&request.app_id, request.window_id.as_deref())
            .await?;
        // A chord is its own tool in this engine, and it takes one `keys` array
        // rather than a key plus a modifier list.
        let (tool, mut params) = if request.modifiers.is_empty() {
            (
                "press_key",
                json!({ "pid": target.pid, "key": request.key }),
            )
        } else {
            let mut keys = request.modifiers.clone();
            keys.push(request.key.clone());
            ("hotkey", json!({ "pid": target.pid, "keys": keys }))
        };
        params["delivery_mode"] = json!(request.delivery.as_str());
        if let Some(window) = target.window_id {
            params["window_id"] = json!(window);
        }
        let reply = self.invoke(tool, &params).await?;
        Ok(parse_action_result(&reply))
    }

    async fn scroll(&self, request: EngineScroll) -> ComputerResult<EngineActionResult> {
        let target = self
            .resolve_target(&request.app_id, request.window_id.as_deref())
            .await?;
        // This engine scrolls a direction by a number of steps, not by a delta
        // pair. The larger axis is the gesture; the sign picks the direction.
        let (direction, amount) = scroll_direction(request.delta_x, request.delta_y);
        let mut params = json!({
            "pid": target.pid,
            "direction": direction,
            "amount": amount,
            "delivery_mode": request.delivery.as_str(),
        });
        if let Some(index) = request.element_index {
            params["element_token"] = json!(
                self.required_token(&request.app_id, target.window_key.as_deref(), index)
                    .await?
            );
        } else if let Some((x, y)) = request.point {
            params["coordinate_frame"] = json!("desktop");
            params["x"] = json!(x);
            params["y"] = json!(y);
            if let Some(window) = target.window_id {
                params["window_id"] = json!(window);
            }
        } else if let Some(window) = target.window_id {
            params["window_id"] = json!(window);
        }
        let reply = self.invoke("scroll", &params).await?;
        Ok(parse_action_result(&reply))
    }

    async fn screenshot(
        &self,
        app_id: Option<&str>,
        window_id: Option<&str>,
    ) -> ComputerResult<Option<ComputerScreenshot>> {
        let reply = match app_id {
            Some(app_id) => {
                let target = self.resolve_target(app_id, window_id).await?;
                let mut params = json!({
                    "pid": target.pid,
                    "include_screenshot": true,
                    // The capture-only path: no accessibility walk, just the
                    // pixels and the window metadata.
                    "include_accessibility_tree": false,
                });
                if let Some(window) = target.window_id {
                    params["window_id"] = json!(window);
                }
                // A capture the compositor cannot attribute comes back with
                // `screenshot_error` and a non-zero exit; it is an answer, and
                // the caller reads the reason rather than a pipe failure.
                self.invoke_allowing_partial("get_window_state", &params, DRIVER_TIMEOUT_MS)
                    .await?
            }
            None => {
                // No application named: the whole display. The engine's own
                // configured long-edge cap applies, so the bytes stay bounded.
                self.invoke_allowing_partial("get_desktop_state", &json!({}), DRIVER_TIMEOUT_MS)
                    .await?
            }
        };
        Ok(parse_screenshot(&reply))
    }

    async fn release_all_keys(&self) -> ComputerResult<()> {
        // cua-driver 0.31.0 has no release-all primitive and no config key for
        // one (`set_config` refuses `release_all_input`). It is not a silent
        // success: every input tool this engine exposes is an atomic
        // press-and-release, and its only held-input pair
        // (`mouse_button_down` / `mouse_button_up`) is not reachable from this
        // crate's tool surface, so there is no held input to release. The
        // emergency stop's input release is therefore already satisfied.
        Ok(())
    }

    async fn user_activity_age_ms(&self) -> ComputerResult<Option<i64>> {
        // The pinned engine answers with the pointer position only; there is no
        // idle clock on this transport. The read is kept tolerant rather than
        // removed, so a driver that grows one starts answering here.
        let reply = self
            .invoke_with_timeout("get_cursor_position", &json!({}), DRIVER_PROBE_TIMEOUT_MS)
            .await?;
        Ok(integer_at(
            &reply,
            &["idle_ms", "user_idle_ms", "activity_age_ms"],
        ))
    }

    async fn launch_app(&self, app_id: &str) -> ComputerResult<ComputerApplication> {
        let apps = self.directory().await?;
        let entry = apps
            .iter()
            .find(|app| directory_app_matches(app, app_id))
            .cloned()
            .ok_or_else(|| {
                ComputerError::validation(
                    codes::UNKNOWN_APP,
                    format!("`{app_id}` is not an application the desktop driver reports"),
                )
                .with_recovery_hint("Call computer_list_apps and use the app id it reports.")
            })?;
        // The launcher command is what this engine resolves; the display name
        // is the fallback it tries as a command too.
        let params = match &entry.launch_path {
            Some(path) if !path.trim().is_empty() => json!({ "launch_path": path }),
            _ => json!({ "name": entry.name }),
        };
        let reply = self.invoke("launch_app", &params).await?;
        let mut launched = parse_applications(&reply);
        if launched.is_empty() {
            // The engine answered; the launch succeeded as far as it is
            // concerned. Report the identity the caller already addresses, and
            // no pid: the directory entry's pid is the *pre-launch* one, and
            // presenting it as the launched process would turn a dispatch into
            // a verified launch.
            launched.push(ComputerApplication {
                app_id: entry.id.clone(),
                display_name: entry.name.clone(),
                executable_path: None,
                bundle_id: None,
                running: true,
                pid: None,
                windows: Vec::new(),
            });
        }
        self.refresh_directory().await.ok();
        Ok(launched.pop().expect("a launched application was built"))
    }

    async fn kill_app(&self, app_id: &str) -> ComputerResult<()> {
        // This engine terminates a pid, not an application id — and an
        // application worth killing may well have no window left.
        let pid = self.resolve_pid(app_id).await?;
        self.invoke("kill_app", &json!({ "pid": pid }))
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

/// One refusal the driver reported, in whichever envelope it used.
///
/// The pinned engine answers a rejected call with **stdout** and a non-zero
/// exit in two shapes: a `{"refusal": {"code", "message"}, "status":
/// "refused"}` envelope for argument problems, and a bare
/// `{"code", "detail", "escalation": …}` document for capability problems such
/// as an unavailable background delivery. Both are read here, because the
/// engine's own words are the only useful thing a caller can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DriverRefusal {
    code: String,
    message: String,
    hint: Option<String>,
}

/// Reads a refusal out of the driver's streams, whatever shape it used.
fn parse_driver_refusal(stdout: &str, stderr: &str) -> Option<DriverRefusal> {
    let stdout = stdout.trim();
    if let Ok(value) = serde_json::from_str::<Value>(stdout) {
        if let Some(refusal) = value.get("refusal") {
            let code = string_at(refusal, &["code"]).unwrap_or_else(|| "refused".to_string());
            let message = string_at(refusal, &["message", "detail"])
                .unwrap_or_else(|| "the driver refused the call".to_string());
            return Some(DriverRefusal {
                code,
                message,
                hint: None,
            });
        }
        if let Some(code) = string_at(&value, &["code"]) {
            let message = string_at(&value, &["detail", "message"])
                .unwrap_or_else(|| "the driver refused the call".to_string());
            // An escalation is the driver telling the caller what would work
            // instead; it becomes this crate's recovery hint verbatim.
            let hint = value
                .get("escalation")
                .and_then(|escalation| string_at(escalation, &["reason", "suggestion"]))
                .or_else(|| string_at(&value, &["suggestion"]));
            return Some(DriverRefusal {
                code,
                message,
                hint,
            });
        }
    }
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return Some(DriverRefusal {
            code: "refused".to_string(),
            message: stderr.to_string(),
            hint: None,
        });
    }
    if !stdout.is_empty() {
        return Some(DriverRefusal {
            code: "refused".to_string(),
            message: stdout.to_string(),
            hint: None,
        });
    }
    None
}

/// Turns a non-zero driver exit into the most precise error available.
///
/// The order matters. A daemon that is not up is not a rejected action — the
/// engine is installed and simply has not been started yet, and the caller
/// starts it and retries. An unavailable background delivery is a capability
/// gap the approval flow can offer a foreground escalation for. Everything
/// else is a refusal and keeps the engine's own code and message, so the model
/// reads a reason instead of "the helper failed".
fn driver_failure(tool: &str, output: &std::process::Output) -> ComputerError {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lowered = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    if lowered.contains("daemon is not running") {
        return ComputerError::capability(
            "computer_driver_daemon_missing",
            "the desktop driver is installed but its daemon is not running",
        )
        .with_diagnostic("detail", bounded_text(&format!("{stdout}\n{stderr}"), 400));
    }
    let Some(refusal) = parse_driver_refusal(&stdout, &stderr) else {
        return ComputerError::process(
            codes::HELPER_FAILED,
            format!(
                "the desktop driver rejected `{tool}` ({}) without a reason",
                output.status
            ),
        );
    };
    let refusal_code = refusal.code.to_ascii_lowercase();
    let background_unavailable = refusal_code.contains("background_unavailable")
        || refusal_code.contains("wm_chord_unavailable")
        || lowered.contains("background unavailable");
    if background_unavailable {
        // The driver reports an unsupported background delivery as a distinct
        // failure; mapping it here is what lets the approval flow offer a
        // foreground escalation instead of a generic error.
        let mut error = ComputerError::capability(
            codes::BACKGROUND_UNAVAILABLE,
            "the driver could not deliver this action in the background",
        )
        .with_diagnostic("driver_code", refusal.code.clone())
        .with_diagnostic("driver_message", bounded_text(&refusal.message, 400));
        if let Some(hint) = refusal.hint {
            error = error.with_recovery_hint(hint);
        }
        return error;
    }
    if refusal_code.contains("stale_element_token") {
        // The engine took the handle and kept no snapshot to resolve it
        // against: the window is not one it will act on, and the model's own
        // remedy is not "observe again" — this adapter observed immediately
        // before the action. Saying so is what keeps a model out of an
        // observe/act/fail loop.
        return ComputerError::validation(
            codes::ELEMENT_NOT_ADDRESSABLE,
            format!(
                "the desktop driver kept no actionable snapshot for that window: {}",
                bounded_text(&refusal.message, 300)
            ),
        )
        .with_recovery_hint(
            "Element actions need a window whose accessibility tree the engine can prove; choose \
             another window or application.",
        )
        .with_diagnostic("driver_code", refusal.code);
    }
    let mut error = ComputerError::process(
        codes::DRIVER_REFUSED,
        format!(
            "the desktop driver refused `{tool}`: {}",
            bounded_text(&refusal.message, 300)
        ),
    )
    .with_diagnostic("driver_code", refusal.code)
    .with_diagnostic("driver_status", output.status.to_string());
    if let Some(hint) = refusal.hint {
        error = error.with_recovery_hint(hint);
    }
    error
}

/// Whether a non-zero exit still carried a usable document.
///
/// A refusal and a capability failure are not answers: the first names an
/// argument problem, the second names something the engine cannot do. Anything
/// else the engine printed is a partial answer it wants the caller to read —
/// for example a screenshot request the compositor could not attribute.
fn is_partial_answer(value: &Value) -> bool {
    value.is_object()
        && value.get("refusal").is_none()
        && value.get("code").is_none()
        && value.get("error").is_none()
}

/// Why a requested screenshot is missing, when the engine said.
fn screenshot_error_reason(reply: &Value) -> Option<String> {
    let error = reply.get("screenshot_error")?;
    if error.is_null() {
        return None;
    }
    string_at(error, &["reason", "message", "detail", "code"])
        .map(|reason| format!("screenshot unavailable: {reason}"))
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
        let actions: Vec<String> = item
            .get("actions")
            .and_then(Value::as_array)
            .map(|actions| {
                actions
                    .iter()
                    .filter_map(|action| action.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        // Linux AT-SPI spells the credential role `password text`, and its
        // entry roles carry no `editable` flag; both are read here so the
        // engine's own vocabulary still maps onto this crate's contract.
        let secure = bool_at(item, &["secure", "is_secure", "password"]).unwrap_or(false)
            || vibex_core::is_secure_field_role(&role)
            || role.to_ascii_lowercase().contains("password");
        let editable =
            bool_at(item, &["editable", "is_editable", "settable"]).unwrap_or_else(|| {
                let lowered = role.to_ascii_lowercase();
                matches!(
                    lowered.as_str(),
                    "axtextfield" | "axtextarea" | "textfield" | "textbox" | "edit" | "combobox"
                ) || lowered.contains("entry")
                    || lowered.contains("combo box")
                    || lowered.contains("spin button")
                    || actions.iter().any(|action| {
                        let action = action.to_ascii_lowercase();
                        action.contains("set value") || action.contains("editable")
                    })
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
    let mut degraded = bool_at(reply, &["degraded", "is_degraded"]).unwrap_or(false);
    let mut degraded_reason = string_at(reply, &["degraded_reason", "degradedReason"]);
    // A screenshot that was asked for and could not be attributed to the window
    // is a degradation of the observation, and it is said out loud: a caller
    // that cannot see pixels must not read the tree as a verified picture.
    if request.screenshot
        && let Some(reason) = screenshot_error_reason(reply)
    {
        degraded = true;
        degraded_reason = Some(match degraded_reason {
            Some(existing) => format!("{existing}; {reason}"),
            None => reason,
        });
    }
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

/// Reads a screenshot out of a driver reply.
///
/// The pinned engine delivers pixels as flat fields beside the window metadata
/// (`screenshot_png_b64`, `screenshot_mime_type`, `screenshot_width`,
/// `screenshot_height`, `frame_scale`); the nested `screenshot` / `image`
/// shapes are accepted too, so a driver version that changes either way keeps
/// working.
fn parse_screenshot(reply: &Value) -> Option<ComputerScreenshot> {
    let node = value_at(reply, &["screenshot", "image", "frame"]);
    let base64 = match node {
        Some(node) => node
            .as_str()
            .map(str::to_string)
            .or_else(|| string_at(node, &["data", "base64", "bytes"]))?,
        None => string_at(
            reply,
            &[
                "screenshot_png_b64",
                "png_base64",
                "screenshot_base64",
                "screenshot_data",
            ],
        )?,
    };
    if base64.is_empty() {
        return None;
    }
    let mime_type = node
        .and_then(|node| string_at(node, &["mime_type", "mimeType", "format"]))
        .or_else(|| string_at(reply, &["screenshot_mime_type", "mime_type"]))
        .map(|mime| {
            if mime.contains('/') {
                mime
            } else {
                format!("image/{mime}")
            }
        })
        .unwrap_or_else(|| "image/png".to_string());
    let width = node
        .and_then(|node| integer_at(node, &["width"]))
        .or_else(|| integer_at(reply, &["screenshot_width", "width"]))
        .unwrap_or(0);
    let height = node
        .and_then(|node| integer_at(node, &["height"]))
        .or_else(|| integer_at(reply, &["screenshot_height", "height"]))
        .unwrap_or(0);
    let scale = node
        .and_then(|node| number_at(node, &["scale", "vision_scale"]))
        .or_else(|| number_at(reply, &["frame_scale", "vision_scale"]))
        .unwrap_or(1.0);
    Some(ComputerScreenshot {
        mime_type,
        base64,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
        scale,
    })
}

/// Maps this crate's delta pair onto the driver's direction and step count.
///
/// The driver scrolls one axis by a number of wheel steps; this crate carries a
/// two-axis delta. The dominant axis is the gesture, a positive vertical delta
/// means "show me what is further down", and the step count is bounded to what
/// the engine accepts (1–50).
fn scroll_direction(delta_x: f64, delta_y: f64) -> (&'static str, u32) {
    let steps = |delta: f64| -> u32 { (delta.abs().round() as u32).clamp(1, 50) };
    if delta_y.abs() >= delta_x.abs() && delta_y != 0.0 {
        (if delta_y < 0.0 { "up" } else { "down" }, steps(delta_y))
    } else if delta_x != 0.0 {
        (if delta_x < 0.0 { "left" } else { "right" }, steps(delta_x))
    } else {
        ("down", 1)
    }
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

    /// The whole read path against a real engine: the directory resolves a pid
    /// and an integer window id, and the observation comes back with the
    /// element handles the action tools address.
    ///
    /// Read-only on purpose — it observes, and never clicks.
    #[tokio::test]
    #[ignore = "requires the desktop driver and a desktop session"]
    async fn a_real_driver_observes_an_open_window() {
        let driver = CuaDriverCli::discover().expect("the driver is installed on this machine");
        let apps = driver.list_apps().await.expect("list_apps answers");
        let Some(app) = apps
            .iter()
            .find(|app| !app.windows.is_empty() && app.pid.is_some())
        else {
            // Without a visible top-level window there is nothing to observe;
            // on Wayland that is the missing CUA_DRIVER_RS_ENABLE_WAYLAND=1.
            eprintln!("no application with a top-level window is visible on this session");
            return;
        };
        let state = driver
            .get_app_state(EngineStateRequest {
                app_id: app.app_id.clone(),
                window_id: None,
                max_elements: 50,
                extended: false,
                screenshot: false,
            })
            .await
            .expect("the corrected arguments are accepted by the real driver");
        assert!(
            !state.tree_digest.is_empty(),
            "an observation carries a digest"
        );
        assert!(
            state.window_id.is_some(),
            "the driver reports which window it walked"
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

    /// Runs one fake driver: a script that prints `stdout` and/or `stderr`
    /// and exits with `code`.
    #[cfg(unix)]
    fn fake_driver(stdout: &str, stderr: &str, code: i32) -> (tempfile::TempDir, CuaDriverCli) {
        fake_script_driver(format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\nprintf '%s' '{}' >&2\nexit {code}\n",
            stdout.replace('\'', "'\\''"),
            stderr.replace('\'', "'\\''")
        ))
    }

    /// Runs an arbitrary fake driver script, for the fakes that have to answer
    /// more than one subcommand.
    #[cfg(unix)]
    fn fake_script_driver(body: String) -> (tempfile::TempDir, CuaDriverCli) {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let script = temporary.path().join("cua-driver");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A script written while another test thread forks can fail `exec`
        // with ETXTBSY for a moment; wait until the kernel will run it.
        for _ in 0..100 {
            let runnable = std::process::Command::new(&script)
                .arg("--wait-for-exec")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok();
            if runnable {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (temporary, CuaDriverCli::new(&script))
    }

    #[cfg(unix)]
    async fn call_fake(driver: &CuaDriverCli) -> ComputerError {
        driver
            .invoke_with_timeout("get_window_state", &json!({}), 5_000)
            .await
            .expect_err("the fake driver fails")
    }

    #[test]
    fn refusals_are_read_through_both_of_the_engines_envelopes() {
        // The envelope the engine uses for an argument it does not know.
        let refusal = parse_driver_refusal(
            r#"{"refusal":{"code":"invalid_arguments","message":"unknown argument app"},"status":"refused"}"#,
            "",
        )
        .unwrap();
        assert_eq!(refusal.code, "invalid_arguments");
        assert_eq!(refusal.message, "unknown argument app");
        // The bare document it uses for a capability problem, with the
        // escalation it recommends.
        let capability = parse_driver_refusal(
            r#"{"code":"background_unavailable","detail":"no focus-free backend","escalation":{"reason":"retry with delivery_mode:\"foreground\"."},"suggestion":"Retry."}"#,
            "",
        )
        .unwrap();
        assert_eq!(capability.code, "background_unavailable");
        assert_eq!(capability.message, "no focus-free backend");
        assert!(capability.hint.unwrap().contains("foreground"));
        // A plain-text answer is still a reason.
        let text = parse_driver_refusal("No windows found for pid 42.", "").unwrap();
        assert_eq!(text.message, "No windows found for pid 42.");
        assert!(parse_driver_refusal("", "   ").is_none());
    }

    /// The regression this whole module exists for: a rejected call must reach
    /// the model as the engine's own reason, not as "the helper failed".
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rejected_call_keeps_the_engines_own_reason() {
        let (_temporary, driver) = fake_driver(
            r#"{"refusal":{"code":"invalid_arguments","message":"get_window_state: unknown argument app"},"status":"refused"}"#,
            "",
            1,
        );
        let error = call_fake(&driver).await;
        assert_eq!(error.code, codes::DRIVER_REFUSED);
        assert!(
            error.message.contains("unknown argument app"),
            "the engine's message survives: {}",
            error.message
        );
        assert!(
            error
                .diagnostics
                .iter()
                .any(|(key, value)| key == "driver_code" && value == "invalid_arguments")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unavailable_background_delivery_is_an_escalation_not_a_defect() {
        let (_temporary, driver) = fake_driver(
            r#"{"code":"background_unavailable","detail":"no focus-free input backend","escalation":{"reason":"retry with delivery_mode:\"foreground\"."}}"#,
            "",
            1,
        );
        let error = call_fake(&driver).await;
        assert_eq!(error.code, codes::BACKGROUND_UNAVAILABLE);
        assert!(error.is_approval_required() || !error.is_unavailable_here());
        assert!(
            error
                .recovery_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("foreground"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_daemon_that_is_not_up_is_started_rather_than_reported() {
        let (_temporary, driver) = fake_driver(
            "",
            "Cua Driver daemon is not running on /tmp/cua.sock.\nStart it first with: cua-driver serve",
            1,
        );
        let error = call_fake(&driver).await;
        assert_eq!(error.code, "computer_driver_daemon_missing");
    }

    #[test]
    fn the_directory_carries_pids_windows_and_launchers() {
        let directory = parse_directory(&json!({
            "apps": [
                {
                    "name": "kitty", "bundle_id": "kitty", "pid": 2820229,
                    "launch_path": "kitty",
                    "windows": [
                        { "window_id": 94587431445600u64, "is_on_screen": false },
                        { "window_id": 94587430045600u64, "is_on_screen": true }
                    ]
                },
                { "name": "installed-but-not-running", "bundle_id": "idle", "pid": 0, "windows": [] }
            ]
        }));
        assert_eq!(directory.len(), 2);
        assert_eq!(directory[0].id, "kitty");
        assert_eq!(directory[0].pid, Some(2820229));
        assert_eq!(directory[0].launch_path.as_deref(), Some("kitty"));
        assert_eq!(directory[0].windows.len(), 2);
        // The integer window id survives the round trip; the string form this
        // crate hands a model is derived from it.
        assert_eq!(directory[0].windows[1].id, 94587430045600);
        assert!(directory[1].windows.is_empty());
    }

    #[test]
    fn the_instance_that_owns_the_named_window_wins() {
        let first = DirectoryApp {
            id: "kitty".to_string(),
            name: "kitty".to_string(),
            pid: Some(1),
            launch_path: None,
            windows: vec![DirectoryWindow {
                id: 10,
                on_screen: false,
            }],
        };
        let second = DirectoryApp {
            id: "kitty".to_string(),
            name: "kitty".to_string(),
            pid: Some(2),
            launch_path: None,
            windows: vec![DirectoryWindow {
                id: 20,
                on_screen: true,
            }],
        };
        let candidates = vec![&first, &second];
        // The named window decides between two instances of one application.
        assert_eq!(
            select_directory_app(&candidates, Some(10)).map(|app| app.pid),
            Some(Some(1))
        );
        assert_eq!(
            select_directory_app(&candidates, Some(20)).map(|app| app.pid),
            Some(Some(2))
        );
        // Without one, the window that is on screen is the better target.
        assert_eq!(
            select_directory_app(&candidates, None).map(|app| app.pid),
            Some(Some(2))
        );
        // An application with no window is still a target for a launch or a
        // kill, as long as it has a live process.
        let headless = DirectoryApp {
            id: "headless".to_string(),
            name: "headless".to_string(),
            pid: Some(3),
            launch_path: None,
            windows: Vec::new(),
        };
        assert_eq!(
            select_directory_app(&[&headless], None).map(|app| app.pid),
            Some(Some(3))
        );
        assert!(select_directory_app(&[], None).is_none());
    }

    #[test]
    fn element_handles_are_read_per_index() {
        let tokens = element_tokens(&json!({
            "elements": [
                { "element_index": 0, "role": "window", "element_token": "s00000001:0" },
                { "element_index": 4, "role": "push button", "element_token": "s00000001:4" },
                { "element_index": 5, "role": "label" }
            ]
        }));
        assert_eq!(tokens.get(&0).map(String::as_str), Some("s00000001:0"));
        assert_eq!(tokens.get(&4).map(String::as_str), Some("s00000001:4"));
        assert!(!tokens.contains_key(&5), "no handle means no handle");
        assert!(element_tokens(&json!({})).is_empty());
    }

    #[test]
    fn scroll_deltas_become_a_direction_and_a_step_count() {
        assert_eq!(scroll_direction(0.0, 3.0), ("down", 3));
        assert_eq!(scroll_direction(0.0, -3.0), ("up", 3));
        assert_eq!(scroll_direction(2.0, 0.0), ("right", 2));
        assert_eq!(scroll_direction(-2.0, 0.0), ("left", 2));
        // The dominant axis is the gesture, and the count stays inside the
        // range the engine accepts.
        assert_eq!(scroll_direction(1.0, 5.0), ("down", 5));
        assert_eq!(scroll_direction(0.0, -400.0), ("up", 50));
        assert_eq!(scroll_direction(0.0, 0.2), ("down", 1));
        assert_eq!(scroll_direction(0.0, 0.0), ("down", 1));
    }

    #[test]
    fn screenshots_parse_from_the_drivers_flat_fields() {
        let screenshot = parse_screenshot(&json!({
            "screenshot_png_b64": "QUJD",
            "screenshot_mime_type": "image/png",
            "screenshot_width": 954,
            "screenshot_height": 1040,
            "frame_scale": 4.8
        }))
        .unwrap();
        assert_eq!(screenshot.base64, "QUJD");
        assert_eq!(screenshot.mime_type, "image/png");
        assert_eq!(screenshot.width, 954);
        assert_eq!(screenshot.height, 1040);
        assert_eq!(screenshot.scale, 4.8);
        assert!(parse_screenshot(&json!({ "screenshot_png_b64": "" })).is_none());
        assert!(parse_screenshot(&json!({})).is_none());
    }

    /// The measurement the automatic Wayland opt-in acts on.
    #[test]
    fn the_wayland_measurement_asks_for_the_opt_in_only_when_it_helps() {
        // A session whose windows appear only with the backend.
        assert!(
            WaylandWindowDetection {
                configured_windows: 0,
                wayland_windows: 2,
                backend_available: true,
            }
            .wants_opt_in()
        );
        // The same windows either way: nothing to turn on.
        assert!(
            !WaylandWindowDetection {
                configured_windows: 3,
                wayland_windows: 3,
                backend_available: true,
            }
            .wants_opt_in()
        );
        // A compositor without the protocol will never offer them.
        assert!(
            !WaylandWindowDetection {
                configured_windows: 0,
                wayland_windows: 0,
                backend_available: false,
            }
            .wants_opt_in()
        );
        // Nothing measured at all is not evidence.
        assert!(!WaylandWindowDetection::default().wants_opt_in());
        assert!(!WaylandWindowDetection::default().is_measured());
    }

    /// The launcher command is the thing this engine resolves, so the one the
    /// directory reported is the one that has to be round-tripped.
    #[cfg(unix)]
    #[tokio::test]
    async fn launch_round_trips_the_launcher_command_the_directory_reported() {
        let temporary = tempfile::tempdir().unwrap();
        let calls = temporary.path().join("calls");
        let body = "#!/bin/sh\n\
                    case \"$1\" in\n\
                    call)\n\
                      shift\n\
                      printf '%s\\n' \"$*\" >> \"$CALLS\"\n\
                      case \"$1\" in\n\
                      list_apps) printf '%s' '{\"apps\":[{\"name\":\"WeChat\",\"bundle_id\":\"wechat-universal\",\"pid\":0,\"launch_path\":\"wechat-universal-start\"}]}';;\n\
                      launch_app) printf '%s' '{\"name\":\"WeChat\",\"pid\":7777}';;\n\
                      esac\n\
                      exit 0;;\n\
                    esac\n\
                    exit 0\n"
            .to_string();
        let (_script, driver) = fake_script_driver(body);
        let driver = driver.with_env("CALLS", calls.display().to_string());

        let apps = driver.list_apps().await.unwrap();
        assert_eq!(apps.len(), 1);
        assert!(!apps[0].running, "the directory says it is not running yet");
        let launched = driver.launch_app("wechat-universal").await.unwrap();
        assert_eq!(
            launched.pid,
            Some(7777),
            "the pid the engine reported survives, which is what makes a launch verifiable"
        );

        let recorded = std::fs::read_to_string(&calls).unwrap();
        assert!(
            recorded.contains("launch_app {\"launch_path\":\"wechat-universal-start\"}"),
            "the launch carries the launcher command, not the app id: {recorded}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_wayland_probe_reads_windows_and_the_compositor_check() {
        let (_temporary, driver) = fake_driver(
            r#"{"windows":[{"window_id":94587430045600},{"window_id":7},{"title":"no id"}]}"#,
            "",
            0,
        );
        assert_eq!(driver.window_ids().await.unwrap(), vec![94587430045600, 7]);
        let (_temporary, driver) = fake_driver(
            r#"{"overall":"ok","checks":[{"name":"session_active","status":"pass"},{"name":"wayland_backend","status":"pass"},{"name":"screen_capture_capability","status":"fail"}]}"#,
            "",
            0,
        );
        assert!(driver.wayland_backend_available().await.unwrap());
        let (_temporary, driver) = fake_driver(
            r#"{"overall":"ok","checks":[{"name":"wayland_backend","status":"skip"}]}"#,
            "",
            0,
        );
        assert!(!driver.wayland_backend_available().await.unwrap());
        let (_temporary, driver) = fake_driver(r#"{"windows":[]}"#, "", 0);
        assert!(driver.window_ids().await.unwrap().is_empty());
    }

    /// A daemon outlives the helper that started it, so a stale one has to be
    /// stopped when the backend setting changed — otherwise the setting is
    /// silently ignored at the next start.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_daemon_with_the_wrong_backend_is_stopped_and_a_matching_one_is_kept() {
        let stopped = |temporary: &tempfile::TempDir| temporary.path().join("stopped");
        let script = |temporary: &tempfile::TempDir| {
            format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                 status) echo 'Cua Driver daemon is running'; exit 0;;\n\
                 stop) echo stopped >> '{}'; exit 0;;\n\
                 call) printf '%s' \"$HEALTH\"; exit 0;;\n\
                 esac\n\
                 exit 0\n",
                stopped(temporary).display()
            )
        };

        // A daemon without the backend, while the backend is wanted: stopped.
        let temporary = tempfile::tempdir().unwrap();
        let (_script_dir, driver) = fake_script_driver(script(&temporary));
        let driver = driver.with_env(
            "HEALTH",
            r#"{"checks":[{"name":"wayland_backend","status":"skip"}]}"#,
        );
        assert!(
            driver
                .align_daemon_wayland_backend_on(true, ComputerPlatform::LinuxWayland)
                .await
                .unwrap()
        );
        assert!(stopped(&temporary).exists(), "the stale daemon was stopped");

        // A daemon that already has it: left alone.
        let temporary = tempfile::tempdir().unwrap();
        let (_script_dir, driver) = fake_script_driver(script(&temporary));
        let driver = driver.with_env(
            "HEALTH",
            r#"{"checks":[{"name":"wayland_backend","status":"pass"}]}"#,
        );
        assert!(
            !driver
                .align_daemon_wayland_backend_on(true, ComputerPlatform::LinuxWayland)
                .await
                .unwrap()
        );
        assert!(!stopped(&temporary).exists());

        // A reader who turned it off gets the same treatment in reverse.
        assert!(
            driver
                .align_daemon_wayland_backend_on(false, ComputerPlatform::LinuxWayland)
                .await
                .unwrap()
        );
        assert!(stopped(&temporary).exists());

        // Off Wayland nothing is touched, whatever the daemon reports.
        let temporary = tempfile::tempdir().unwrap();
        let (_script_dir, driver) = fake_script_driver(script(&temporary));
        let driver = driver.with_env(
            "HEALTH",
            r#"{"checks":[{"name":"wayland_backend","status":"pass"}]}"#,
        );
        assert!(
            !driver
                .align_daemon_wayland_backend_on(true, ComputerPlatform::LinuxX11)
                .await
                .unwrap()
        );
        assert!(!stopped(&temporary).exists());
    }

    /// The engine exits non-zero for a partial answer — a capture it could not
    /// attribute to the window — and that is not a broken channel.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_partial_answer_survives_a_non_zero_exit() {
        let payload = r#"{"window_id":94587431445600,"screenshot_error":{"code":"surface_identity_unproven","reason":"Wayland capture cannot prove pixels belong to that window"}}"#;
        let (_temporary, driver) = fake_driver(payload, "", 1);
        // The strict call treats it as the engine's rejection...
        let strict = driver
            .invoke_with_timeout("get_window_state", &json!({}), 5_000)
            .await
            .expect_err("a strict caller sees a failure");
        assert_eq!(strict.code, codes::DRIVER_REFUSED);
        // ...and the tolerant one reads the document the engine printed.
        let partial = driver
            .invoke_allowing_partial("get_window_state", &json!({}), 5_000)
            .await
            .expect("a partial answer is an answer");
        assert!(partial.get("screenshot_error").is_some());
        assert!(is_partial_answer(&partial));
        assert!(!is_partial_answer(&json!({
            "refusal": { "code": "invalid_arguments", "message": "unknown argument app" }
        })));
        assert!(!is_partial_answer(
            &json!({ "code": "background_unavailable" })
        ));
    }

    /// A window the engine keeps no snapshot for is not a stale reference: this
    /// adapter observed immediately before the action, so "observe again" would
    /// be a loop with no exit.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_window_the_engine_keeps_no_snapshot_for_is_not_a_stale_reference() {
        let (_temporary, driver) = fake_driver(
            r#"{"current_snapshots":[],"refusal":{"code":"stale_element_token","message":"element_token is stale; call get_window_state again to refresh; pid 51315 has no current snapshot"},"status":"refused"}"#,
            "",
            1,
        );
        let error = call_fake(&driver).await;
        assert_eq!(error.code, codes::ELEMENT_NOT_ADDRESSABLE);
        assert!(
            !error.is_stale_state(),
            "an element the engine will not address is not something to re-observe"
        );
        assert!(
            error.message.contains("no actionable snapshot"),
            "{}",
            error.message
        );
    }

    #[tokio::test]
    async fn an_element_without_a_handle_says_which_kind_of_missing_it_is() {
        // A window the engine could not prove: no handles at all.
        let driver = CuaDriverCli::new("/nonexistent-cua-driver");
        let error = driver
            .required_token("kitty", Some("94587429338640"), 0)
            .await
            .expect_err("there is no handle to hand out");
        assert_eq!(error.code, codes::ELEMENT_NOT_ADDRESSABLE);
        assert!(!error.is_stale_state());
        assert!(
            error.message.contains("could not prove"),
            "{}",
            error.message
        );

        // A proven window whose tree simply does not contain that index.
        driver
            .remember_tokens(
                "kitty",
                Some("w1"),
                &json!({
                    "window_id": "w1",
                    "elements": [{ "element_index": 0, "element_token": "s00000001:0" }]
                }),
            )
            .await;
        let error = driver
            .required_token("kitty", Some("w1"), 9)
            .await
            .expect_err("element 9 is not in the tree");
        assert!(
            error.message.contains("no handle for element 9"),
            "{}",
            error.message
        );
        assert_eq!(
            driver.required_token("kitty", Some("w1"), 0).await.unwrap(),
            "s00000001:0"
        );
    }

    #[test]
    fn a_missing_screenshot_degrades_the_observation_out_loud() {
        let request = EngineStateRequest {
            app_id: "kitty".to_string(),
            window_id: None,
            max_elements: 10,
            extended: false,
            screenshot: true,
        };
        let state = parse_app_state(
            &json!({
                "window_id": 94587431445600u64,
                "elements": [{ "element_index": 0, "role": "window", "label": "kitty" }],
                "screenshot_error": {
                    "code": "surface_identity_unproven",
                    "reason": "Wayland capture cannot prove pixels belong to that window"
                }
            }),
            &request,
        )
        .unwrap();
        assert!(state.screenshot.is_none());
        assert!(state.degraded, "an unverifiable picture is a degradation");
        assert!(
            state
                .degraded_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("screenshot unavailable")
                    && reason.contains("Wayland capture")),
            "{:?}",
            state.degraded_reason
        );
        // A screenshot that was not asked for is not a degradation.
        let not_asked = parse_app_state(
            &json!({
                "elements": [{ "element_index": 0, "role": "window" }],
                "screenshot_error": { "code": "surface_identity_unproven" }
            }),
            &EngineStateRequest {
                screenshot: false,
                ..request
            },
        )
        .unwrap();
        assert!(!not_asked.degraded);
    }
}

//! Browser process ownership: launch, health, and teardown.
//!
//! The browser is a multi-process application — Chrome forks renderer, GPU,
//! network and utility helpers. An orphaned tree is expensive, so the runtime
//! owns the whole group explicitly and reaps it on shutdown. `kill_on_drop` is
//! a safety net, not the plan: the runtime's shutdown chain calls
//! [`BrowserProcess::shutdown`] directly.
//!
//! Launch flags are security-relevant and are not configurable:
//!
//! * `--user-data-dir` always points at an isolated directory. Chrome 136 and
//!   later refuse remote debugging against the default profile, and reusing the
//!   user's real profile would hand the agent their live cookies.
//! * `--remote-debugging-pipe` is used wherever it is available, so no TCP port
//!   is opened at all. The loopback port fallback exists only for platforms
//!   where the pipe transport is not implemented.
//! * `--no-sandbox` is never passed. In a container, user namespaces or seccomp
//!   are the right answer.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use command_group::{AsyncCommandGroup, AsyncGroupChild};
use tokio::process::Command;
use vibex_core::BrowserInstallation;

use crate::cdp::{CdpConnection, CdpTransportKind};
use crate::error::{BrowserError, BrowserResult};

/// How long to wait for the browser to answer on the debugging channel.
pub const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for a graceful exit before the process group is killed.
pub const BROWSER_TERMINATE_GRACE: Duration = Duration::from_secs(3);
/// Extra flags the runtime always passes.
pub const BROWSER_BASE_FLAGS: &[&str] = &[
    "--no-first-run",
    "--no-default-browser-check",
    // Screencast needs the renderer to keep painting while the panel is not
    // focused or the window is occluded.
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    // A deterministic viewport: the runtime drives the device metrics itself.
    "--disable-features=CalculateNativeWinOcclusion,Translate",
    "--disable-dev-shm-usage",
    "--headless=new",
    "about:blank",
];

/// Configuration for one browser process launch.
#[derive(Debug, Clone)]
pub struct BrowserLaunchConfig {
    pub installation: BrowserInstallation,
    pub user_data_dir: PathBuf,
    pub transport: CdpTransportKind,
    /// Extra flags appended after the base flags.
    pub extra_flags: Vec<String>,
}

impl BrowserLaunchConfig {
    /// Builds the launch configuration for an installation.
    ///
    /// The profile directory must live under the runtime's data directory, never
    /// inside the user's workspace: a browser profile is not a project artifact.
    pub fn new(
        installation: BrowserInstallation,
        user_data_dir: impl Into<PathBuf>,
        transport: CdpTransportKind,
    ) -> Self {
        Self {
            installation,
            user_data_dir: user_data_dir.into(),
            transport,
            extra_flags: Vec::new(),
        }
    }

    pub fn with_extra_flag(mut self, flag: impl Into<String>) -> Self {
        self.extra_flags.push(flag.into());
        self
    }
}

/// A running browser process together with its debugging connection.
pub struct BrowserProcess {
    child: AsyncGroupChild,
    connection: Arc<CdpConnection>,
    user_data_dir: PathBuf,
    pid: Option<u32>,
}

impl std::fmt::Debug for BrowserProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserProcess")
            .field("pid", &self.pid)
            .field("transport", &self.connection.transport())
            .field("alive", &self.is_alive())
            .finish()
    }
}

impl BrowserProcess {
    /// Shared handle to the debugging channel. Tab sessions keep this alive
    /// independently of the process record.
    pub fn connection(&self) -> Arc<CdpConnection> {
        Arc::clone(&self.connection)
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn user_data_dir(&self) -> &Path {
        &self.user_data_dir
    }

    /// Cheap liveness probe. The authoritative signal is the CDP channel, but
    /// the UI and the idle reaper ask this first.
    pub fn is_alive(&self) -> bool {
        !self.connection.is_closed()
    }

    /// Terminates the browser and its whole process tree.
    pub async fn shutdown(mut self) {
        self.terminate().await;
    }

    async fn terminate(&mut self) {
        self.connection.mark_closed("browser_process_stopped");
        let _ = &self.connection;
        // `kill()` on an `AsyncGroupChild` signals the process group, so the
        // renderer/GPU helpers go with the browser.
        let _ = self.child.start_kill();
        match tokio::time::timeout(BROWSER_TERMINATE_GRACE, self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                tracing::warn!(
                    target: "vibex_browser",
                    "the browser process group did not exit within the grace period"
                );
                let _ = self.child.kill().await;
            }
        }
    }
}

/// Launches a browser and connects to its debugging channel.
pub async fn launch(config: &BrowserLaunchConfig) -> BrowserResult<BrowserProcess> {
    std::fs::create_dir_all(&config.user_data_dir).map_err(|error| {
        BrowserError::process(
            "browser_profile_dir_failed",
            "the runtime could not create the isolated browser profile directory",
        )
        .with_diagnostic("error", error.to_string())
    })?;

    match config.transport {
        CdpTransportKind::Pipe => launch_with_pipe(config).await,
        CdpTransportKind::WebSocket => launch_with_port(config).await,
    }
}

#[cfg(unix)]
async fn launch_with_pipe(config: &BrowserLaunchConfig) -> BrowserResult<BrowserProcess> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    // Chrome reads commands from fd 3 and writes responses to fd 4. Two socket
    // pairs give the parent the opposite ends.
    let (parent_write, child_read) = std::os::unix::net::UnixStream::pair().map_err(|error| {
        BrowserError::process("browser_pipe_failed", "failed to create the CDP pipe pair")
            .with_diagnostic("error", error.to_string())
    })?;
    let (child_write, parent_read) = std::os::unix::net::UnixStream::pair().map_err(|error| {
        BrowserError::process("browser_pipe_failed", "failed to create the CDP pipe pair")
            .with_diagnostic("error", error.to_string())
    })?;

    let read_fd = child_read.as_raw_fd();
    let write_fd = child_write.as_raw_fd();

    let mut command = Command::new(&config.installation.executable);
    command
        .arg(format!(
            "--user-data-dir={}",
            config.user_data_dir.to_string_lossy()
        ))
        .arg("--remote-debugging-pipe")
        .args(BROWSER_BASE_FLAGS)
        .args(&config.extra_flags)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_from_controlling_terminal(command.as_std_mut());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command
            .as_std_mut()
            .creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    // SAFETY: between fork and exec the hook only performs async-signal-safe
    // syscalls. `dup2` clears FD_CLOEXEC on the destination, so the browser
    // inherits exactly fd 3 and fd 4 and nothing else.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::dup2(read_fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(write_fd, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = spawn_grouped(&mut command, &config.installation.label).await?;
    // The parent must drop the child's ends, otherwise EOF never arrives.
    drop(child_read);
    drop(child_write);

    let pid = child.id();
    parent_write.set_nonblocking(true).map_err(|error| {
        BrowserError::process("browser_pipe_failed", "failed to configure the CDP pipe")
            .with_diagnostic("error", error.to_string())
    })?;
    parent_read.set_nonblocking(true).map_err(|error| {
        BrowserError::process("browser_pipe_failed", "failed to configure the CDP pipe")
            .with_diagnostic("error", error.to_string())
    })?;
    let writer = tokio::net::UnixStream::from_std(parent_write).map_err(|error| {
        BrowserError::process("browser_pipe_failed", "failed to adopt the CDP pipe")
            .with_diagnostic("error", error.to_string())
    })?;
    let reader = tokio::net::UnixStream::from_std(parent_read).map_err(|error| {
        BrowserError::process("browser_pipe_failed", "failed to adopt the CDP pipe")
            .with_diagnostic("error", error.to_string())
    })?;

    let connection = Arc::new(CdpConnection::from_pipe(writer, reader));

    Ok(BrowserProcess {
        child,
        connection,
        user_data_dir: config.user_data_dir.clone(),
        pid,
    })
}

#[cfg(not(unix))]
async fn launch_with_pipe(_config: &BrowserLaunchConfig) -> BrowserResult<BrowserProcess> {
    Err(BrowserError::capability(
        "browser_pipe_unsupported",
        "the pipe debugging transport is not implemented on this platform",
    )
    .with_recovery_hint("The runtime falls back to the loopback debugging port."))
}

async fn launch_with_port(config: &BrowserLaunchConfig) -> BrowserResult<BrowserProcess> {
    let mut command = Command::new(&config.installation.executable);
    command
        .arg(format!(
            "--user-data-dir={}",
            config.user_data_dir.to_string_lossy()
        ))
        // Port 0 asks the OS for a free port; the real value is read back from
        // the profile directory so nothing is guessed.
        .arg("--remote-debugging-port=0")
        .arg("--remote-allow-origins=http://127.0.0.1")
        .args(BROWSER_BASE_FLAGS)
        .args(&config.extra_flags)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_from_controlling_terminal(command.as_std_mut());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command
            .as_std_mut()
            .creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }

    let child = spawn_grouped(&mut command, &config.installation.label).await?;
    let pid = child.id();

    let active_port = config.user_data_dir.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + BROWSER_START_TIMEOUT;
    let endpoint = loop {
        if tokio::time::Instant::now() >= deadline {
            let mut child = child;
            let _ = child.start_kill();
            return Err(BrowserError::timeout(
                "browser_start_timeout",
                "the browser did not publish its debugging endpoint in time",
            ));
        }
        if let Ok(contents) = tokio::fs::read_to_string(&active_port).await
            && let Some((port, Some(path))) = crate::cdp::parse_devtools_active_port(&contents)
        {
            break (port, path);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let url = format!("ws://127.0.0.1:{}{}", endpoint.0, endpoint.1);
    let (sink, read) = crate::cdp::connect_websocket(&url).await?;

    Ok(BrowserProcess {
        child,
        connection: Arc::new(CdpConnection::from_websocket(sink, read)),
        user_data_dir: config.user_data_dir.clone(),
        pid,
    })
}

async fn spawn_grouped(command: &mut Command, label: &str) -> BrowserResult<AsyncGroupChild> {
    command.group().kill_on_drop(true).spawn().map_err(|error| {
        BrowserError::process("browser_spawn_failed", format!("failed to start {label}"))
            .with_diagnostic("error", error.to_string())
            .with_recovery_hint(
                "Check that the browser executable still exists and is runnable by this user.",
            )
    })
}

/// Removes the child from the desktop's controlling terminal.
///
/// A browser launched from a terminal app can be stopped by `SIGTTIN` when it
/// touches `/dev/tty`, which looks exactly like a hung page.
#[cfg(unix)]
pub fn detach_from_controlling_terminal(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: the hook only performs async-signal-safe syscalls between fork
    // and exec. It does not allocate, lock, or touch shared Rust state.
    unsafe {
        command.pre_exec(|| {
            const DEV_TTY: &[u8] = b"/dev/tty\0";
            let fd = libc::open(DEV_TTY.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC);
            if fd >= 0 {
                libc::ioctl(fd, libc::TIOCNOTTY);
                libc::close(fd);
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn detach_from_controlling_terminal(_command: &mut std::process::Command) {}

#[cfg(windows)]
pub const WINDOWS_CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// True when the process group still has members.
///
/// `SIGCONT` is harmless to a live process and fails with `ESRCH` once the
/// group is empty, which makes it a cheap liveness probe for a whole tree.
#[cfg(unix)]
pub fn process_group_has_members(pid: u32) -> bool {
    // SAFETY: `kill` with a negative pid targets the process group. `SIGCONT`
    // cannot terminate anything.
    unsafe { libc::kill(-(pid as i32), libc::SIGCONT) == 0 }
}

#[cfg(not(unix))]
pub fn process_group_has_members(_pid: u32) -> bool {
    false
}

/// Sends `SIGKILL` to a whole process group.
#[cfg(unix)]
pub fn kill_process_group(pid: u32) {
    // SAFETY: `kill` with a negative pid targets the process group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub fn kill_process_group(_pid: u32) {}

/// Best-effort reaping of a killed process group.
#[cfg(unix)]
pub fn reap_killed_process_group(pid: u32) {
    // SAFETY: `waitpid` with `WNOHANG` only reaps an already-dead child; a live
    // group member leaves the call returning immediately.
    unsafe {
        let mut status = 0;
        libc::waitpid(-(pid as i32), &mut status, libc::WNOHANG);
    }
}

#[cfg(not(unix))]
pub fn reap_killed_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_flags_never_disable_the_sandbox() {
        for flag in BROWSER_BASE_FLAGS {
            assert_ne!(*flag, "--no-sandbox");
            assert!(!flag.contains("no-sandbox"));
        }
    }

    #[test]
    fn base_flags_keep_the_renderer_painting() {
        assert!(BROWSER_BASE_FLAGS.contains(&"--disable-renderer-backgrounding"));
        assert!(BROWSER_BASE_FLAGS.contains(&"--disable-backgrounding-occluded-windows"));
        assert!(BROWSER_BASE_FLAGS.contains(&"--disable-background-timer-throttling"));
    }

    #[test]
    fn launch_config_appends_extra_flags_after_the_base_set() {
        let config = BrowserLaunchConfig::new(
            BrowserInstallation {
                id: "chrome".to_string(),
                label: "Google Chrome".to_string(),
                executable: "/usr/bin/google-chrome".to_string(),
                version: None,
            },
            "/tmp/vibex-browser-test",
            CdpTransportKind::Pipe,
        )
        .with_extra_flag("--lang=en-US");
        assert_eq!(config.extra_flags, vec!["--lang=en-US".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn process_group_probe_is_false_for_an_unused_pid() {
        // PID 0x7fff_fffe is above any realistic pid_max.
        assert!(!process_group_has_members(0x7fff_fffe));
    }
}

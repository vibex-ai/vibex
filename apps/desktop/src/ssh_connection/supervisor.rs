//! Runtime status, stop and port selection for an SSH-provisioned server.
//!
//! The server's own `status --json` and `stop` are the authority; this module
//! only runs them with the managed env file and validates the result. Port
//! selection is a preflight hint; the launcher still handles a race at start.

use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::command::SshCommand;
use super::launch::{LaunchConfig, LaunchError, LaunchState, run_script, start, write_env};

const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const START_DEADLINE: Duration = Duration::from_secs(10);
const STOP_DEADLINE: Duration = Duration::from_secs(10);
const STATUS_POLL: Duration = Duration::from_millis(100);
const MAX_PORT_ATTEMPTS: u16 = 10;
pub const STATUS_SCHEMA_VERSION: &str = "vibex-server-status.v1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub schema_version: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub port: u16,
    pub version: String,
    pub home: String,
    pub health: String,
}

impl ServerStatus {
    pub fn is_healthy(&self) -> bool {
        self.running && self.health == "ok"
    }

    /// Healthy *and* bound to the port this connection uses.
    ///
    /// A home owns exactly one runtime, but that runtime can outlive the
    /// profile that started it: the SSH probe skips a port it finds busy, so
    /// re-adding a host whose runtime is still up records the next port. The
    /// status of the old process is still healthy, yet the tunnel and the
    /// pairing claim address the new port, where nothing listens. Callers must
    /// therefore treat a healthy runtime on another port as "not started".
    pub fn is_healthy_on(&self, port: u16) -> bool {
        self.is_healthy() && self.port == port
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopOutcome {
    pub already_stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl SupervisorError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    fn retryable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: true,
        }
    }
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SupervisorError {}

impl From<LaunchError> for SupervisorError {
    fn from(error: LaunchError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        }
    }
}

pub fn status(
    command: &SshCommand,
    config: &LaunchConfig,
) -> Result<ServerStatus, SupervisorError> {
    let output = run_script(
        command,
        STATUS_SCRIPT,
        &[config.env_file.clone(), config.binary.clone()],
        STATUS_TIMEOUT,
    )?;
    parse_status(&output.stdout_lossy())
}

pub fn parse_status(stdout: &str) -> Result<ServerStatus, SupervisorError> {
    let status: ServerStatus = serde_json::from_str(stdout).map_err(|_| {
        SupervisorError::new(
            "ssh_runtime_status_invalid",
            "the remote runtime returned invalid status JSON",
        )
    })?;
    if status.schema_version != STATUS_SCHEMA_VERSION {
        return Err(SupervisorError::new(
            "ssh_runtime_status_schema_invalid",
            "the remote runtime returned an unsupported status schema",
        ));
    }
    Ok(status)
}

/// Runs the server's idempotent stop and confirms it is down.
pub fn stop(command: &SshCommand, config: &LaunchConfig) -> Result<StopOutcome, SupervisorError> {
    let output = run_script(
        command,
        STOP_SCRIPT,
        &[config.env_file.clone(), config.binary.clone()],
        STOP_TIMEOUT,
    )?;
    let stdout = output.stdout_lossy();
    let already_stopped = stdout.contains("already_stopped=true");
    let stopped = stdout.contains("stopped=true") || already_stopped;
    if !stopped {
        return Err(SupervisorError::new(
            "ssh_runtime_stop_unconfirmed",
            "the remote runtime did not confirm the stop",
        ));
    }
    let current = status(command, config)?;
    if current.running {
        return Err(SupervisorError::new(
            "ssh_runtime_still_running",
            "the remote runtime is still running after stop",
        ));
    }
    Ok(StopOutcome { already_stopped })
}

/// Picks the first port in `preferred..preferred+10` that `ss` reports free.
/// `unknown` is an error: the caller can still attempt a start and handle the
/// race through the launcher.
pub fn select_free_port(
    command: &SshCommand,
    preferred: u16,
    attempts: u16,
) -> Result<u16, SupervisorError> {
    let attempts = attempts.clamp(1, MAX_PORT_ATTEMPTS);
    for offset in 0..attempts {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        let output = run_script(command, PORT_SCRIPT, &[port.to_string()], STATUS_TIMEOUT)?;
        let stdout = output.stdout_lossy();
        if stdout.contains("free") {
            return Ok(port);
        }
        if stdout.contains("unknown") {
            return Err(SupervisorError::new(
                "ssh_runtime_port_unknown",
                "the remote host could not report port availability",
            ));
        }
    }
    Err(SupervisorError::new(
        "ssh_runtime_no_free_port",
        "no free remote runtime port was found",
    ))
}

/// Writes env, starts the runtime, and waits for `running=true, health=ok` on
/// `config.remote_port`.
///
/// A healthy runtime that is bound to a different port is not usable: the
/// tunnel, the pairing endpoint and the env file all follow `remote_port`. It
/// is stopped and restarted instead of being reported as already started,
/// because otherwise pairing dials a closed loopback port and fails as
/// `remote_http_unreachable`.
pub fn ensure_started(
    command: &SshCommand,
    config: &LaunchConfig,
) -> Result<ServerStatus, SupervisorError> {
    if let Ok(current) = status(command, config)
        && current.is_healthy_on(config.remote_port)
    {
        // Refresh the env so the next start (manual or after a reboot) uses
        // this profile's roots, but leave the running process alone.
        write_env(command, config)?;
        return Ok(current);
    }
    let mut outcome = start(command, config)?;
    if outcome.state == LaunchState::PortConflict {
        // The runtime that owns this home predates the profile and still holds
        // the old port. `start` reads its own state file, so the env rewrite
        // alone never moves it. Stop it through the same env file, then start
        // again on the port this connection uses.
        ensure_stopped(command, config)?;
        outcome = start(command, config)?;
        if outcome.state == LaunchState::PortConflict {
            return Err(SupervisorError::retryable(
                "ssh_runtime_port_conflict",
                "the remote runtime is still bound to another port after a restart",
            ));
        }
    }
    let deadline = Instant::now() + START_DEADLINE;
    loop {
        let current = status(command, config)?;
        if current.is_healthy_on(config.remote_port) {
            return Ok(current);
        }
        if !current.running
            && matches!(
                outcome.state,
                LaunchState::AlreadyRunning | LaunchState::PidAlive
            )
        {
            return Err(SupervisorError::new(
                "ssh_runtime_start_failed",
                "the remote runtime is not running",
            ));
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::retryable(
                "ssh_runtime_health_timeout",
                "the remote runtime did not become healthy in time",
            ));
        }
        thread::sleep(STATUS_POLL);
    }
}

/// Stops the runtime and waits until status reports it down.
pub fn ensure_stopped(
    command: &SshCommand,
    config: &LaunchConfig,
) -> Result<StopOutcome, SupervisorError> {
    let current = status(command, config)?;
    if !current.running {
        return Ok(StopOutcome {
            already_stopped: true,
        });
    }
    let outcome = stop(command, config)?;
    let deadline = Instant::now() + STOP_DEADLINE;
    loop {
        let current = status(command, config)?;
        if !current.running {
            return Ok(outcome);
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::retryable(
                "ssh_runtime_stop_timeout",
                "the remote runtime did not stop in time",
            ));
        }
        thread::sleep(STATUS_POLL);
    }
}

pub const STATUS_SCRIPT: &str = r#"set -eu
envfile=$1
binary=$2
set -a
. "$envfile"
set +a
"$binary" status --json
"#;

pub const STOP_SCRIPT: &str = r#"set -eu
envfile=$1
binary=$2
set -a
. "$envfile"
set +a
"$binary" stop
"#;

pub const PORT_SCRIPT: &str = r#"set -eu
port=$1
if command -v ss >/dev/null 2>&1; then
  if [ "$(ss -ltn "sport = :$port" 2>/dev/null | wc -l)" -gt 1 ]; then
    echo busy
  else
    echo free
  fi
else
  echo unknown
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::command::SshCommand;
    use crate::ssh_connection::launch::LaunchConfig;

    fn config() -> LaunchConfig {
        LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8765,
        )
        .unwrap()
    }

    #[test]
    fn parses_healthy_and_stopped_status() {
        let healthy = parse_status(
            r#"{"schemaVersion":"vibex-server-status.v1","running":true,"pid":42,"port":8765,"version":"0.1.0","home":"/home/dev","health":"ok"}"#,
        )
        .unwrap();
        assert!(healthy.is_healthy());
        assert_eq!(healthy.pid, Some(42));

        let stopped = parse_status(
            r#"{"schemaVersion":"vibex-server-status.v1","running":false,"pid":null,"port":8765,"version":"0.1.0","home":"/home/dev","health":"stopped"}"#,
        )
        .unwrap();
        assert!(!stopped.running);
    }

    #[test]
    fn rejects_invalid_status_schema_and_json() {
        assert_eq!(
            parse_status("{}").unwrap_err().code,
            "ssh_runtime_status_invalid"
        );
        let wrong_schema = r#"{"schemaVersion":"v1","running":false,"pid":null,"port":8765,"version":"0.1.0","home":"/home/dev","health":"stopped"}"#;
        assert_eq!(
            parse_status(wrong_schema).unwrap_err().code,
            "ssh_runtime_status_schema_invalid"
        );
    }

    #[cfg(unix)]
    fn fake_ssh(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let path = crate::ssh_connection::test_support::unique_path(dir, "fake-ssh");
        crate::ssh_connection::test_support::write_executable(
            &path,
            &format!("#!/bin/sh\n{body}\n"),
        );
        path
    }

    #[cfg(unix)]
    #[test]
    fn status_and_stop_use_the_server_cli() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(
            dir.path(),
            "script=$(cat)\n\
             case \"$script\" in\n\
               *'\"$binary\" stop'*) echo 'stopped=true';;\n\
               *'status --json'*) echo '{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":false,\"pid\":null,\"port\":8765,\"version\":\"0.1.0\",\"home\":\"/home/dev\",\"health\":\"stopped\"}';;\n\
               *) echo 'env=ok';;\n\
             esac",
        );
        let command = SshCommand::new(&ssh);
        let config = config();
        let current = status(&command, &config).unwrap();
        assert!(!current.running);
        let outcome = stop(&command, &config).unwrap();
        assert!(!outcome.already_stopped);
    }

    #[cfg(unix)]
    #[test]
    fn select_free_port_reports_busy_then_free() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("count");
        let ssh = fake_ssh(
            dir.path(),
            &format!(
                "n=$(cat {count} 2>/dev/null || echo 0)\n\
                 n=$((n+1))\n\
                 echo $n > {count}\n\
                 if [ $n -eq 1 ]; then echo busy; else echo free; fi",
                count = count.display()
            ),
        );
        let command = SshCommand::new(&ssh);
        assert_eq!(select_free_port(&command, 8765, 3).unwrap(), 8766);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_started_waits_for_healthy_status() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(
            dir.path(),
            "script=$(cat)\n\
             case \"$script\" in\n\
               *'env=ok'*) echo 'env=ok';;\n\
               *'launch=started'*) echo 'launch=started pid=7';;\n\
               *'status --json'*) echo '{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":true,\"pid\":7,\"port\":8765,\"version\":\"0.1.0\",\"home\":\"/home/dev\",\"health\":\"ok\"}';;\n\
               *) echo 'ok';;\n\
             esac",
        );
        let command = SshCommand::new(&ssh);
        let status = ensure_started(&command, &config()).unwrap();
        assert!(status.is_healthy());
        assert_eq!(status.pid, Some(7));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_stopped_reports_already_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(
            dir.path(),
            "cat >/dev/null\necho '{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":false,\"pid\":null,\"port\":8765,\"version\":\"0.1.0\",\"home\":\"/home/dev\",\"health\":\"stopped\"}'",
        );
        let command = SshCommand::new(&ssh);
        let outcome = ensure_stopped(&command, &config()).unwrap();
        assert!(outcome.already_stopped);
    }

    #[test]
    fn scripts_keep_the_managed_env_contract() {
        assert!(STATUS_SCRIPT.contains("status --json"));
        assert!(STOP_SCRIPT.contains(" stop"));
        assert!(PORT_SCRIPT.contains("ss -ltn"));
    }

    #[test]
    fn healthy_status_on_another_port_is_not_this_connection() {
        let status = parse_status(
            r#"{"schemaVersion":"vibex-server-status.v1","running":true,"pid":42,"port":8766,"version":"0.1.0","home":"/home/dev","health":"ok"}"#,
        )
        .unwrap();
        assert!(status.is_healthy());
        assert!(!status.is_healthy_on(8767));
        assert!(status.is_healthy_on(8766));
    }

    /// The production failure: a runtime left over from an earlier profile keeps
    /// owning the home and serves the old port, while the profile, the env file
    /// and the tunnel all moved to the next free port. Reporting that runtime as
    /// started makes pairing dial a closed loopback port and fail with
    /// `remote_http_unreachable`. `ensure_started` must stop it and restart on
    /// the requested port instead.
    #[cfg(unix)]
    #[test]
    fn ensure_started_restarts_a_runtime_bound_to_another_port() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let running = dir.path().join("running-port");
        std::fs::write(&running, "8766").unwrap();
        let ssh = fake_ssh(
            dir.path(),
            &format!(
                "script=$(cat)\n\
                 wanted=8767\n\
                 case \"$script\" in\n\
                   *launch=already_running*)\n\
                     current=$(cat {running} 2>/dev/null || true)\n\
                     if [ -n \"$current\" ] && [ \"$current\" = \"$wanted\" ]; then\n\
                       echo 'launch=already_running'\n\
                     elif [ -n \"$current\" ]; then\n\
                       echo \"launch=port_conflict port=$current wanted=$wanted\"\n\
                     else\n\
                       echo \"$wanted\" > {running}\n\
                       echo 'launch=started pid=7'\n\
                     fi ;;\n\
                   *'\"$binary\" stop'*)\n\
                     rm -f {running}\n\
                     echo 'stopped=true' >> {calls}\n\
                     echo 'stopped=true' ;;\n\
                   *env=ok*)\n\
                     echo 'env=ok' ;;\n\
                   *'status --json'*)\n\
                     current=$(cat {running} 2>/dev/null || true)\n\
                     if [ -n \"$current\" ]; then\n\
                       echo \"{{\\\"schemaVersion\\\":\\\"vibex-server-status.v1\\\",\\\"running\\\":true,\\\"pid\\\":7,\\\"port\\\":$current,\\\"version\\\":\\\"0.1.0\\\",\\\"home\\\":\\\"/home/dev\\\",\\\"health\\\":\\\"ok\\\"}}\"\n\
                     else\n\
                       echo '{{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":false,\"pid\":null,\"port\":8767,\"version\":\"0.1.0\",\"home\":\"/home/dev\",\"health\":\"stopped\"}}'\n\
                     fi ;;\n\
                   *) echo 'ok';;\n\
                 esac",
                running = running.display(),
                calls = calls.display(),
            ),
        );
        let command = SshCommand::new(&ssh);
        let launch = LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8767,
        )
        .unwrap();
        let status = ensure_started(&command, &launch).unwrap();
        assert!(status.is_healthy_on(8767));
        assert_eq!(status.pid, Some(7));
        // The stale runtime was stopped before the replacement started.
        let log = std::fs::read_to_string(&calls).unwrap();
        assert!(
            log.contains("stopped=true"),
            "the old runtime must be stopped: {log}"
        );
        assert_eq!(std::fs::read_to_string(&running).unwrap().trim(), "8767");
    }

    /// A healthy runtime already bound to the requested port is adopted as-is:
    /// connecting to a host twice must not restart it.
    #[cfg(unix)]
    #[test]
    fn ensure_started_keeps_a_runtime_on_the_requested_port() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let ssh = fake_ssh(
            dir.path(),
            &format!(
                "script=$(cat)\n\
                 case \"$script\" in\n\
                   *'\"$binary\" stop'*) echo 'stopped=true' >> {calls} ;;\n\
                   *'launch=started'*) echo 'launch=started pid=9' >> {calls} ;;\n\
                 esac\n\
                 case \"$script\" in\n\
                   *'env=ok'*) echo 'env=ok' ;;\n\
                   *'status --json'*) echo '{{\"schemaVersion\":\"vibex-server-status.v1\",\"running\":true,\"pid\":9,\"port\":8767,\"version\":\"0.1.0\",\"home\":\"/home/dev\",\"health\":\"ok\"}}' ;;\n\
                   *) echo 'ok' ;;\n\
                 esac",
                calls = calls.display(),
            ),
        );
        let command = SshCommand::new(&ssh);
        let launch = LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8767,
        )
        .unwrap();
        let status = ensure_started(&command, &launch).unwrap();
        assert_eq!(status.pid, Some(9));
        assert!(!calls.exists(), "a healthy runtime must not be restarted");
    }
}

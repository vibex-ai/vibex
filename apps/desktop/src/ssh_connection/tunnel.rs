//! SSH local-forward tunnel for a remote runtime.
//!
//! The tunnel is a child `ssh -N -L ...` process. Closing it never sends a stop
//! command to the remote runtime; only the SSH child is killed.

use std::ffi::OsString;
use std::fmt;
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::command::{SshCommand, SshDestination};

const MAX_LOCAL_PORT_ATTEMPTS: u16 = 20;
const MAX_BACKOFF_EXPONENT: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSpec {
    pub program: PathBuf,
    pub destination: SshDestination,
    pub config_path: Option<PathBuf>,
    pub local_port: u16,
    pub remote_port: u16,
}

impl TunnelSpec {
    pub fn new(
        program: impl Into<PathBuf>,
        destination: SshDestination,
        config_path: Option<PathBuf>,
        local_port: u16,
        remote_port: u16,
    ) -> Result<Self, TunnelError> {
        if local_port == 0 || remote_port == 0 {
            return Err(TunnelError::new(
                "ssh_tunnel_port_invalid",
                "tunnel ports must not be zero",
            ));
        }
        Ok(Self {
            program: program.into(),
            destination,
            config_path,
            local_port,
            remote_port,
        })
    }

    pub fn forward(&self) -> String {
        format!(
            "127.0.0.1:{}:127.0.0.1:{}",
            self.local_port, self.remote_port
        )
    }

    pub fn command(&self) -> Result<SshCommand, TunnelError> {
        let mut command = SshCommand::for_destination(
            &self.program,
            &self.destination,
            self.config_path.as_deref(),
        )
        .map_err(|error| TunnelError::new("ssh_tunnel_command_invalid", error.to_string()))?;
        let target_index = command
            .args
            .iter()
            .position(|argument| argument == "--")
            .ok_or_else(|| {
                TunnelError::new(
                    "ssh_tunnel_command_invalid",
                    "the SSH command has no destination separator",
                )
            })?;
        let options = vec![
            OsString::from("-N"),
            OsString::from("-o"),
            OsString::from("ExitOnForwardFailure=yes"),
            OsString::from("-L"),
            OsString::from(self.forward()),
        ];
        command.args.splice(target_index..target_index, options);
        Ok(command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl TunnelError {
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

impl fmt::Display for TunnelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TunnelError {}

#[derive(Debug)]
pub struct TunnelProcess {
    child: Child,
    local_port: u16,
}

impl TunnelProcess {
    pub fn spawn(spec: &TunnelSpec) -> Result<Self, TunnelError> {
        #[cfg(test)]
        let _process_guard = crate::ssh_connection::test_support::process_guard();
        let command = spec.command()?;
        let child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    TunnelError::new(
                        "ssh_tunnel_client_missing",
                        "the SSH client is not available",
                    )
                } else {
                    TunnelError::retryable(
                        "ssh_tunnel_spawn_failed",
                        format!("the SSH tunnel could not be started: {error}"),
                    )
                }
            })?;
        Ok(Self {
            child,
            local_port: spec.local_port,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, TunnelError> {
        self.child.try_wait().map_err(|error| {
            TunnelError::new(
                "ssh_tunnel_wait_failed",
                format!("the SSH tunnel status could not be read: {error}"),
            )
        })
    }

    /// Closes only the SSH child. The remote runtime is intentionally untouched.
    pub fn stop(&mut self) -> Result<(), TunnelError> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        self.child.kill().map_err(|error| {
            TunnelError::new(
                "ssh_tunnel_stop_failed",
                format!("the SSH tunnel could not be stopped: {error}"),
            )
        })?;
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for TunnelProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Clone)]
pub struct TunnelBackoff {
    attempts: u32,
    base: Duration,
    maximum: Duration,
}

impl TunnelBackoff {
    pub fn new(base: Duration, maximum: Duration) -> Result<Self, TunnelError> {
        if base.is_zero() || maximum < base {
            return Err(TunnelError::new(
                "ssh_tunnel_backoff_invalid",
                "the tunnel backoff bounds are invalid",
            ));
        }
        Ok(Self {
            attempts: 0,
            base,
            maximum,
        })
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn next_delay(&mut self) -> Duration {
        self.attempts = self.attempts.saturating_add(1);
        let exponent = (self.attempts - 1).min(MAX_BACKOFF_EXPONENT);
        self.base.saturating_mul(1u32 << exponent).min(self.maximum)
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    Started,
    Running,
    Exited { restart_in: Duration },
}

/// A non-blocking supervisor. Call `ensure_started` on a timer; it restarts a
/// child that exited after the reported backoff.
#[derive(Debug)]
pub struct TunnelSupervisor {
    spec: TunnelSpec,
    process: Option<TunnelProcess>,
    backoff: TunnelBackoff,
}

impl TunnelSupervisor {
    pub fn new(spec: TunnelSpec, backoff: TunnelBackoff) -> Self {
        Self {
            spec,
            process: None,
            backoff,
        }
    }

    pub fn ensure_started(&mut self) -> Result<TunnelState, TunnelError> {
        if let Some(process) = self.process.as_mut() {
            if process.try_wait()?.is_some() {
                self.process = None;
                return Ok(TunnelState::Exited {
                    restart_in: self.backoff.next_delay(),
                });
            }
            return Ok(TunnelState::Running);
        }
        self.process = Some(TunnelProcess::spawn(&self.spec)?);
        self.backoff.reset();
        Ok(TunnelState::Started)
    }

    pub fn stop(&mut self) -> Result<(), TunnelError> {
        if let Some(mut process) = self.process.take() {
            process.stop()?;
        }
        Ok(())
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process.as_ref().map(TunnelProcess::id)
    }
}

/// Picks a loopback port for the tunnel. A bound preferred port is skipped;
/// port 0 asks the OS for an ephemeral port.
pub fn select_local_port(preferred: u16, attempts: u16) -> Result<u16, TunnelError> {
    if preferred == 0 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| {
            TunnelError::new(
                "ssh_tunnel_local_port_unavailable",
                "no loopback port is available for the SSH tunnel",
            )
        })?;
        return listener
            .local_addr()
            .map(|address| address.port())
            .map_err(|_| {
                TunnelError::new(
                    "ssh_tunnel_local_port_unavailable",
                    "the loopback port could not be read",
                )
            });
    }
    let attempts = attempts.clamp(1, MAX_LOCAL_PORT_ATTEMPTS);
    for offset in 0..attempts {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return Ok(port);
        }
    }
    Err(TunnelError::new(
        "ssh_tunnel_local_port_unavailable",
        "no loopback port is available for the SSH tunnel",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination() -> SshDestination {
        SshDestination {
            host: "example.com".to_string(),
            user: Some("dev".to_string()),
            port: 22,
        }
    }

    fn spec(program: PathBuf) -> TunnelSpec {
        TunnelSpec::new(program, destination(), None, 18765, 8765).unwrap()
    }

    #[test]
    fn command_is_detached_forwarded_and_strict() {
        let spec = spec(PathBuf::from("ssh"));
        let command = spec.command().unwrap();
        let args = command
            .args
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.contains(&"-N".to_string()));
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert!(args.contains(&"StrictHostKeyChecking=yes".to_string()));
        assert!(args.contains(&"127.0.0.1:18765:127.0.0.1:8765".to_string()));
        let target = args.iter().position(|arg| arg == "--").unwrap();
        let forward = args
            .iter()
            .position(|arg| arg == "127.0.0.1:18765:127.0.0.1:8765")
            .unwrap();
        assert!(
            forward < target,
            "forward must be an SSH option, not a remote command"
        );
        assert!(!args.iter().any(|arg| arg == "stop"));
    }

    #[test]
    fn backoff_doubles_caps_and_resets() {
        let mut backoff =
            TunnelBackoff::new(Duration::from_secs(1), Duration::from_secs(30)).unwrap();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_secs(2));
        assert_eq!(backoff.next_delay(), Duration::from_secs(4));
        assert_eq!(backoff.next_delay(), Duration::from_secs(8));
        assert_eq!(backoff.next_delay(), Duration::from_secs(16));
        assert_eq!(backoff.next_delay(), Duration::from_secs(30));
        backoff.reset();
        assert_eq!(backoff.attempts(), 0);
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn rejects_invalid_backoff_and_ports() {
        assert!(TunnelBackoff::new(Duration::ZERO, Duration::from_secs(1)).is_err());
        assert!(TunnelBackoff::new(Duration::from_secs(2), Duration::from_secs(1)).is_err());
        assert!(TunnelSpec::new("ssh", destination(), None, 0, 8765).is_err());
        assert!(TunnelSpec::new("ssh", destination(), None, 18765, 0).is_err());
    }

    #[test]
    fn selects_a_free_loopback_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = listener.local_addr().unwrap().port();
        assert_eq!(
            select_local_port(taken, 1).unwrap_err().code,
            "ssh_tunnel_local_port_unavailable"
        );
        let free = select_local_port(taken, 20).unwrap();
        assert_ne!(free, taken);
        let ephemeral = select_local_port(0, 1).unwrap();
        assert_ne!(ephemeral, 0);
    }

    #[cfg(unix)]
    #[test]
    fn spawns_and_stops_only_the_ssh_child() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = crate::ssh_connection::test_support::unique_path(dir.path(), "fake-ssh");
        crate::ssh_connection::test_support::write_executable(&ssh, "#!/bin/sh\nexec sleep 30\n");
        let spec = spec(ssh);
        let mut process = TunnelProcess::spawn(&spec).unwrap();
        assert!(process.try_wait().unwrap().is_none());
        process.stop().unwrap();
        assert!(process.try_wait().unwrap().is_some());
        process.stop().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_reports_started_then_running() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = crate::ssh_connection::test_support::unique_path(dir.path(), "fake-ssh");
        crate::ssh_connection::test_support::write_executable(&ssh, "#!/bin/sh\nexec sleep 30\n");
        let mut supervisor = TunnelSupervisor::new(
            spec(ssh),
            TunnelBackoff::new(Duration::from_millis(10), Duration::from_millis(100)).unwrap(),
        );
        assert_eq!(supervisor.ensure_started().unwrap(), TunnelState::Started);
        assert_eq!(supervisor.ensure_started().unwrap(), TunnelState::Running);
        assert!(supervisor.process_id().is_some());
        supervisor.stop().unwrap();
        assert!(supervisor.process_id().is_none());
    }
}

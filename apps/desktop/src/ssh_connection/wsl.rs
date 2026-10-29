//! Windows Subsystem for Linux transport primitives.
//!
//! WSL2 is a local Linux environment, not a remote Windows host: there is no
//! sshd or SSH auth. The desktop invokes `wsl.exe -d <distro> --exec /bin/sh
//! -s`, sends fixed scripts on stdin, and reaches the runtime through WSL2
//! localhost forwarding.
//!
//! This module is deliberately transport-only. The SSH installer/launcher
//! scripts are reused by the provisioning layer once a Windows test host is
//! available; until then, these primitives are unit-tested with a fake
//! `wsl.exe` and a local TCP listener.

use std::fmt;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::environment::{EnvironmentError, RemoteEnvironment, parse_probe_output, probe_script};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_SCRIPT: &str = r#"set -eu; umask 077; mkdir -p "$(dirname "$1")"; cat > "$1""#;
const MAX_SCRIPT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl WslError {
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

impl fmt::Display for WslError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WslError {}

#[derive(Debug, Clone)]
pub struct WslCommand {
    program: PathBuf,
    distro: String,
    args: Vec<std::ffi::OsString>,
    stdin: Option<Vec<u8>>,
}

impl WslCommand {
    pub fn new(program: impl Into<PathBuf>, distro: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            distro: distro.into(),
            args: Vec::new(),
            stdin: None,
        }
    }

    pub fn distro(&self) -> &str {
        &self.distro
    }

    pub fn remote_script(&mut self, script: &str) -> Result<&mut Self, WslError> {
        if script.len() > MAX_SCRIPT_BYTES || script.contains('\0') {
            return Err(WslError::new(
                "wsl_script_invalid",
                "the WSL script is invalid or too large",
            ));
        }
        self.args.clear();
        self.args.push("-d".into());
        self.args.push(self.distro.clone().into());
        self.args.push("--exec".into());
        self.args.push("/bin/sh".into());
        self.args.push("-s".into());
        self.stdin = Some(script.as_bytes().to_vec());
        Ok(self)
    }

    pub fn arg(&mut self, value: impl Into<std::ffi::OsString>) -> &mut Self {
        self.args.push(value.into());
        self
    }

    /// Runs a fixed script with exact positional arguments.
    pub fn run_script(&mut self, script: &str, args: &[String]) -> Result<WslOutput, WslError> {
        self.remote_script(script)?;
        for arg in args {
            self.arg(arg.clone());
        }
        self.run()
    }

    /// Uploads bytes to a destination inside the distro without a shell
    /// reparse of the path.
    pub fn upload(&mut self, destination: &str, contents: &[u8]) -> Result<WslOutput, WslError> {
        self.args.clear();
        self.args.extend([
            "-d".into(),
            self.distro.clone().into(),
            "--exec".into(),
            "/bin/sh".into(),
            "-c".into(),
            UPLOAD_SCRIPT.into(),
            "sh".into(),
            destination.into(),
        ]);
        self.stdin = Some(contents.to_vec());
        self.run()
    }

    pub fn run(&self) -> Result<WslOutput, WslError> {
        self.run_with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn run_with_timeout(&self, timeout: Duration) -> Result<WslOutput, WslError> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.env("WSL_UTF8", "1");
        command.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                WslError::new("wsl_missing", "wsl.exe is not available")
            } else {
                WslError::new(
                    "wsl_spawn_failed",
                    format!("wsl.exe could not start: {error}"),
                )
            }
        })?;
        if let Some(input) = &self.stdin
            && let Some(mut stdin) = child.stdin.take()
            && stdin.write_all(input).is_err()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WslError::new(
                "wsl_stdin_failed",
                "the WSL stdin pipe closed early",
            ));
        }
        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(WslError::new(
                        "wsl_wait_failed",
                        format!("wsl.exe status could not be read: {error}"),
                    ));
                }
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut output) = child.stdout.take() {
            let _ = output.read_to_end(&mut stdout);
        }
        if let Some(mut error) = child.stderr.take() {
            let _ = error.read_to_end(&mut stderr);
        }
        let Some(status) = status else {
            return Err(WslError::retryable(
                "wsl_timeout",
                "the WSL command timed out",
            ));
        };
        if !status.success() {
            return Err(WslError::new(
                "wsl_command_failed",
                format!(
                    "wsl.exe exited with {}: {}",
                    status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&stderr).trim()
                ),
            ));
        }
        Ok(WslOutput {
            status,
            stdout,
            stderr,
            timed_out,
        })
    }
}

/// Holds a WSL2 distro open for the lifetime of a runtime. WSL terminates the
/// distro when its last Windows client exits; a detached server alone is not
/// enough on current WSL builds.
#[derive(Debug)]
pub struct WslKeepalive {
    child: std::process::Child,
}

impl WslKeepalive {
    /// Runs the server as the foreground process of a held `wsl.exe`. WSL
    /// terminates detached children when the client exits, so keeping the
    /// client alive is the reliable way to own the runtime.
    pub fn spawn_runtime(
        program: &Path,
        distro: &str,
        env_file: &str,
        binary: &str,
    ) -> Result<Self, WslError> {
        let child = Command::new(program)
            .args([
                "-d",
                distro,
                "--exec",
                "/bin/sh",
                "-c",
                "set -a; . \"$1\"; set +a; exec \"$2\" serve --no-pairing",
                "sh",
                env_file,
                binary,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    WslError::new("wsl_missing", "wsl.exe is not available")
                } else {
                    WslError::new(
                        "wsl_runtime_spawn_failed",
                        format!("the WSL runtime could not start: {error}"),
                    )
                }
            })?;
        Ok(Self { child })
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    pub fn spawn(program: &Path, distro: &str) -> Result<Self, WslError> {
        let child = Command::new(program)
            .args(["-d", distro, "--exec", "sleep", "infinity"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    WslError::new("wsl_missing", "wsl.exe is not available")
                } else {
                    WslError::new(
                        "wsl_keepalive_failed",
                        format!("the WSL keepalive could not start: {error}"),
                    )
                }
            })?;
        Ok(Self { child })
    }
}

impl Drop for WslKeepalive {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
pub struct WslOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl WslOutput {
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Lists installed distros. WSL's redirected output is UTF-16LE on Windows;
/// `WSL_UTF8=1` asks for UTF-8, and the NUL-stripping fallback handles older
/// builds.
pub fn list_distros(program: &Path) -> Result<Vec<String>, WslError> {
    let output = Command::new(program)
        .args(["-l", "-q"])
        .env("WSL_UTF8", "1")
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                WslError::new("wsl_missing", "wsl.exe is not available")
            } else {
                WslError::new(
                    "wsl_list_failed",
                    format!("wsl.exe could not list distros: {error}"),
                )
            }
        })?;
    if !output.status.success() {
        return Err(WslError::new(
            "wsl_list_failed",
            "wsl.exe could not list distros",
        ));
    }
    Ok(parse_distro_output(&output.stdout))
}

pub fn parse_distro_output(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.contains(&0) {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .filter_map(|unit| char::from_u32(u32::from(unit)))
            .collect::<String>()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|line| line.trim().trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Probes the Linux environment inside a distro using the same fixed script as
/// SSH Linux provisioning.
pub fn probe_environment(
    program: &Path,
    distro: &str,
    remote_port: u16,
) -> Result<RemoteEnvironment, WslError> {
    let mut command = WslCommand::new(program, distro);
    command
        .remote_script(&probe_script(remote_port))
        .map_err(|error| WslError::new("wsl_probe_script", error.message))?;
    let output = command.run()?;
    let environment: RemoteEnvironment = parse_probe_output(&output.stdout_lossy())
        .map_err(|error: EnvironmentError| WslError::new("wsl_probe_invalid", error.message()))?;
    environment
        .validate(super::environment::MIN_FREE_KB)
        .map_err(|error| WslError::new("wsl_probe_invalid", error.message()))?;
    Ok(environment)
}

/// WSL2 localhost forwarding: the runtime binds inside the distro, and Windows
/// exposes it on the same loopback port. A missing forwarding rule must be
/// reported, not silently treated as a dead runtime.
pub fn localhost_forwarded(port: u16) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&address, Duration::from_millis(750)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::test_support;

    /// A fake `wsl.exe` written and executed the way the rest of the SSH tests
    /// do it, so a parallel test binary cannot hit `ETXTBSY` on it.
    #[cfg(unix)]
    fn fake_wsl(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = test_support::unique_path(dir, "wsl");
        test_support::write_executable(&path, body);
        path
    }

    #[cfg(unix)]
    #[test]
    fn lists_distros_through_wsl_exe() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(dir.path(), "#!/bin/sh\nprintf 'Ubuntu-24.04\\nDebian\\n'\n");
        let _guard = test_support::process_guard();
        assert_eq!(list_distros(&wsl).unwrap(), ["Ubuntu-24.04", "Debian"]);
    }

    #[test]
    fn parses_utf16le_and_utf8_distro_lists() {
        let utf16: Vec<u8> = "Ubuntu-24.04\r\nDebian\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(parse_distro_output(&utf16), ["Ubuntu-24.04", "Debian"]);
        assert_eq!(
            parse_distro_output(b"Ubuntu-24.04\n\nDebian\n"),
            ["Ubuntu-24.04", "Debian"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn builds_exec_shell_arguments_and_sends_the_script_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("args");
        let wsl = fake_wsl(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\ncat >/dev/null\n",
                capture.display()
            ),
        );
        let _guard = test_support::process_guard();
        let mut command = WslCommand::new(&wsl, "Ubuntu-24.04");
        command.remote_script("set -eu\necho ok\n").unwrap();
        command.arg("one two").arg("three");
        command.run().unwrap();
        let args = std::fs::read_to_string(&capture).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "-d",
                "Ubuntu-24.04",
                "--exec",
                "/bin/sh",
                "-s",
                "one two",
                "three"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn probes_with_a_fake_wsl_and_parses_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(
            dir.path(),
            "#!/bin/sh\ncat >/dev/null\nprintf 'probe=ok\\nos=Linux\\narch=aarch64\\nhome=/home/dev\\nshell=/bin/bash\\ngit=yes\\nsha256sum=yes\\nfree_kb=1048576\\nglibc=2.36\\nhome_writable=yes\\nport=free\\nruntime=absent\\n'\n",
        );
        let _guard = test_support::process_guard();
        let environment = probe_environment(&wsl, "Ubuntu", 8765).unwrap();
        assert_eq!(environment.os, "Linux");
        assert_eq!(
            environment.arch,
            super::super::environment::RemoteArch::Aarch64
        );
    }

    #[cfg(windows)]
    #[test]
    fn real_wsl_transport_when_a_distro_is_installed() {
        let program = Path::new("wsl.exe");
        let Ok(distros) = list_distros(program) else {
            return;
        };
        let Some(distro) = distros.first() else {
            return;
        };
        let mut command = WslCommand::new(program, distro);
        let output = command.run_script("printf '%s' \"$HOME\"", &[]).unwrap();
        let home = output.stdout_lossy();
        assert!(home.starts_with('/'), "HOME={home:?}");
        let environment = probe_environment(program, distro, 8765).unwrap();
        assert_eq!(environment.os, "Linux");
        assert!(environment.home.starts_with('/'));
    }

    #[test]
    fn detects_localhost_forwarding_with_a_real_listener() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(localhost_forwarded(port));
        // A socket that is bound but never listens keeps the port reserved,
        // so a parallel test cannot start listening on it and the connect is
        // refused deterministically.
        let reserved = tokio::net::TcpSocket::new_v4().unwrap();
        reserved
            .bind(std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .unwrap();
        let reserved_port = reserved.local_addr().unwrap().port();
        assert!(!localhost_forwarded(reserved_port));
    }
}

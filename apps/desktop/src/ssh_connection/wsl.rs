//! Windows Subsystem for Linux transport primitives.
//!
//! WSL2 is a local Linux environment, not a remote Windows host: there is no
//! sshd or SSH auth. The desktop invokes `wsl.exe -d <distro> --cd ~ --exec
//! /bin/sh -s`, sends fixed scripts on stdin, and reaches the runtime through
//! WSL2 localhost forwarding. `--cd ~` keeps the guest out of the caller's
//! Windows working directory, which WSL cannot always translate.
//!
//! This module is deliberately transport-only. The SSH installer/launcher
//! scripts are reused by the provisioning layer once a Windows test host is
//! available; until then, these primitives are unit-tested with a fake
//! `wsl.exe` and a local TCP listener.

use std::fmt;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::console::hidden_command;
use super::environment::{EnvironmentError, RemoteEnvironment, parse_probe_output, probe_script};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// `wsl -l -q` only reads the registered distros, so it answers at once or not
/// at all. A half-installed WSL or a stopped `LxssManager` can leave `wsl.exe`
/// running for minutes, and the list is read while the add dialog is opening.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);
const UPLOAD_SCRIPT: &str = r#"set -eu; umask 077; mkdir -p "$(dirname "$1")"; cat > "$1""#;
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
/// How much of a child's output travels with an error. `wsl.exe` prints one
/// actionable sentence, and the point of keeping it is that the reader can
/// read it.
const MAX_DETAIL_CHARS: usize = 2048;

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
        self.args = self.enter_distro_args();
        self.args.push("--exec".into());
        self.args.push("/bin/sh".into());
        self.args.push("-s".into());
        self.stdin = Some(script.as_bytes().to_vec());
        Ok(self)
    }

    /// `-d <distro> --cd ~`: enter the distro in its own home directory.
    ///
    /// WSL translates the *Windows* working directory into the distro before
    /// it runs anything, and a directory it cannot map — a network drive, a
    /// UNC path, a drive excluded from automount — makes `wsl.exe` exit before
    /// the guest starts. `wsl -l -q` needs no translation, so the distro list
    /// keeps working while every `-d <distro> --exec` call fails, which reads
    /// as "the distro is broken" when only the caller's cwd is. `--cd ~`
    /// removes the dependency: the guest always starts in its own home.
    fn enter_distro_args(&self) -> Vec<std::ffi::OsString> {
        vec![
            "-d".into(),
            self.distro.clone().into(),
            "--cd".into(),
            "~".into(),
        ]
    }

    pub fn arg(&mut self, value: impl Into<std::ffi::OsString>) -> &mut Self {
        self.args.push(value.into());
        self
    }

    /// Runs a fixed script with exact positional arguments.
    pub fn run_script(&mut self, script: &str, args: &[String]) -> Result<WslOutput, WslError> {
        self.run_script_with_timeout(script, args, DEFAULT_TIMEOUT)
    }

    /// Runs a fixed script with an explicit deadline.
    pub fn run_script_with_timeout(
        &mut self,
        script: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<WslOutput, WslError> {
        self.remote_script(script)?;
        for arg in args {
            self.arg(arg.clone());
        }
        self.run_with_timeout(timeout)
    }

    /// Uploads bytes to a destination inside the distro without a shell
    /// reparse of the path.
    pub fn upload(&mut self, destination: &str, contents: &[u8]) -> Result<WslOutput, WslError> {
        self.upload_with_timeout(destination, contents, DEFAULT_TIMEOUT)
    }

    /// Uploads bytes with an explicit deadline (large server tarballs need
    /// more than the default 30s timeout).
    pub fn upload_with_timeout(
        &mut self,
        destination: &str,
        contents: &[u8],
        timeout: Duration,
    ) -> Result<WslOutput, WslError> {
        self.args = self.enter_distro_args();
        self.args.extend([
            "--exec".into(),
            "/bin/sh".into(),
            "-c".into(),
            UPLOAD_SCRIPT.into(),
            "sh".into(),
            destination.into(),
        ]);
        self.stdin = Some(contents.to_vec());
        self.run_with_timeout(timeout)
    }

    pub fn run(&self) -> Result<WslOutput, WslError> {
        self.run_with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn run_with_timeout(&self, timeout: Duration) -> Result<WslOutput, WslError> {
        self.run_inner(timeout, true)
    }

    /// One `wsl.exe` call. `explain` is false for the transport's own health
    /// check, whose failure must not start another health check.
    fn run_inner(&self, timeout: Duration, explain: bool) -> Result<WslOutput, WslError> {
        let mut command = hidden_command(&self.program);
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
        // Drain stdout/stderr and feed stdin on dedicated threads so large
        // uploads (>64 KiB pipe buffer) or verbose errors never deadlock, and
        // an early BrokenPipe still surfaces the child's exit diagnostics.
        let stdin_thread = self.stdin.as_ref().and_then(|input| {
            let mut stdin = child.stdin.take()?;
            let input = input.clone();
            Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            }))
        });
        let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
        if let Some(mut output) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = output.read_to_end(&mut buffer);
                let _ = stdout_tx.send(buffer);
            });
        } else {
            let _ = stdout_tx.send(Vec::new());
        }
        let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
        if let Some(mut error) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = error.read_to_end(&mut buffer);
                let _ = stderr_tx.send(buffer);
            });
        } else {
            let _ = stderr_tx.send(Vec::new());
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
        if timed_out {
            let stdout = stdout_rx
                .recv_timeout(Duration::from_millis(200))
                .unwrap_or_default();
            let stderr = stderr_rx
                .recv_timeout(Duration::from_millis(200))
                .unwrap_or_default();
            let detail = child_detail(&stdout, &stderr);
            let message = if detail.is_empty() {
                "the WSL command timed out".to_string()
            } else {
                format!("the WSL command timed out: {detail}")
            };
            return Err(WslError::retryable("wsl_timeout", message));
        }
        let stdout = stdout_rx.recv().unwrap_or_default();
        let stderr = stderr_rx.recv().unwrap_or_default();
        if let Some(handle) = stdin_thread {
            let _ = handle.join();
        }
        let Some(status) = status else {
            return Err(WslError::retryable(
                "wsl_timeout",
                "the WSL command timed out",
            ));
        };
        if !status.success() {
            let exit = status.code().unwrap_or(-1);
            return Err(if explain {
                self.explain_failure(exit, &stdout, &stderr)
            } else {
                WslError::new("wsl_command_failed", format!("wsl.exe exited with {exit}"))
            });
        }
        Ok(WslOutput {
            status,
            stdout,
            stderr,
            timed_out,
        })
    }

    /// Names the cause of a non-zero `wsl.exe` exit.
    ///
    /// The child's own words are reported first: `wsl.exe` writes its usage and
    /// distribution errors to **stdout** (UTF-16LE on builds that ignore
    /// `WSL_UTF8`), so reporting stderr alone produced "wsl.exe exited with 2:"
    /// with an empty tail — the reader got a status and no cause.
    ///
    /// A silent exit needs one more step, because its two causes want opposite
    /// next moves: the distro refuses to run anything (WSL's own registration
    /// or version problem) or the guest command failed without a word. One
    /// trivial command separates them; it never runs for a failure that already
    /// said something.
    fn explain_failure(&self, exit: i32, stdout: &[u8], stderr: &[u8]) -> WslError {
        let detail = child_detail(stdout, stderr);
        if !detail.is_empty() {
            return WslError::new(
                "wsl_command_failed",
                format!("wsl.exe exited with {exit}: {detail}"),
            );
        }
        let distro = &self.distro;
        match trivial_command_runs(&self.program, distro) {
            Some(true) => WslError::new(
                "wsl_command_failed",
                format!(
                    "wsl.exe exited with {exit} and the command printed nothing, while `/bin/true` ran in {distro}: the command itself failed inside the distro"
                ),
            ),
            Some(false) => WslError::new(
                "wsl_exec_refused",
                format!(
                    "WSL refused to run commands in {distro} (wsl.exe exited with {exit} and printed nothing, and `/bin/true` fails the same way); run `wsl --update` and `wsl -l -v`, then `wsl.exe -d {distro} -- true` in a terminal"
                ),
            ),
            None => WslError::new(
                "wsl_command_failed",
                format!("wsl.exe exited with {exit} and printed nothing on stdout or stderr"),
            ),
        }
    }
}

/// Whether `wsl.exe` can run a command in the distro at all.
///
/// `None` means the check itself could not answer (it failed to start or timed
/// out), which is not evidence either way.
fn trivial_command_runs(program: &Path, distro: &str) -> Option<bool> {
    let mut command = WslCommand::new(program, distro);
    command.args = command.enter_distro_args();
    command.args.extend(["--exec".into(), "/bin/true".into()]);
    match command.run_inner(DEFAULT_TIMEOUT, false) {
        Ok(_) => Some(true),
        Err(error) if error.code == "wsl_command_failed" => Some(false),
        Err(_) => None,
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
        log_file: &str,
    ) -> Result<Self, WslError> {
        let child = hidden_command(program)
            .args([
                "-d",
                distro,
                "--cd",
                "~",
                "--exec",
                "/bin/sh",
                "-c",
                "set -a; . \"$1\"; set +a; mkdir -p \"$(dirname \"$3\")\"; exec \"$2\" serve --no-pairing >> \"$3\" 2>&1",
                "sh",
                env_file,
                binary,
                log_file,
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
        let child = hidden_command(program)
            .args(["-d", distro, "--cd", "~", "--exec", "sleep", "infinity"])
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
    /// The guest's output. `wsl.exe` may hand back UTF-16LE when it ignores
    /// `WSL_UTF8`, and the probe and the status/pairing JSON go through the
    /// same decoder as the distro list so one build cannot break one reader.
    pub fn stdout_lossy(&self) -> String {
        decode_wsl_text(&self.stdout)
    }
}

/// Lists installed distros. WSL's redirected output is UTF-16LE on Windows;
/// `WSL_UTF8=1` asks for UTF-8, and the NUL-stripping fallback handles older
/// builds.
///
/// The two failures are kept apart on purpose: a build that cannot start
/// `wsl.exe` at all is an installation problem, while a `wsl.exe` that runs and
/// exits non-zero is a broken WSL registration. Both used to report
/// `wsl_list_failed` and neither carried the child's stderr, so the only thing
/// the reader got was "could not list distros" with no cause and no next step.
pub fn list_distros(program: &Path) -> Result<Vec<String>, WslError> {
    list_distros_with_timeout(program, LIST_TIMEOUT)
}

/// [`list_distros`] with the deadline as an argument, so a test does not have
/// to wait the production one out.
fn list_distros_with_timeout(program: &Path, timeout: Duration) -> Result<Vec<String>, WslError> {
    let mut command = hidden_command(program);
    command
        .args(["-l", "-q"])
        .env("WSL_UTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            WslError::new("wsl_missing", "wsl.exe is not available")
        } else {
            WslError::new(
                "wsl_list_spawn_failed",
                format!("wsl.exe could not start: {error}"),
            )
        }
    })?;
    // Both pipes are drained on their own threads while the child runs: a
    // child that fills one of them before exiting would otherwise wait for a
    // reader that this loop cannot reach.
    let stdout_reader = child.stdout.take().map(|output| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::BufReader::new(output).read_to_end(&mut bytes);
            bytes
        })
    });
    let stderr_reader = child.stderr.take().map(|output| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::BufReader::new(output).read_to_end(&mut bytes);
            bytes
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(WslError::new(
                    "wsl_list_failed",
                    format!("wsl.exe status could not be read: {error}"),
                ));
            }
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WslError::retryable(
                "wsl_list_timeout",
                format!(
                    "wsl.exe did not list distros within {} seconds; run `wsl --status` in a terminal",
                    timeout.as_secs()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let Some(status) = status else {
        return Err(WslError::retryable(
            "wsl_list_timeout",
            "wsl.exe did not list distros",
        ));
    };
    if !status.success() {
        let detail = child_detail(&stdout, &stderr);
        let exit = status.code().unwrap_or(-1);
        let message = if detail.is_empty() {
            format!("wsl.exe could not list distros (exit {exit})")
        } else {
            format!("wsl.exe could not list distros (exit {exit}): {detail}")
        };
        return Err(WslError::new("wsl_exec_failed", message));
    }
    Ok(parse_distro_output(&stdout))
}

/// The child's own words from both streams, on one line, bounded the way
/// `SshFailure` bounds its stderr tail.
///
/// stdout comes first because that is where `wsl.exe` prints its own messages
/// and where every guest command in this module writes its result. WSL writes
/// UTF-16LE on older builds, so a NUL-bearing stream goes through the same
/// decoder the distro list uses.
fn child_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut joined = String::new();
    for bytes in [stdout, stderr] {
        for line in decode_wsl_text(bytes)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if !joined.is_empty() {
                joined.push_str("; ");
            }
            joined.push_str(line);
        }
    }
    if joined.chars().count() <= MAX_DETAIL_CHARS {
        return joined;
    }
    joined.chars().take(MAX_DETAIL_CHARS).collect()
}

pub fn parse_distro_output(bytes: &[u8]) -> Vec<String> {
    decode_wsl_text(bytes)
        .lines()
        .map(|line| line.trim().trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Decodes text that `wsl.exe` wrote to a redirected pipe.
///
/// `WSL_UTF8=1` asks current builds for UTF-8, but the variable is a request
/// rather than a guarantee, and builds that ignore it emit UTF-16LE — whose
/// every character after the first has a NUL byte. A NUL-bearing stream is
/// therefore decoded as UTF-16LE; anything else is read as UTF-8, lossily,
/// because a decode failure must not turn into an empty list.
fn decode_wsl_text(bytes: &[u8]) -> String {
    let decoded = if bytes.contains(&0) {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    decoded.trim_start_matches('\u{feff}').to_string()
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
        .map_err(|error| match error {
            // The port is the one probe failure the reader can fix in the add
            // dialog. Name the port they picked instead of repeating the
            // generic "port is in use" sentence, and keep a dedicated code so
            // the UI can offer the input rather than a transport diagnostic.
            EnvironmentError::PortBusy => WslError::retryable(
                "wsl_port_busy",
                format!(
                    "the WSL runtime port {remote_port} is already in use; choose another port"
                ),
            ),
            error => WslError::new("wsl_probe_invalid", error.message()),
        })?;
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

    /// A `wsl.exe` that fails has to say why.
    ///
    /// Every failed `wsl -l -q` used to collapse into the same sentence with the
    /// child's stderr discarded, so "could not list distros" was all a reader
    /// ever saw — no cause, no next step.
    #[cfg(unix)]
    #[test]
    fn a_failed_list_keeps_the_childs_own_words() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(
            dir.path(),
            "#!/bin/sh\nprintf 'wsl: no installed distributions. Run wsl --install.\\n' >&2\nexit 1\n",
        );
        let _guard = test_support::process_guard();
        let error = list_distros(&wsl).unwrap_err();
        assert_eq!(error.code, "wsl_exec_failed");
        assert!(
            error.message.contains("no installed distributions"),
            "the child's stderr must survive: {}",
            error.message
        );
        assert!(
            error.message.contains("exit 1"),
            "the exit status belongs in the message: {}",
            error.message
        );
        assert!(!error.retryable, "a broken WSL registration is not a flake");
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_wsl_exe_stays_a_installation_problem() {
        let dir = tempfile::tempdir().unwrap();
        let error = list_distros(&dir.path().join("no-such-wsl")).unwrap_err();
        assert_eq!(error.code, "wsl_missing");
    }

    /// WSL that never answers must not hold the caller forever.
    ///
    /// `list_distros` used to use `Command::output`, which has no deadline, and
    /// the add dialog read it on the UI thread; a half-installed WSL froze the
    /// whole window with nothing on screen to explain why.
    #[cfg(unix)]
    #[test]
    fn a_hung_wsl_is_cut_off_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(dir.path(), "#!/bin/sh\nsleep 30\n");
        let _guard = test_support::process_guard();
        let error = list_distros_with_timeout(&wsl, Duration::from_millis(250)).unwrap_err();
        assert_eq!(error.code, "wsl_list_timeout");
        assert!(error.retryable, "a timeout is worth another try");
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
                // The guest never starts in the caller's Windows directory.
                "--cd",
                "~",
                "--exec",
                "/bin/sh",
                "-s",
                "one two",
                "three"
            ]
        );
    }

    /// The upload path and the runtime holder enter the distro the same way.
    #[cfg(unix)]
    #[test]
    fn upload_and_keepalive_enter_the_distro_home() {
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
        let mut command = WslCommand::new(&wsl, "Ubuntu");
        command
            .upload("/home/dev/.incoming.tar.gz", b"bytes")
            .unwrap();
        let args = std::fs::read_to_string(&capture).unwrap();
        assert_eq!(
            args.lines().take(4).collect::<Vec<_>>(),
            ["-d", "Ubuntu", "--cd", "~"]
        );
        assert!(
            args.contains("-c"),
            "the upload still runs the fixed stdin script: {args}"
        );

        // The holder is the process that keeps the runtime alive, so it must
        // start in the same directory or the whole deployment fails where the
        // upload succeeded.
        let keepalive_capture = dir.path().join("keepalive-args");
        let keepalive = fake_wsl(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nexec sleep 30\n",
                keepalive_capture.display()
            ),
        );
        let _held = WslKeepalive::spawn(&keepalive, "Ubuntu").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut keepalive_args = String::new();
        while Instant::now() < deadline {
            if let Ok(read) = std::fs::read_to_string(&keepalive_capture) {
                keepalive_args = read;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            keepalive_args.lines().take(4).collect::<Vec<_>>(),
            ["-d", "Ubuntu", "--cd", "~"]
        );
    }

    /// A failing command has to carry what it said.
    ///
    /// `wsl.exe` prints its usage and distribution errors on **stdout**, so a
    /// failure that reported only stderr reached the dialog as "wsl.exe exited
    /// with 2:" with an empty tail — the reported shape of the WSL issue.
    #[cfg(unix)]
    #[test]
    fn a_failed_command_keeps_the_childs_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(
            dir.path(),
            "#!/bin/sh\ncat >/dev/null\nprintf 'Invalid command line option: --exec\\n'\nexit 2\n",
        );
        let _guard = test_support::process_guard();
        let mut command = WslCommand::new(&wsl, "Ubuntu");
        command.remote_script("echo ok\n").unwrap();
        let error = command.run().unwrap_err();
        assert_eq!(error.code, "wsl_command_failed");
        assert!(
            error.message.contains("Invalid command line option"),
            "the child's stdout must survive: {}",
            error.message
        );
        assert!(
            error.message.contains("exited with 2"),
            "the exit status belongs in the message: {}",
            error.message
        );
    }

    /// The same failure written the way older WSL builds write it.
    #[cfg(unix)]
    #[test]
    fn a_utf16_failure_message_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = "There is no distribution with the supplied name.\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        std::fs::write(dir.path().join("out.bin"), bytes).unwrap();
        let wsl = fake_wsl(
            dir.path(),
            &format!(
                "#!/bin/sh\ncat >/dev/null\ncat '{}'\nexit 1\n",
                dir.path().join("out.bin").display()
            ),
        );
        let _guard = test_support::process_guard();
        let mut command = WslCommand::new(&wsl, "Ubuntu");
        command.remote_script("echo ok\n").unwrap();
        let error = command.run().unwrap_err();
        assert!(
            error
                .message
                .contains("no distribution with the supplied name"),
            "a UTF-16LE message must decode: {}",
            error.message
        );
    }

    /// A silent exit has two causes and they want opposite next moves, so one
    /// trivial command settles it instead of one sentence for both.
    #[cfg(unix)]
    #[test]
    fn a_silent_failure_separates_the_command_from_the_distro() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(
            dir.path(),
            "#!/bin/sh\ncase \"$*\" in\n  *\"/bin/true\"*) exit 0 ;;\nesac\ncat >/dev/null\nexit 2\n",
        );
        let _guard = test_support::process_guard();
        let mut command = WslCommand::new(&wsl, "Ubuntu");
        command.remote_script("echo ok\n").unwrap();
        let error = command.run().unwrap_err();
        assert_eq!(error.code, "wsl_command_failed");
        assert!(
            error.message.contains("/bin/true") && error.message.contains("Ubuntu"),
            "the message says what still worked and where: {}",
            error.message
        );

        // A distro that refuses everything is its own failure: `wsl --update`
        // and `wsl -l -v` are the next steps, not "the script is broken".
        let refused = fake_wsl(dir.path(), "#!/bin/sh\ncat >/dev/null\nexit 2\n");
        let mut command = WslCommand::new(&refused, "Ubuntu");
        command.remote_script("echo ok\n").unwrap();
        let error = command.run().unwrap_err();
        assert_eq!(error.code, "wsl_exec_refused");
        assert!(error.message.contains("wsl --update"), "{}", error.message);
        assert!(error.message.contains("wsl -l -v"), "{}", error.message);
        assert!(!error.retryable, "a refusing distro is not a flake");
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

    /// A busy port is a different failure from a broken distro.
    ///
    /// The add dialog offers the port field only when it can see this code, and
    /// the message has to name the port the reader chose; the generic probe
    /// sentence left them guessing which of two occupied ports to change.
    #[cfg(unix)]
    #[test]
    fn a_busy_runtime_port_names_the_port() {
        let dir = tempfile::tempdir().unwrap();
        let wsl = fake_wsl(
            dir.path(),
            "#!/bin/sh\ncat >/dev/null\nprintf 'probe=ok\\nos=Linux\\narch=x86_64\\nhome=/home/dev\\nshell=/bin/bash\\ngit=yes\\nsha256sum=yes\\nfree_kb=1048576\\nglibc=2.36\\nhome_writable=yes\\nport=busy\\nruntime=absent\\n'\n",
        );
        let _guard = test_support::process_guard();
        let error = probe_environment(&wsl, "Ubuntu", 18765).unwrap_err();
        assert_eq!(error.code, "wsl_port_busy");
        assert!(
            error.message.contains("18765"),
            "the message names the chosen port: {}",
            error.message
        );
        assert!(error.retryable);
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
